use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::PathBuf,
};

use crate::{
    Level,
    header::{
        DEFAULT_HEADER_PATTERN, DefaultHeader, LEVEL_THREAD_LOCATION_HEADER_PATTERN,
        LevelThreadLocationHeader, StaticHeader, StaticHeaderWriteFn, TokenizedHeader,
    },
};

/// Configuration for automatic log file rotation.
pub struct RotationConfig {
    /// Maximum file size in bytes before rotating. Must be > 0.
    pub max_size: u64,
    /// Maximum number of rotated files to keep. Oldest files beyond this count are deleted.
    pub max_files: u32,
}

impl RotationConfig {
    /// Create a new rotation config with defaults (100MB max, 5 files).
    pub fn new() -> Self {
        Self { max_size: 100 * 1024 * 1024, max_files: 5 }
    }

    /// Create a rotation config: rotate when file exceeds `max_size` bytes,
    /// keeping at most `max_files` old rotated files.
    pub fn by_size(max_size: u64, max_files: u32) -> Self {
        Self { max_size, max_files }
    }

    /// Set the maximum file size before rotation.
    pub fn max_size(mut self, bytes: u64) -> Self {
        self.max_size = bytes;
        self
    }

    /// Set the maximum number of rotated files to keep.
    pub fn max_files(mut self, count: u32) -> Self {
        self.max_files = count;
        self
    }

    /// Set the maximum file size in megabytes before rotation.
    pub fn max_size_mb(mut self, megabytes: u64) -> Self {
        self.max_size = megabytes * 1024 * 1024;
        self
    }
}

impl Default for RotationConfig {
    fn default() -> Self {
        Self::new()
    }
}

struct FileRotation {
    base_path: PathBuf,
    config: RotationConfig,
    current_size: u64,
}

enum HeaderMode {
    Empty,
    Default(DefaultHeader),
    LevelThreadLocation(LevelThreadLocationHeader),
    Static(StaticHeader),
    Dynamic(TokenizedHeader),
}

impl HeaderMode {
    #[inline]
    fn from_pattern(pattern: &str) -> io::Result<Self> {
        if pattern.is_empty() {
            Ok(Self::Empty)
        } else if pattern == DEFAULT_HEADER_PATTERN {
            Ok(Self::Default(DefaultHeader::new()))
        } else if pattern == LEVEL_THREAD_LOCATION_HEADER_PATTERN {
            Ok(Self::LevelThreadLocation(LevelThreadLocationHeader::new()))
        } else {
            TokenizedHeader::parse_pattern(pattern).map(Self::Dynamic)
        }
    }

    #[inline]
    fn write_header(
        &mut self,
        output: &mut Vec<u8>,
        nanoseconds: i64,
        level: Level,
        thread_name: &[u8],
        location: &str,
        span_id: u64,
    ) {
        match self {
            Self::Empty => {}
            Self::Default(header) => {
                header.write_header(output, nanoseconds, level, thread_name, location, span_id);
            }
            Self::LevelThreadLocation(header) => {
                header.write_header(output, nanoseconds, level, thread_name, location, span_id);
            }
            Self::Static(header) => {
                header.write_header(output, nanoseconds, level, thread_name, location, span_id);
            }
            Self::Dynamic(header) => {
                header.write_header(output, nanoseconds, level, thread_name, location, span_id);
            }
        }
    }
}

/// Buffered log output that formats records through a [`TokenizedHeader`] and
/// writes them to a configurable [`Write`] destination.
pub struct OutputBuffer {
    pending: Vec<u8>,
    writer: Box<dyn Write + Send>,
    header: HeaderMode,
    rotation: Option<FileRotation>,
}

impl Default for OutputBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl OutputBuffer {
    /// Creates a new `OutputBuffer` that writes to stderr with the default header pattern.
    pub fn new() -> Self {
        Self {
            pending: Vec::with_capacity(8 * 1024),
            writer: Box::new(io::stderr()),
            header: HeaderMode::Default(DefaultHeader::new()),
            rotation: None,
        }
    }

    /// Replaces the underlying writer with the given one.
    pub fn set_writer(&mut self, writer: Box<dyn Write + Send>) {
        self.writer = writer;
    }

    /// Opens (or creates) a log file at `path` in append mode and redirects output to it.
    /// Any previously configured rotation is cleared.
    pub fn set_log_file(&mut self, path: &str) -> io::Result<()> {
        self.flush()?;
        let file = open_log_file(path)?;
        self.writer = Box::new(BufWriter::new(file));
        self.rotation = None;
        Ok(())
    }

    /// Opens (or creates) a log file at `path` with automatic size-based rotation.
    /// When the file exceeds `config.max_size` bytes, it is rotated according to
    /// the rotation configuration.
    pub fn set_log_file_with_rotation(
        &mut self,
        path: &str,
        config: RotationConfig,
    ) -> io::Result<()> {
        self.flush()?;
        let file = open_log_file(path)?;
        let current_size = file.metadata()?.len();
        self.writer = Box::new(BufWriter::new(file));
        self.rotation = Some(FileRotation { base_path: PathBuf::from(path), config, current_size });
        Ok(())
    }

    /// Sets a custom header pattern for log record formatting.
    pub fn set_header_pattern(&mut self, pattern: &str) -> io::Result<()> {
        self.header = HeaderMode::from_pattern(pattern)?;
        Ok(())
    }

    pub fn set_static_header(&mut self, write: StaticHeaderWriteFn) {
        self.header = HeaderMode::Static(StaticHeader::new(write));
    }

    /// Formats and buffers a single log record directly into the pending output buffer.
    pub fn write_record<F>(
        &mut self,
        timestamp_nanoseconds: i64,
        level: Level,
        thread_name: &[u8],
        location: &str,
        span_id: u64,
        append_body: F,
    ) -> io::Result<()>
    where
        F: FnOnce(&mut Vec<u8>),
    {
        self.header.write_header(
            &mut self.pending,
            timestamp_nanoseconds,
            level,
            thread_name,
            location,
            span_id,
        );
        append_body(&mut self.pending);
        self.pending.push(b'\n');
        Ok(())
    }

    /// Returns the number of bytes currently buffered and awaiting flush.
    #[inline]
    pub fn buffered_len(&self) -> usize {
        self.pending.len()
    }

    /// Returns `true` if there are buffered bytes that have not been flushed yet.
    #[inline]
    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Flushes all pending bytes to the underlying writer.
    /// If rotation is configured and the file size exceeds the threshold,
    /// the log file is rotated after flushing.
    pub fn flush(&mut self) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let written = self.pending.len() as u64;
        self.writer.write_all(&self.pending)?;
        self.writer.flush()?;
        self.pending.clear();

        let needs_rotation = if let Some(ref mut rotation) = self.rotation {
            rotation.current_size += written;
            rotation.current_size >= rotation.config.max_size
        } else {
            false
        };

        if needs_rotation {
            self.rotate()?;
        }

        Ok(())
    }

    /// Rotates the current log file by renaming it through a numbered
    /// suffix chain (base -> base.1 -> base.2 -> ...) and opening a
    /// fresh file at the base path.
    fn rotate(&mut self) -> io::Result<()> {
        let rotation = self.rotation.as_mut().expect("rotate called without rotation config");
        let base_path = &rotation.base_path;
        let max_files = rotation.config.max_files;

        // Drop the current writer so the file handle is released.
        self.writer = Box::new(io::sink());

        // Delete the oldest file if it exceeds max_files.
        let oldest = format!("{}.{}", base_path.display(), max_files);
        let _ = fs::remove_file(&oldest);

        // Rename base.N-1 -> base.N, ..., base.1 -> base.2
        for i in (1..max_files).rev() {
            let from = format!("{}.{}", base_path.display(), i);
            let to = format!("{}.{}", base_path.display(), i + 1);
            let _ = fs::rename(&from, &to);
        }

        // Rename base -> base.1
        let first_rotated = format!("{}.1", base_path.display());
        let _ = fs::rename(base_path, &first_rotated);

        // Open a new file at the base path
        let file = open_log_file(base_path.to_str().unwrap_or(""))?;
        self.writer = Box::new(BufWriter::new(file));

        rotation.current_size = 0;
        Ok(())
    }
}

fn open_log_file(path: &str) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

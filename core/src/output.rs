use std::{
    fs::{self, File, OpenOptions},
    io::{self, BufWriter, Write},
    path::{Path, PathBuf},
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
    reopen_pending: bool,
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

/// Maximum retained formatted output before draining applies backpressure.
pub const MAX_PENDING_BYTES: usize = 16 * 1024 * 1024;

pub(crate) struct PreparedOutput {
    header: Option<HeaderMode>,
    destination: Option<PreparedDestination>,
}

struct PreparedDestination {
    writer: Box<dyn Write + Send>,
    rotation: Option<FileRotation>,
}

/// Buffered log output that formats records through a [`TokenizedHeader`] and
/// writes them to a configurable [`Write`] destination.
pub struct OutputBuffer {
    pending: Vec<u8>,
    write_offset: usize,
    writer_needs_flush: bool,
    flush_required: bool,
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
            write_offset: 0,
            writer_needs_flush: false,
            flush_required: false,
            writer: Box::new(io::stderr()),
            header: HeaderMode::Default(DefaultHeader::new()),
            rotation: None,
        }
    }

    pub(crate) fn prepare_configuration(
        pattern: Option<&str>,
        path: Option<&str>,
        rotation: Option<RotationConfig>,
    ) -> io::Result<PreparedOutput> {
        let header = pattern.map(HeaderMode::from_pattern).transpose()?;
        if rotation.as_ref().is_some_and(|r| r.max_size == 0 || r.max_files == 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rotation limits must be positive",
            ));
        }
        if rotation.is_some() && path.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "rotation requires a log file",
            ));
        }
        let destination = path
            .map(|path| -> io::Result<_> {
                let file = open_log_file(path)?;
                let size = file.metadata()?.len();
                Ok(PreparedDestination {
                    writer: Box::new(BufWriter::new(file)),
                    rotation: rotation.map(|config| FileRotation {
                        base_path: PathBuf::from(path),
                        config,
                        current_size: size,
                        reopen_pending: false,
                    }),
                })
            })
            .transpose()?;
        Ok(PreparedOutput { header, destination })
    }

    pub(crate) fn apply_configuration(&mut self, prepared: PreparedOutput) -> io::Result<()> {
        self.flush()?;
        if let Some(header) = prepared.header {
            self.header = header;
        }
        if let Some(destination) = prepared.destination {
            self.writer = destination.writer;
            self.rotation = destination.rotation;
        }
        Ok(())
    }

    /// Replaces the underlying writer with the given one.
    pub fn set_writer(&mut self, writer: Box<dyn Write + Send>) {
        self.writer = writer;
        self.rotation = None;
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
        let prepared = Self::prepare_configuration(None, Some(path), Some(config))?;
        self.apply_configuration(prepared)
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
        if self.pending.len() >= MAX_PENDING_BYTES {
            self.flush_required = true;
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "pending log output budget exhausted",
            ));
        }
        let mut record_start = self.pending.len();
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
        if self.pending.len() > MAX_PENDING_BYTES {
            if record_start > 0 {
                if self.flush_required {
                    self.pending.truncate(record_start);
                    self.pending.shrink_to(MAX_PENDING_BYTES);
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "pending log output budget exhausted",
                    ));
                }
                let new_record = self.pending.split_off(record_start);
                self.flush()?;
                record_start = 0;
                self.pending = new_record;
            }
            if record_start == 0 && self.pending.len() > MAX_PENDING_BYTES {
                clamp_single_oversized_record(&mut self.pending);
            }
            self.flush_required = true;
        }
        Ok(())
    }

    /// Returns the number of bytes currently buffered and awaiting flush.
    #[inline]
    pub fn buffered_len(&self) -> usize {
        self.pending.len() - self.write_offset
    }

    /// Returns `true` if there are buffered bytes that have not been flushed yet.
    #[inline]
    pub fn has_pending(&self) -> bool {
        self.write_offset < self.pending.len() || self.writer_needs_flush || self.flush_required
    }

    pub(crate) fn is_flush_required(&self) -> bool {
        self.flush_required
    }

    pub(crate) fn retry_pending(&mut self) -> io::Result<()> {
        if self.flush_required {
            self.flush()?;
        }
        Ok(())
    }

    /// Flushes all pending bytes to the underlying writer.
    /// If rotation is configured and the file size exceeds the threshold,
    /// the log file is rotated after flushing.
    pub fn flush(&mut self) -> io::Result<()> {
        self.flush_required = true;
        while self.write_offset < self.pending.len() {
            match self.writer.write(&self.pending[self.write_offset..]) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "log writer made no progress",
                    ));
                }
                Ok(written) => {
                    self.write_offset += written;
                    self.writer_needs_flush = true;
                    if let Some(rotation) = &mut self.rotation {
                        rotation.current_size =
                            rotation.current_size.saturating_add(written as u64);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        self.pending.clear();
        if self.pending.capacity() > MAX_PENDING_BYTES {
            self.pending.shrink_to(MAX_PENDING_BYTES);
        }
        self.write_offset = 0;
        if self.writer_needs_flush {
            self.writer.flush()?;
            self.writer_needs_flush = false;
        }
        if self.rotation.as_ref().is_some_and(|r| r.current_size >= r.config.max_size) {
            self.rotate()?;
        }
        self.flush_required = false;
        Ok(())
    }

    /// Rotates the current log file by renaming it through a numbered
    /// suffix chain (base -> base.1 -> base.2 -> ...) and opening a
    /// fresh file at the base path.
    fn rotate(&mut self) -> io::Result<()> {
        self.rotate_with(
            |path| fs::remove_file(path),
            |from, to| fs::rename(from, to),
            |path| {
                let file = OpenOptions::new().create(true).append(true).open(path)?;
                Ok(Box::new(BufWriter::new(file)))
            },
        )
    }

    fn rotate_with(
        &mut self,
        mut remove: impl FnMut(&Path) -> io::Result<()>,
        mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
        open: impl FnOnce(&Path) -> io::Result<Box<dyn Write + Send>>,
    ) -> io::Result<()> {
        let rotation = self.rotation.as_mut().expect("rotate called without rotation config");
        let base_path = &rotation.base_path;
        let max_files = rotation.config.max_files;

        if !rotation.reopen_pending {
            // Keep the old handle live until the replacement has opened successfully.
            let oldest = format!("{}.{}", base_path.display(), max_files);
            ignore_missing(remove(Path::new(&oldest)))?;
            for i in (1..max_files).rev() {
                let from = format!("{}.{}", base_path.display(), i);
                let to = format!("{}.{}", base_path.display(), i + 1);
                ignore_missing(rename(Path::new(&from), Path::new(&to)))?;
            }
            let first_rotated = format!("{}.1", base_path.display());
            rename(base_path, Path::new(&first_rotated))?;
            // On open failure, retry opening only: repeated renames could unlink
            // the retained active writer once it reaches the retention limit.
            rotation.reopen_pending = true;
        }

        // Open a new file at the base path
        self.writer = open(base_path)?;

        rotation.current_size = 0;
        rotation.reopen_pending = false;
        Ok(())
    }
}

fn open_log_file(path: &str) -> io::Result<File> {
    OpenOptions::new().create(true).append(true).open(path)
}

fn clamp_single_oversized_record(pending: &mut Vec<u8>) {
    let limit = MAX_PENDING_BYTES - 1;
    let mut cut = limit.min(pending.len());
    while cut > 0 && (pending[cut] & 0b1100_0000) == 0b1000_0000 && limit - cut < 4 {
        cut -= 1;
    }
    pending.truncate(cut);
    pending.push(b'\n');
    pending.shrink_to(MAX_PENDING_BYTES);
}

fn ignore_missing(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retrying_reopen_does_not_rotate_or_unlink_the_retained_writer() {
        pending_reopen_preserves_retained_writer();
        #[cfg(not(miri))]
        {
            let directory = std::env::temp_dir().join(format!(
                "zerolog-reopen-{}-{}",
                std::process::id(),
                crate::timestamp_counter::TimestampCounter::read_timestamp_counter()
            ));
            fs::create_dir(&directory).unwrap();
            let base = directory.join("output");
            let retained = directory.join("output.1");
            let mut output = OutputBuffer::new();
            output
                .set_log_file_with_rotation(base.to_str().unwrap(), RotationConfig::by_size(1, 1))
                .unwrap();
            output.writer.write_all(b"accepted").unwrap();
            output.writer.flush().unwrap();
            fs::rename(&base, &retained).unwrap();
            fs::create_dir(&base).unwrap();
            // Reproduce the valid state between renaming the active file and reopening its path.
            let rotation = output.rotation.as_mut().unwrap();
            rotation.current_size = 8;
            rotation.reopen_pending = true;
            for _ in 0..3 {
                assert!(output.flush().is_err());
                assert_eq!(fs::read(&retained).unwrap(), b"accepted");
            }
            fs::remove_dir(&base).unwrap();
            output.flush().unwrap();
            assert_eq!(fs::read(&retained).unwrap(), b"accepted");
            assert!(base.is_file());
            drop(output);
            fs::remove_dir_all(directory).unwrap();
        }
    }
    fn pending_reopen_preserves_retained_writer() {
        use std::sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        };
        struct MemoryFile(Arc<AtomicU64>);
        impl Write for MemoryFile {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.store(u64::from_le_bytes(bytes.try_into().unwrap()), Ordering::Relaxed);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let retained = Arc::new(AtomicU64::new(0));
        let mut output = OutputBuffer::new();
        output.set_writer(Box::new(MemoryFile(retained.clone())));
        output.writer.write_all(b"accepted").unwrap();
        output.rotation = Some(FileRotation {
            base_path: PathBuf::from("output"),
            config: RotationConfig::by_size(1, 1),
            current_size: 8,
            reopen_pending: true,
        });
        for _ in 0..3 {
            let error = output
                .rotate_with(
                    |_| panic!("a reopen retry must not remove retained files"),
                    |_, _| panic!("a reopen retry must not rename retained files"),
                    |_| Err(io::Error::other("replacement unavailable")),
                )
                .unwrap_err();
            assert_eq!(error.to_string(), "replacement unavailable");
            assert!(output.rotation.as_ref().unwrap().reopen_pending);
            assert_eq!(retained.load(Ordering::Relaxed).to_le_bytes(), *b"accepted");
        }
        // The old writer remains usable after each failed replacement.
        output.writer.write_all(b"retained").unwrap();
        assert_eq!(retained.load(Ordering::Relaxed).to_le_bytes(), *b"retained");
        output
            .rotate_with(
                |_| panic!("a reopen retry must not remove retained files"),
                |_, _| panic!("a reopen retry must not rename retained files"),
                |_| Ok(Box::new(io::sink())),
            )
            .unwrap();
        let rotation = output.rotation.as_ref().unwrap();
        assert!(!rotation.reopen_pending);
        assert_eq!(rotation.current_size, 0);
        assert_eq!(retained.load(Ordering::Relaxed).to_le_bytes(), *b"retained");
    }
}

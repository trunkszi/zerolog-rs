#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use uarch::prelude::*;

use crate::{
    Level,
    contract::FrozenSlab,
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
    rotated_paths: Vec<PathBuf>,
    config: RotationConfig,
    current_size: u64,
    reopen_pending: bool,
}

impl FileRotation {
    fn new(base_path: PathBuf, config: RotationConfig, current_size: u64) -> Self {
        let mut rotated_paths = Vec::with_capacity(config.max_files as usize);
        let display = base_path.display();
        for index in 1..=config.max_files {
            rotated_paths.push(PathBuf::from(format!("{display}.{index}")));
        }
        Self { base_path, rotated_paths, config, current_size, reopen_pending: false }
    }

    fn open(path: &str, config: RotationConfig) -> io::Result<(File, Self)> {
        let file = open_log_file(path)?;
        let size = file.metadata()?.len();
        Ok((file, Self::new(PathBuf::from(path), config, size)))
    }
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
const DEFAULT_SLAB_CAPACITY: usize = 8 * 1024;
const MAX_RECYCLED_SLAB_CAPACITY: usize = 64 * 1024;

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
    recycle_slab: Option<Box<[u8]>>,
    write_offset: usize,
    writer_needs_flush: bool,
    flush_required: bool,
    external_consumer: bool,
    frozen_in_flight: bool,
    frozen_bytes: usize,
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
            pending: Vec::with_capacity(DEFAULT_SLAB_CAPACITY),
            recycle_slab: None,
            write_offset: 0,
            writer_needs_flush: false,
            flush_required: false,
            external_consumer: false,
            frozen_in_flight: false,
            frozen_bytes: 0,
            writer: Box::new(io::stderr()),
            header: HeaderMode::Default(DefaultHeader::new()),
            rotation: None,
        }
    }

    pub(crate) fn set_external_consumer(&mut self, external: bool) {
        self.external_consumer = external;
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
                match rotation {
                    Some(config) => {
                        let (file, file_rotation) = FileRotation::open(path, config)?;
                        Ok(PreparedDestination {
                            writer: Box::new(file),
                            rotation: Some(file_rotation),
                        })
                    }
                    None => {
                        let file = open_log_file(path)?;
                        Ok(PreparedDestination { writer: Box::new(file), rotation: None })
                    }
                }
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
        self.writer = Box::new(file);
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
        let pending_limit = MAX_PENDING_BYTES.saturating_sub(self.frozen_bytes);
        if unlikely(self.write_offset > 0 && self.pending.len() >= pending_limit) {
            self.pending.drain(..self.write_offset);
            self.write_offset = 0;
        }
        if unlikely(self.pending.len() >= pending_limit) {
            self.flush_required = true;
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "pending log output budget exhausted",
            ));
        }
        let record_start = self.pending.len();
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
        if unlikely(self.pending.len() > pending_limit) {
            let external_mode = self.external_consumer || self.frozen_in_flight;
            if record_start > 0 || self.frozen_bytes > 0 {
                if self.flush_required || external_mode {
                    self.pending.truncate(record_start);
                    self.pending.shrink_to(MAX_PENDING_BYTES);
                    self.flush_required = true;
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        "pending log output budget exhausted",
                    ));
                }
                let new_record = self.pending.split_off(record_start);
                if let Err(err) = self.flush() {
                    self.pending.extend_from_slice(&new_record);
                    self.flush_required = true;
                    return Err(err);
                }
                self.pending = new_record;
            }
            if self.pending.len() > MAX_PENDING_BYTES {
                clamp_single_oversized_record(&mut self.pending);
            }
            self.flush_required = true;
        }
        Ok(())
    }

    /// Extracts currently buffered bytes into an owned [`FrozenSlab`] for asynchronous I/O.
    pub fn freeze_pending(&mut self) -> Option<FrozenSlab> {
        if self.buffered_len() == 0 {
            return None;
        }
        if self.write_offset > 0 {
            self.pending.drain(..self.write_offset);
            self.write_offset = 0;
        }
        let active_len = self.pending.len().min(u32::MAX as usize) as u32;
        let replacement = match self.recycle_slab.take() {
            Some(boxed) => {
                let mut vec = Vec::from(boxed);
                vec.clear();
                vec
            }
            None => Vec::with_capacity(DEFAULT_SLAB_CAPACITY),
        };
        let mut frozen = std::mem::replace(&mut self.pending, replacement);
        let cap = frozen.capacity();
        if cap > frozen.len() && cap <= MAX_RECYCLED_SLAB_CAPACITY {
            frozen.resize(cap, 0);
        }
        self.flush_required = false;
        self.frozen_in_flight = true;
        self.frozen_bytes = active_len as usize;
        Some(FrozenSlab::new(frozen.into_boxed_slice(), active_len))
    }

    /// Advances an in-flight [`FrozenSlab`] by `written` bytes, recycling its allocation once empty.
    pub fn complete_frozen(&mut self, mut slab: FrozenSlab, written: u32) -> Option<FrozenSlab> {
        let completed = slab.advance(written);
        if let Some(rotation) = &mut self.rotation {
            rotation.current_size = rotation.current_size.saturating_add(written as u64);
        }
        if !completed {
            self.frozen_bytes = slab.len() as usize;
            return Some(slab);
        }
        self.frozen_in_flight = false;
        self.frozen_bytes = 0;
        self.store_recycle_slab(slab.into_inner_box());
        None
    }

    /// Restores unwritten bytes from a failed [`FrozenSlab`] back to the front of `pending`.
    #[cold_sink]
    pub fn restore_failed_slab(&mut self, slab: FrozenSlab) {
        self.frozen_in_flight = false;
        self.frozen_bytes = 0;
        if slab.is_empty() {
            self.store_recycle_slab(slab.into_inner_box());
            return;
        }
        if self.write_offset > 0 {
            self.pending.drain(..self.write_offset);
            self.write_offset = 0;
        }
        self.pending.splice(0..0, slab.active_slice().iter().copied());
        self.write_offset = 0;
        self.flush_required = true;
        self.store_recycle_slab(slab.into_inner_box());
    }

    fn store_recycle_slab(&mut self, bytes: Box<[u8]>) {
        if bytes.len() <= MAX_RECYCLED_SLAB_CAPACITY
            && self.recycle_slab.as_ref().is_none_or(|existing| existing.len() < bytes.len())
        {
            self.recycle_slab = Some(bytes);
        }
    }

    /// Returns the number of bytes currently buffered and awaiting flush.
    #[inline]
    pub fn buffered_len(&self) -> usize {
        self.pending.len() - self.write_offset
    }

    #[inline]
    pub fn total_unflushed_bytes(&self) -> usize {
        self.buffered_len().saturating_add(self.frozen_bytes)
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
                Ok(Box::new(file))
            },
        )
    }

    fn rotate_with(
        &mut self,
        mut remove: impl FnMut(&Path) -> io::Result<()>,
        mut rename: impl FnMut(&Path, &Path) -> io::Result<()>,
        open: impl FnOnce(&Path) -> io::Result<Box<dyn Write + Send>>,
    ) -> io::Result<()> {
        let Some(rotation) = self.rotation.as_mut() else {
            return Ok(());
        };

        if !rotation.reopen_pending {
            // Keep the old handle live until the replacement has opened successfully.
            if let Some(oldest) = rotation.rotated_paths.last() {
                ignore_missing(remove(oldest.as_path()))?;
            }
            for index in (0..rotation.rotated_paths.len().saturating_sub(1)).rev() {
                ignore_missing(rename(
                    rotation.rotated_paths[index].as_path(),
                    rotation.rotated_paths[index + 1].as_path(),
                ))?;
            }
            if let Some(first_rotated) = rotation.rotated_paths.first() {
                rename(rotation.base_path.as_path(), first_rotated.as_path())?;
            }
            // On open failure, retry opening only: repeated renames could unlink
            // the retained active writer once it reaches the retention limit.
            rotation.reopen_pending = true;
        }

        // Open a new file at the base path
        self.writer = open(&rotation.base_path)?;

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
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn reopen_retry_keeps_retained_writer() {
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
        let mut rotation =
            FileRotation::new(PathBuf::from("output"), RotationConfig::by_size(1, 1), 8);
        rotation.reopen_pending = true;
        output.rotation = Some(rotation);
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

    #[test]
    fn external_overflow_blocks_and_failed_flush_retains() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };
        struct CountingFailWriter(Arc<AtomicUsize>);
        impl Write for CountingFailWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "sink unavailable"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let writer_calls = Arc::new(AtomicUsize::new(0));
        let mut output = OutputBuffer::new();
        output.set_header_pattern("").unwrap();
        output.set_writer(Box::new(CountingFailWriter(writer_calls.clone())));

        output
            .write_record(0, Level::INF, b"t", "loc", 0, |b| b.extend_from_slice(b"seed"))
            .unwrap();
        let in_flight = output.freeze_pending().unwrap();
        assert_eq!(in_flight.active_slice(), b"seed\n");

        output
            .write_record(0, Level::INF, b"t", "loc", 0, |b| {
                b.resize(b.len() + MAX_PENDING_BYTES - 8, b'a');
            })
            .unwrap();
        let err = output
            .write_record(0, Level::INF, b"t", "loc", 0, |b| {
                b.extend_from_slice(b"overflow-after-freeze");
            })
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(writer_calls.load(Ordering::Relaxed), 0);
        assert_eq!(output.buffered_len(), MAX_PENDING_BYTES - 7);
        assert!(output.complete_frozen(in_flight, 5).is_none());

        let slab2 = output.freeze_pending().unwrap();
        let len2 = slab2.len();
        assert!(output.complete_frozen(slab2, len2).is_none());

        output
            .write_record(0, Level::INF, b"t", "loc", 0, |b| {
                b.resize(b.len() + MAX_PENDING_BYTES - 8, b'x');
            })
            .unwrap();
        let flush_err = output
            .write_record(0, Level::INF, b"t", "loc", 0, |b| {
                b.extend_from_slice(b"retained-tail");
            })
            .unwrap_err();
        assert_eq!(flush_err.kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(writer_calls.load(Ordering::Relaxed), 1);
        assert!(output.pending.ends_with(b"retained-tail\n"));
    }
    #[test]
    fn frozen_budget_tracks_partial_writes() -> Result<(), Box<dyn std::error::Error>> {
        let mut output = OutputBuffer::new();
        output.set_header_pattern("")?;
        output.set_writer(Box::new(io::sink()));
        output.write_record(0, Level::INF, b"t", "loc", 0, |bytes| {
            bytes.resize(MAX_PENDING_BYTES - 1, b'a');
        })?;
        let slab = output.freeze_pending().ok_or("missing full frozen slab")?;
        assert_eq!(output.total_unflushed_bytes(), MAX_PENDING_BYTES);
        let error = output
            .write_record(0, Level::INF, b"t", "loc", 0, |_| {
                panic!("full combined budget must reject before formatting");
            })
            .err()
            .ok_or("expected backpressure at zero pending quota")?;
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        let half = MAX_PENDING_BYTES / 2;
        let suffix = output.complete_frozen(slab, half as u32).ok_or("missing partial suffix")?;
        assert_eq!(output.total_unflushed_bytes(), half);
        let error = output
            .write_record(0, Level::INF, b"t", "loc", 0, |bytes| {
                bytes.resize(half, b'b');
            })
            .err()
            .ok_or("oversized first pending record must wait")?;
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(output.buffered_len(), 0);
        assert_eq!(output.total_unflushed_bytes(), half);
        output.write_record(0, Level::INF, b"t", "loc", 0, |bytes| {
            bytes.resize(half - 1, b'c');
        })?;
        assert_eq!(output.total_unflushed_bytes(), MAX_PENDING_BYTES);
        output.restore_failed_slab(suffix);
        assert_eq!(output.buffered_len(), MAX_PENDING_BYTES);
        assert_eq!(output.total_unflushed_bytes(), MAX_PENDING_BYTES);
        let restored = output.freeze_pending().ok_or("missing restored slab")?;
        assert_eq!(restored.len() as usize, MAX_PENDING_BYTES);
        assert!(output.complete_frozen(restored, MAX_PENDING_BYTES as u32).is_none());
        assert_eq!(output.total_unflushed_bytes(), 0);
        output.write_record(0, Level::INF, b"t", "loc", 0, |bytes| {
            bytes.extend_from_slice(b"next")
        })?;
        assert_eq!(output.total_unflushed_bytes(), 5);
        Ok(())
    }
}

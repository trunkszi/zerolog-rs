use std::{io, time::Duration};

use crate::{Level, QueueFullCallback, QueueFullPolicy, logger, output::RotationConfig};

/// Owns one background polling generation. Drop joins and flushes only while
/// that generation remains active; a manual stop or later start revokes ownership.
#[must_use = "dropping ZeroLogGuard stops its active background polling generation; bind it to a variable (e.g. let _guard = ...)"]
pub struct ZeroLogGuard {
    generation: Option<u64>,
}

impl ZeroLogGuard {
    /// Joins this generation and drains output, returning any shutdown error.
    /// A stale guard does not stop or flush a newer generation.
    pub fn shutdown(mut self) -> io::Result<()> {
        self.generation.take().map_or(Ok(()), logger::shutdown_generation)
    }
}

impl Drop for ZeroLogGuard {
    fn drop(&mut self) {
        if let Some(generation) = self.generation.take() {
            let _ = logger::shutdown_generation(generation);
        }
    }
}

/// Builder for configuring and starting the zerolog logger.
///
/// # Example
///
/// ```ignore
/// let _guard = zerolog::builder()
///     .level(zerolog::INFO)
///     .log_file("/var/log/app.log")
///     .rotation(RotationConfig::by_size(100_000_000, 5))
///     .flush_delay(Duration::from_secs(3))
///     .flush_buffer_size(8192)
///     .flush_on(zerolog::ERR)
///     .poller_interval(Duration::from_millis(1))
///     .poller_core_affinity(3)
///     .start()?;
/// // guard drop automatically stops poller and flushes
/// ```
pub struct ZeroLogBuilder {
    level: Level,
    flush_delay: Duration,
    flush_buffer_size: u32,
    flush_level: Level,
    queue_full_policy: QueueFullPolicy,
    queue_full_callback: Option<QueueFullCallback>,
    log_file: Option<String>,
    rotation: Option<RotationConfig>,
    header_pattern: Option<String>,
    poller_interval: Duration,
    poller_core_affinity: Option<usize>,
}

impl Default for ZeroLogBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl ZeroLogBuilder {
    /// Creates a new builder with default settings.
    pub fn new() -> Self {
        Self {
            level: Level::DBG,
            flush_delay: Duration::from_secs(3),
            flush_buffer_size: 8 * 1024,
            flush_level: Level::OFF,
            queue_full_policy: QueueFullPolicy::Drop,
            queue_full_callback: None,
            log_file: None,
            rotation: None,
            header_pattern: None,
            poller_interval: Duration::from_millis(1),
            poller_core_affinity: None,
        }
    }

    /// Set the minimum log level.
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Set the flush delay duration.
    pub fn flush_delay(mut self, delay: Duration) -> Self {
        self.flush_delay = delay;
        self
    }

    /// Set the flush buffer size in bytes.
    pub fn flush_buffer_size(mut self, bytes: u32) -> Self {
        self.flush_buffer_size = bytes;
        self
    }

    /// Set the level at or above which an immediate flush is triggered.
    pub fn flush_on(mut self, level: Level) -> Self {
        self.flush_level = level;
        self
    }

    /// Set the queue-full policy.
    pub fn queue_full_policy(mut self, policy: QueueFullPolicy) -> Self {
        self.queue_full_policy = policy;
        self
    }

    /// Set the queue-full callback.
    pub fn queue_full_callback(mut self, callback: QueueFullCallback) -> Self {
        self.queue_full_callback = Some(callback);
        self
    }

    /// Set the log output file path.
    pub fn log_file(mut self, path: impl Into<String>) -> Self {
        self.log_file = Some(path.into());
        self
    }

    /// Set the log file rotation config. Requires `log_file` to be set.
    pub fn rotation(mut self, config: RotationConfig) -> Self {
        self.rotation = Some(config);
        self
    }

    /// Set a custom header pattern string.
    pub fn header_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.header_pattern = Some(pattern.into());
        self
    }

    /// Set the poller thread interval.
    pub fn poller_interval(mut self, interval: Duration) -> Self {
        self.poller_interval = interval;
        self
    }

    /// Pin the poller thread to the given CPU core (Linux only; ignored on
    /// other platforms).
    pub fn poller_core_affinity(mut self, core_id: usize) -> Self {
        self.poller_core_affinity = Some(core_id);
        self
    }

    /// Apply all configuration and start the background polling thread.
    ///
    /// Returns a [`ZeroLogGuard`] that stops the poller and flushes when
    /// dropped.
    pub fn start(self) -> io::Result<ZeroLogGuard> {
        // Prepare every fallible header/file operation before touching global settings.
        let prepared = crate::output::OutputBuffer::prepare_configuration(
            self.header_pattern.as_deref(),
            self.log_file.as_deref(),
            self.rotation,
        )?;
        crate::thread_buffer::preallocate();
        let generation = logger::start_configured_polling_thread(
            self.poller_interval.as_nanos().min(i64::MAX as u128) as i64,
            self.poller_core_affinity,
            move |output| {
                output.apply_configuration(prepared)?;
                logger::set_level(self.level);
                logger::set_flush_delay(self.flush_delay.as_nanos().min(i64::MAX as u128) as i64);
                logger::set_flush_buffer_size(self.flush_buffer_size);
                logger::flush_on(self.flush_level);
                logger::set_queue_full_policy(self.queue_full_policy);
                logger::set_queue_full_callback(self.queue_full_callback);
                Ok(())
            },
        )?;
        Ok(ZeroLogGuard { generation: Some(generation) })
    }
}

/// Creates a new [`ZeroLogBuilder`] for configuring the logger.
pub fn builder() -> ZeroLogBuilder {
    ZeroLogBuilder::new()
}

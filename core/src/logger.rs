#[cfg(debug_assertions)]
use std::sync::atomic::AtomicBool;
use std::{
    cell::UnsafeCell,
    io::{self, Write},
    sync::{
        OnceLock,
        atomic::{AtomicI64, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
};

use crate::{
    Level, StaticHeaderWriteFn, level_from_u8,
    output::{OutputBuffer, RotationConfig},
    poller::{self, PollerState, ReadyMessageHeap},
    spsc_queue::{PAYLOAD_ARGS_OFFSET, PAYLOAD_INFO_OFFSET, SpscVarQueue},
    thread_buffer::{THREAD_BUFFER_REGISTRY, ThreadBuffer},
    timestamp_counter::TimestampCounter,
};

/// Function pointer type for decoding a log record's payload into a formatted buffer.
pub type DecodeFn = unsafe fn(*const u8, &mut Vec<u8>);

/// Callback function type invoked when the per-thread SPSC queue is full.
/// The argument is the cumulative count of queue-full events.
pub type QueueFullCallback = fn(u64);

/// Default log level (DBG = accept all messages).
const DEFAULT_LEVEL: u8 = Level::DBG as u8;

/// Default flush delay (3 seconds in nanoseconds).
const DEFAULT_FLUSH_DELAY_NS: i64 = 3_000_000_000;

/// Default flush buffer size (8 KB).
const DEFAULT_FLUSH_BUFFER_SIZE: u32 = 8 * 1024;

/// Default flush level (OFF = never auto-flush by level).
const DEFAULT_FLUSH_LEVEL: u8 = Level::OFF as u8;

/// Default queue-full policy (Drop).
const DEFAULT_QUEUE_FULL_POLICY: u8 = QueueFullPolicy::Drop as u8;

/// Global log level filter. Read on every `log!()` call (hot path).
///
/// All top-level static configuration fields below are typically written once
/// at startup (write-once-read-forever) and read on every log call or
/// queue-full event. Placing them as top-level statics eliminates the
/// `OnceLock` indirection of `GlobalLogger` on the hot path.
///
/// # Ordering rationale
///
/// - `CURRENT_LEVEL` uses `Relaxed`: log level filtering is eventually
///   consistent; a few extra messages after `set_level()` is acceptable.
///   On x86 a `Relaxed` load is identical to a plain `mov`.
/// - Config fields (`FLUSH_*`, `QUEUE_FULL_*`) use `Relaxed`: they are set
///   once at startup and read on cold paths (poll / queue-full). Eventual
///   consistency is sufficient.
/// - `QUEUE_FULL_COUNT` uses `Relaxed` for `fetch_add`: the count is
///   advisory; exact ordering between increments across threads is not
///   required.
static CURRENT_LEVEL: AtomicU8 = AtomicU8::new(DEFAULT_LEVEL);

/// Flush delay in nanoseconds. Read by the poller each cycle.
static FLUSH_DELAY_NS: AtomicI64 = AtomicI64::new(DEFAULT_FLUSH_DELAY_NS);

/// Flush buffer size threshold in bytes. Read by the poller each cycle.
static FLUSH_BUFFER_SIZE: AtomicU32 = AtomicU32::new(DEFAULT_FLUSH_BUFFER_SIZE);

/// Minimum level that triggers an immediate flush. Read by the poller.
static FLUSH_LEVEL: AtomicU8 = AtomicU8::new(DEFAULT_FLUSH_LEVEL);

/// Queue-full policy (Drop=0, Block=1). Read when queue is full.
static QUEUE_FULL_POLICY: AtomicU8 = AtomicU8::new(DEFAULT_QUEUE_FULL_POLICY);

/// Cumulative count of queue-full events. Incremented by writer threads.
static QUEUE_FULL_COUNT: AtomicU64 = AtomicU64::new(0);

/// Callback function pointer for queue-full events. null = no callback.
static QUEUE_FULL_CALLBACK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Policy applied when a thread's SPSC log queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QueueFullPolicy {
    /// Drop the log message.
    Drop = 0,
    /// Block (spin) until space becomes available.
    Block = 1,
}

/// Internal action derived from [`QueueFullPolicy`].
pub(crate) enum QueueFullAction {
    /// Discard the current log message.
    Drop,
    /// Retry pushing the message into the queue.
    Retry,
}

/// Compile-time metadata associated with a single log call site.
#[derive(Clone, Copy)]
pub struct StaticInfo {
    /// Severity level of this call site.
    pub level: Level,
    /// Source location string (e.g. `"src/main.rs:42"`).
    pub location: &'static str,
    /// The format string for this log call site.
    pub format_string: &'static str,
    /// Decoder function that reads the payload and appends formatted output.
    pub decode: DecodeFn,
}

// ---------------------------------------------------------------------------
// GlobalLogger — only holds state that requires runtime initialization
// ---------------------------------------------------------------------------

/// Mutable state accessed exclusively by the poller thread (or the
/// configuration API when the poller is stopped).
struct LoggerInner {
    timestamp_counter: TimestampCounter,
    output: OutputBuffer,
    next_flush_deadline_nanoseconds: Option<i64>,
}

/// The global logger instance.
///
/// Only contains state that requires runtime initialization
/// ([`TimestampCounter`], [`OutputBuffer`]) and the poller state machine.
/// All configuration atomics live as top-level statics so the hot path
/// (`check_level`) never touches this struct.
struct GlobalLogger {
    inner: UnsafeCell<LoggerInner>,
    poller_state: PollerState,
    #[cfg(debug_assertions)]
    inner_in_use: AtomicBool,
}

// Safety: LoggerInner has exclusive access guaranteed by the poller state
// machine. PollerState is internally Sync.
unsafe impl Sync for GlobalLogger {}

impl GlobalLogger {
    fn new() -> Self {
        let mut timestamp_counter = TimestampCounter::new();
        timestamp_counter.init_default();
        Self {
            inner: UnsafeCell::new(LoggerInner {
                timestamp_counter,
                output: OutputBuffer::new(),
                next_flush_deadline_nanoseconds: None,
            }),
            poller_state: PollerState::default(),
            #[cfg(debug_assertions)]
            inner_in_use: AtomicBool::new(false),
        }
    }

    /// Obtain an exclusive raw pointer to [`LoggerInner`].
    ///
    /// # Safety
    /// The caller must guarantee that only one thread accesses `inner`
    /// at any given time:
    /// - During `poll()`: the poller thread has exclusive access.
    /// - During configuration API calls: the poller must be stopped.
    ///
    /// The returned pointer must not be used to create overlapping `&mut` references.
    #[inline]
    unsafe fn inner_ptr(&self) -> *mut LoggerInner {
        #[cfg(debug_assertions)]
        assert!(
            !self.inner_in_use.swap(true, Ordering::AcqRel),
            "ferrilog: concurrent access to LoggerInner detected"
        );
        self.inner.get()
    }

    /// Release the exclusive-access marker for `inner` (debug mode only).
    #[inline]
    unsafe fn inner_release(&self) {
        #[cfg(debug_assertions)]
        self.inner_in_use.store(false, Ordering::Release);
    }

    fn ensure_poller_stopped(&self) -> io::Result<()> {
        if self.poller_state.is_running() {
            Err(io::Error::other("polling thread is running; call stop_polling_thread() first"))
        } else {
            Ok(())
        }
    }
}

/// Return a reference to the lazily-initialized global logger singleton.
///
/// This is only called on cold paths (poll, output/header configuration,
/// poller start/stop). The hot path (`check_level`) does NOT go through
/// this function.
fn global_logger() -> &'static GlobalLogger {
    static LOGGER: OnceLock<GlobalLogger> = OnceLock::new();
    LOGGER.get_or_init(GlobalLogger::new)
}

// ---------------------------------------------------------------------------
// Public API — hot-path level check (no OnceLock indirection)
// ---------------------------------------------------------------------------

/// Set the global minimum log level. Messages below this level are discarded.
///
/// Uses `Relaxed` ordering: the change is eventually visible to all threads.
pub fn set_level(level: Level) {
    CURRENT_LEVEL.store(level as u8, Ordering::Relaxed);
}

/// Return `true` if the given level is enabled under the current global level.
///
/// This is called on every `log!()` invocation. It is a single `Relaxed`
/// atomic load — no OnceLock, no indirection, one instruction on x86.
#[inline(always)]
pub fn check_level(level: Level) -> bool {
    level as u8 >= CURRENT_LEVEL.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Public API — configuration (write-once at startup, read on cold paths)
// ---------------------------------------------------------------------------

/// Replace the output writer. The polling thread must be stopped first.
pub fn set_output_writer<W>(writer: W)
where
    W: Write + Send + 'static,
{
    let logger = global_logger();
    logger
        .ensure_poller_stopped()
        .expect("set_output_writer requires the polling thread to be stopped first");
    // Safety: poller is stopped, only the current thread accesses inner
    unsafe {
        let inner = &mut *logger.inner_ptr();
        inner.output.set_writer(Box::new(writer));
        logger.inner_release();
    }
}

/// Set the policy applied when a thread's log queue is full.
pub fn set_queue_full_policy(policy: QueueFullPolicy) {
    QUEUE_FULL_POLICY.store(policy as u8, Ordering::Relaxed);
}

/// Set (or clear) the callback invoked on each queue-full event.
pub fn set_queue_full_callback(callback: Option<QueueFullCallback>) {
    let ptr = match callback {
        Some(f) => f as *mut (),
        None => std::ptr::null_mut(),
    };
    QUEUE_FULL_CALLBACK.store(ptr, Ordering::Release);
}

/// Return the cumulative number of queue-full events since the last reset.
pub fn queue_full_count() -> u64 {
    QUEUE_FULL_COUNT.load(Ordering::Relaxed)
}

/// Reset the queue-full event counter to zero.
pub fn reset_queue_full_count() {
    QUEUE_FULL_COUNT.store(0, Ordering::Relaxed);
}

/// Set the flush delay in nanoseconds. Buffered output is flushed after
/// this duration elapses with no new messages.
pub fn set_flush_delay(nanoseconds: i64) {
    FLUSH_DELAY_NS.store(nanoseconds.max(0), Ordering::Relaxed);
}

/// Set the flush buffer size threshold in bytes. The output buffer is
/// flushed when its length reaches this value.
pub fn set_flush_buffer_size(bytes: u32) {
    FLUSH_BUFFER_SIZE.store(bytes, Ordering::Relaxed);
}

/// Set the level at or above which a flush is triggered immediately
/// after writing a record.
pub fn flush_on(level: Level) {
    FLUSH_LEVEL.store(level as u8, Ordering::Relaxed);
}

/// Set the log output file path. The polling thread must be stopped first.
pub fn set_log_file(path: &str) -> io::Result<()> {
    let logger = global_logger();
    logger.ensure_poller_stopped()?;
    // Safety: poller is stopped, only the current thread accesses inner
    unsafe {
        let inner = &mut *logger.inner_ptr();
        let result = inner.output.set_log_file(path);
        logger.inner_release();
        result
    }
}

/// Set the log file with automatic size-based rotation.
/// The polling thread must be stopped first.
pub fn set_log_file_with_rotation(path: &str, config: RotationConfig) -> io::Result<()> {
    let logger = global_logger();
    logger.ensure_poller_stopped()?;
    // Safety: poller is stopped, only the current thread accesses inner
    unsafe {
        let inner = &mut *logger.inner_ptr();
        let result = inner.output.set_log_file_with_rotation(path, config);
        logger.inner_release();
        result
    }
}

/// Set the header pattern for log output formatting. The polling thread
/// must be stopped first.
pub fn set_header_pattern(pattern: &str) -> io::Result<()> {
    let logger = global_logger();
    logger.ensure_poller_stopped()?;
    // Safety: poller is stopped, only the current thread accesses inner
    unsafe {
        let inner = &mut *logger.inner_ptr();
        let result = inner.output.set_header_pattern(pattern);
        logger.inner_release();
        result
    }
}

/// Install a compile-time-generated static header writer. The polling thread
/// must be stopped first.
pub fn set_static_header(write: StaticHeaderWriteFn) -> io::Result<()> {
    let logger = global_logger();
    logger.ensure_poller_stopped()?;
    unsafe {
        let inner = &mut *logger.inner_ptr();
        inner.output.set_static_header(write);
        logger.inner_release();
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Public API — poller control
// ---------------------------------------------------------------------------

/// Start the background polling thread that drains per-thread log queues.
pub fn start_polling_thread(interval_nanoseconds: i64) -> io::Result<()> {
    global_logger().poller_state.start(interval_nanoseconds, crate::logger::poll)
}

/// Start the background polling thread pinned to the specified CPU core.
/// On Linux this uses `sched_setaffinity`; on other platforms the core_id hint is ignored.
pub fn start_polling_thread_on_core(interval_nanoseconds: i64, core_id: usize) -> io::Result<()> {
    global_logger().poller_state.start_on_core(
        interval_nanoseconds,
        crate::logger::poll,
        Some(core_id),
    )
}

/// Stop the background polling thread and wait for it to finish.
pub fn stop_polling_thread() -> io::Result<()> {
    global_logger().poller_state.stop()
}

// ---------------------------------------------------------------------------
// Poll — drains all thread buffers, decodes messages, writes output
// ---------------------------------------------------------------------------

/// Poll all thread buffers once, decode pending messages, and write them
/// to the output. If `force_flush` is true the output is flushed
/// unconditionally.
pub fn poll(force_flush: bool) -> io::Result<()> {
    // Read config from top-level statics (no OnceLock indirection)
    let flush_buffer_size = FLUSH_BUFFER_SIZE.load(Ordering::Relaxed) as usize;
    let flush_level = level_from_u8(FLUSH_LEVEL.load(Ordering::Relaxed));
    let flush_delay_nanoseconds = FLUSH_DELAY_NS.load(Ordering::Relaxed);

    let mut buffers = Vec::with_capacity(THREAD_BUFFER_REGISTRY.count());
    for index in 0..THREAD_BUFFER_REGISTRY.count() {
        let pointer = THREAD_BUFFER_REGISTRY.get(index);
        if !pointer.is_null() {
            buffers.push(pointer);
        }
    }

    let logger = global_logger();
    // Safety: poll() is only called from the poller thread (or manually, never concurrently)
    let inner = unsafe { &mut *logger.inner_ptr() };
    let result = poll_inner(
        inner,
        &buffers,
        flush_buffer_size,
        flush_level,
        flush_delay_nanoseconds,
        force_flush,
    );
    unsafe { logger.inner_release() };
    result
}

fn poll_inner(
    inner: &mut LoggerInner,
    buffers: &[*mut ThreadBuffer],
    flush_buffer_size: usize,
    flush_level: Level,
    flush_delay_nanoseconds: i64,
    force_flush: bool,
) -> io::Result<()> {
    let LoggerInner { timestamp_counter, output, next_flush_deadline_nanoseconds } = inner;

    timestamp_counter.calibrate();
    let mut heap = ReadyMessageHeap::from_buffers(buffers);

    while let Some(node) = heap.pop() {
        let payload = unsafe { (*node.header()).payload_pointer() as *const u8 };

        // Read *const StaticInfo from the beginning of the payload
        let info: &StaticInfo = unsafe {
            let info_ptr =
                payload.add(PAYLOAD_INFO_OFFSET).cast::<*const StaticInfo>().read_unaligned();
            &*info_ptr
        };

        let timestamp_nanoseconds =
            timestamp_counter.timestamp_to_nanoseconds(node.raw_timestamp());
        let thread_name = unsafe { (*node.thread_buffer()).name_bytes() };
        unsafe {
            output.write_record(
                timestamp_nanoseconds,
                info.level,
                thread_name,
                info.location,
                |buffer| (info.decode)(payload.add(PAYLOAD_ARGS_OFFSET), buffer),
            )?;
        }

        if output.buffered_len() >= flush_buffer_size || info.level >= flush_level {
            output.flush()?;
            *next_flush_deadline_nanoseconds = None;
        }

        unsafe {
            let queue_pointer = &raw mut (*node.thread_buffer()).queue;
            SpscVarQueue::consumer_pop(queue_pointer);
        }
        heap.refresh_buffer(node.thread_buffer());
    }

    poller::cleanup_finished_buffers();

    if force_flush {
        output.flush()?;
        *next_flush_deadline_nanoseconds = None;
    } else if output.has_pending() {
        let now = timestamp_counter.read_nanoseconds();
        match *next_flush_deadline_nanoseconds {
            Some(deadline_nanoseconds) if now > deadline_nanoseconds => {
                output.flush()?;
                *next_flush_deadline_nanoseconds = None;
            }
            None => {
                *next_flush_deadline_nanoseconds = Some(now + flush_delay_nanoseconds);
            }
            _ => {}
        }
    } else {
        *next_flush_deadline_nanoseconds = None;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Queue-full handler (cold path — no OnceLock indirection)
// ---------------------------------------------------------------------------

/// Handle a queue-full event: increment the counter, invoke the callback
/// (if set), and return the configured action (Drop or Retry).
///
/// Reads directly from top-level statics — no `global_logger()` call.
pub(crate) fn handle_queue_full() -> QueueFullAction {
    let count = QUEUE_FULL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let ptr = QUEUE_FULL_CALLBACK.load(Ordering::Acquire);
    if !ptr.is_null() {
        // Safety: ptr was converted from a valid fn(u64) pointer by set_queue_full_callback
        let callback: QueueFullCallback = unsafe { std::mem::transmute(ptr) };
        callback(count);
    }

    match queue_full_policy_from_u8(QUEUE_FULL_POLICY.load(Ordering::Relaxed)) {
        QueueFullPolicy::Drop => QueueFullAction::Drop,
        QueueFullPolicy::Block => QueueFullAction::Retry,
    }
}

/// Convert a raw `u8` to a [`QueueFullPolicy`]. Any unrecognized value
/// defaults to [`QueueFullPolicy::Drop`].
#[inline]
pub(crate) fn queue_full_policy_from_u8(value: u8) -> QueueFullPolicy {
    match value {
        1 => QueueFullPolicy::Block,
        _ => QueueFullPolicy::Drop,
    }
}

use std::{
    cell::UnsafeCell,
    io::{self, Write},
    ops::{Deref, DerefMut},
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, Ordering},
    },
};

use crate::{
    Level, StaticHeaderWriteFn, level_from_u8,
    output::{OutputBuffer, RotationConfig},
    poller::{self, PollerState, ReadyMessageHeap},
    spsc_queue::{PAYLOAD_ARGS_OFFSET, PAYLOAD_INFO_OFFSET, PAYLOAD_SPAN_ID_OFFSET, SpscVarQueue},
    thread_buffer::{THREAD_BUFFER_REGISTRY, ThreadBuffer},
    timestamp_counter::TimestampCounter,
};

/// Function pointer type for decoding a log record's payload into a formatted buffer.
pub type DecodeFn = unsafe fn(*const u8, &mut Vec<u8>);

/// Callback function type invoked when the per-thread SPSC queue is full.
/// The argument is the cumulative count of queue-full events.
pub type QueueFullCallback = fn(u64);

/// Default log level (DBG = accept debug and above; trace stays opt-in).
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
    scratch_buffers: Vec<*mut ThreadBuffer>,
    scratch_heap: ReadyMessageHeap,
}

/// The global logger instance.
///
/// Only contains state that requires runtime initialization
/// ([`TimestampCounter`], [`OutputBuffer`]) and the poller state machine.
/// All configuration atomics live as top-level statics so the hot path
/// (`check_level`) never touches this struct.
struct GlobalLogger {
    inner: UnsafeCell<LoggerInner>,
    consumer_busy: AtomicBool,
    poller_state: PollerState,
}

// SAFETY: scratch pointers never escape consumer access under `inner`. They are
// metadata only, and each poll clears them before releasing the consumer guard.
unsafe impl Send for LoggerInner {}

// SAFETY: `inner` is only accessed through `ConsumerGuard`, which enforces exclusive
// access via `consumer_busy` with `Acquire`/`Release` ordering.
unsafe impl Sync for GlobalLogger {}

struct ConsumerGuard<'a> {
    logger: &'a GlobalLogger,
}

impl Deref for ConsumerGuard<'_> {
    type Target = LoggerInner;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `consumer_busy` guarantees exclusive access for the lifetime of this guard.
        unsafe { &*self.logger.inner.get() }
    }
}

impl DerefMut for ConsumerGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: `consumer_busy` guarantees exclusive access for the lifetime of this guard.
        unsafe { &mut *self.logger.inner.get() }
    }
}

impl Drop for ConsumerGuard<'_> {
    fn drop(&mut self) {
        self.logger.consumer_busy.store(false, Ordering::Release);
    }
}

impl GlobalLogger {
    fn new() -> Self {
        let mut timestamp_counter = TimestampCounter::new();
        timestamp_counter.init_default();
        Self {
            inner: UnsafeCell::new(LoggerInner {
                timestamp_counter,
                output: OutputBuffer::new(),
                next_flush_deadline_nanoseconds: None,
                scratch_buffers: Vec::new(),
                scratch_heap: ReadyMessageHeap::default(),
            }),
            consumer_busy: AtomicBool::new(false),
            poller_state: PollerState::default(),
        }
    }

    #[inline]
    fn acquire_consumer(&self) -> ConsumerGuard<'_> {
        if self
            .consumer_busy
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self
                .consumer_busy
                .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                std::hint::spin_loop();
                std::thread::yield_now();
            }
        }
        ConsumerGuard { logger: self }
    }

    fn configure<T>(&self, apply: impl FnOnce(&mut LoggerInner) -> io::Result<T>) -> io::Result<T> {
        self.poller_state.configure(|| {
            let mut inner = self.acquire_consumer();
            apply(&mut inner)
        })
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
    global_logger()
        .configure(|inner| {
            inner.output.set_writer(Box::new(writer));
            Ok(())
        })
        .expect("set_output_writer requires the polling thread to be stopped first");
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
    global_logger().configure(|inner| inner.output.set_log_file(path))
}

/// Sets a rotating destination while holding both lifecycle and consumer exclusion.
pub fn set_log_file_with_rotation(path: &str, config: RotationConfig) -> io::Result<()> {
    global_logger().configure(|inner| inner.output.set_log_file_with_rotation(path, config))
}

/// Changes the header only while the background poller is stopped.
pub fn set_header_pattern(pattern: &str) -> io::Result<()> {
    global_logger().configure(|inner| inner.output.set_header_pattern(pattern))
}

/// Installs a static header while the background poller is stopped.
pub fn set_static_header(write: StaticHeaderWriteFn) -> io::Result<()> {
    global_logger().configure(|inner| {
        inner.output.set_static_header(write);
        Ok(())
    })
}

pub(crate) fn start_configured_polling_thread(
    interval: i64,
    core: Option<usize>,
    configure: impl FnOnce(&mut OutputBuffer) -> io::Result<()>,
) -> io::Result<u64> {
    let logger = global_logger();
    logger.poller_state.start_configured(interval, poll, core, || {
        let mut inner = logger.acquire_consumer();
        poll_inner(
            &mut inner,
            FLUSH_BUFFER_SIZE.load(Ordering::Relaxed) as usize,
            level_from_u8(FLUSH_LEVEL.load(Ordering::Relaxed)),
            FLUSH_DELAY_NS.load(Ordering::Relaxed),
            true,
        )?;
        configure(&mut inner.output)
    })
}

pub(crate) fn shutdown_generation(generation: u64) -> io::Result<()> {
    global_logger().poller_state.stop_generation(generation, || poll(true)).map(|_| ())
}

pub(crate) fn cleanup_finished_buffers() {
    let _inner = global_logger().acquire_consumer();
    // SAFETY: every registry snapshot, consumer and reclamation holds this guard.
    unsafe { poller::cleanup_finished_buffers_exclusive() };
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

/// Drains currently published ready records using a serialized consumer.
///
/// Cross-thread timestamp merging is best effort on invariant-TSC systems. A
/// producer may publish an older record after another producer's record was emitted.
/// Concurrent manual/background polls serialize; producers never take this lock.
/// If `force_flush` is true, accepted output is flushed before returning success.
pub fn poll(force_flush: bool) -> io::Result<()> {
    let mut inner = global_logger().acquire_consumer();
    poll_inner(
        &mut inner,
        FLUSH_BUFFER_SIZE.load(Ordering::Relaxed) as usize,
        level_from_u8(FLUSH_LEVEL.load(Ordering::Relaxed)),
        FLUSH_DELAY_NS.load(Ordering::Relaxed),
        force_flush,
    )
}

struct PollScratch<'a> {
    buffers: &'a mut Vec<*mut ThreadBuffer>,
    heap: &'a mut ReadyMessageHeap,
}

impl Drop for PollScratch<'_> {
    fn drop(&mut self) {
        self.heap.clear();
        self.buffers.clear();
    }
}

fn poll_inner(
    inner: &mut LoggerInner,
    flush_buffer_size: usize,
    flush_level: Level,
    flush_delay_nanoseconds: i64,
    force_flush: bool,
) -> io::Result<()> {
    let LoggerInner {
        timestamp_counter,
        output,
        next_flush_deadline_nanoseconds,
        scratch_buffers,
        scratch_heap,
    } = inner;
    let scratch = PollScratch { buffers: scratch_buffers, heap: scratch_heap };
    scratch.buffers.clear();
    for index in 0..THREAD_BUFFER_REGISTRY.count() {
        let pointer = THREAD_BUFFER_REGISTRY.get(index);
        if !pointer.is_null() {
            scratch.buffers.push(pointer);
        }
    }
    let flush_after_drain = force_flush || output.is_flush_required();
    output.retry_pending()?;
    timestamp_counter.calibrate();
    // SAFETY: `inner` excludes every consumer and reclamation until scratch drops.
    unsafe { scratch.heap.clear_and_fill(scratch.buffers) };

    while let Some(node) = scratch.heap.pop() {
        // SAFETY: the consumer permit retains this acquired record until pop.
        let payload = unsafe {
            crate::spsc_queue::MessageHeader::payload_pointer_raw(node.header()) as *const u8
        };

        // Read *const StaticInfo from the beginning of the payload
        // SAFETY: the published metadata pointer names a static descriptor.
        let info: &StaticInfo = unsafe {
            let info_ptr =
                payload.add(PAYLOAD_INFO_OFFSET).cast::<*const StaticInfo>().read_unaligned();
            &*info_ptr
        };

        let timestamp_nanoseconds =
            timestamp_counter.timestamp_to_nanoseconds(node.raw_timestamp());
        // SAFETY: the published record includes the fixed span metadata.
        let span_id = unsafe { payload.add(PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().read_unaligned() };
        // SAFETY: the consumer permit keeps this buffer and name versions live.
        let thread_name = unsafe { (*node.thread_buffer()).name_bytes() };
        // SAFETY: the typed descriptor decodes its own acquired payload.
        unsafe {
            output.write_record(
                timestamp_nanoseconds,
                info.level,
                thread_name,
                info.location,
                span_id,
                |buffer| (info.decode)(payload.add(PAYLOAD_ARGS_OFFSET), buffer),
            )?;
        }

        // SAFETY: the acquired payload is fully decoded; the consumer permit
        // protects both cursor advancement and refreshing the live queue.
        // Ownership transfers to pending output once, even if flushing fails.
        unsafe {
            let queue_pointer = &raw mut (*node.thread_buffer()).queue;
            SpscVarQueue::consumer_pop(queue_pointer);
            scratch.heap.refresh_buffer(node.thread_buffer());
        }
        if output.buffered_len() >= flush_buffer_size
            || output.buffered_len() >= crate::output::MAX_PENDING_BYTES
            || info.level >= flush_level
        {
            output.flush()?;
            *next_flush_deadline_nanoseconds = None;
        }
    }
    drop(scratch);
    // SAFETY: consumer exclusion still covers the registry after scratch is cleared.
    unsafe { poller::cleanup_finished_buffers_exclusive() };

    if flush_after_drain {
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
                *next_flush_deadline_nanoseconds =
                    Some(now.saturating_add(flush_delay_nanoseconds));
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
        // SAFETY: ptr was converted from a valid fn(u64) pointer by set_queue_full_callback
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

#[cfg(test)]
mod tests {
    use std::{
        alloc::{GlobalAlloc, Layout, System},
        cell::Cell,
    };

    use super::*;

    std::thread_local! {
        static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) };
    }
    struct CountingAllocator;
    fn count_allocation() {
        let _ = ALLOCATIONS.try_with(|count| {
            if let Some(value) = count.get() {
                count.set(Some(value + 1));
            }
        });
    }
    // SAFETY: all allocations retain System's layouts, ownership and pointer contracts.
    unsafe impl GlobalAlloc for CountingAllocator {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count_allocation();
            // SAFETY: forwarded with the caller's unchanged allocation contract.
            unsafe { System.alloc(layout) }
        }
        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count_allocation();
            // SAFETY: forwarded with the caller's unchanged allocation contract.
            unsafe { System.alloc_zeroed(layout) }
        }
        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: System created the allocation under this exact layout.
            unsafe { System.dealloc(ptr, layout) }
        }
        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            count_allocation();
            // SAFETY: forwarded with the caller's unchanged reallocation contract.
            unsafe { System.realloc(ptr, layout, size) }
        }
    }
    #[global_allocator]
    static ALLOCATOR: CountingAllocator = CountingAllocator;

    fn publish_empty_body() {
        unsafe fn decode(_: *const u8, _: &mut Vec<u8>) {}
        static INFO: StaticInfo =
            StaticInfo { level: Level::INF, location: "scratch-test", format_string: "", decode };
        let size = crate::spsc_queue::PAYLOAD_HEADER_SIZE;
        let header = crate::thread_buffer::alloc_message(size as u32).unwrap();
        // SAFETY: this thread owns the reservation until Release publication.
        unsafe {
            let payload = crate::spsc_queue::MessageHeader::payload_pointer_raw(header);
            payload.add(PAYLOAD_INFO_OFFSET).cast::<*const StaticInfo>().write_unaligned(&INFO);
            payload
                .add(crate::spsc_queue::PAYLOAD_TIMESTAMP_OFFSET)
                .cast::<i64>()
                .write_unaligned(TimestampCounter::read_timestamp_counter());
            payload.add(PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().write_unaligned(0);
            (*header).push(size as u32);
        }
    }

    #[test]
    fn warmed_poll_reuses_registry_and_heap_storage() {
        set_output_writer(io::sink());
        set_header_pattern("").unwrap();
        publish_empty_body();
        poll(true).unwrap();
        for _ in 0..10 {
            publish_empty_body();
            ALLOCATIONS.set(Some(0));
            let result = poll(true);
            let allocations = ALLOCATIONS.replace(None).unwrap();
            result.unwrap();
            assert_eq!(allocations, 0, "warmed scalar consumer must reuse scratch");
        }
    }
    #[test]
    fn local_consumer_serializes_poll_and_configuration() {
        let logger = std::sync::Arc::new(GlobalLogger::new());
        logger
            .configure(|inner| {
                inner.output.set_writer(Box::new(io::sink()));
                Ok(())
            })
            .unwrap();
        let config_logger = logger.clone();
        let config = std::thread::spawn(move || {
            for _ in 0..if cfg!(miri) { 4 } else { 50 } {
                config_logger
                    .configure(|inner| {
                        inner.next_flush_deadline_nanoseconds = Some(123);
                        std::thread::yield_now();
                        assert_eq!(inner.next_flush_deadline_nanoseconds, Some(123));
                        Ok(())
                    })
                    .unwrap();
            }
        });
        let poll_logger = logger.clone();
        let poller = std::thread::spawn(move || {
            for _ in 0..if cfg!(miri) { 4 } else { 50 } {
                let mut inner = poll_logger.acquire_consumer();
                poll_inner(&mut inner, usize::MAX, Level::OFF, 0, true).unwrap();
                assert!(inner.scratch_buffers.is_empty());
                std::thread::yield_now();
                assert!(inner.next_flush_deadline_nanoseconds.is_none());
            }
        });
        config.join().unwrap();
        poller.join().unwrap();
        let mut inner = logger.acquire_consumer();
        assert!(inner.scratch_heap.pop().is_none());
    }

    #[test]
    fn consumer_and_lifecycle_guards_release_after_configuration_unwind() {
        let logger = GlobalLogger::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: io::Result<()> = logger.configure(|inner| {
                inner.next_flush_deadline_nanoseconds = Some(456);
                panic!("configuration panic");
            });
        }));
        assert!(result.is_err());
        logger
            .configure(|inner| {
                assert_eq!(inner.next_flush_deadline_nanoseconds, Some(456));
                inner.output.set_writer(Box::new(io::sink()));
                Ok(())
            })
            .unwrap();
        let mut inner = logger.acquire_consumer();
        poll_inner(&mut inner, usize::MAX, Level::OFF, 0, true).unwrap();
        assert!(inner.next_flush_deadline_nanoseconds.is_none());
    }
}

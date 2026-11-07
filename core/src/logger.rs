#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

use std::{
    cell::UnsafeCell,
    io::{self, Write},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU8, AtomicU32, Ordering},
    },
};

use uarch::prelude::*;

use crate::{
    Level, StaticHeaderWriteFn,
    contract::{
        ConsumerCore, ConsumerLease, DrainBudget, DrainFlags, DrainReport, FrozenSlab, LEVEL_GATE,
        LevelGateWord, QUEUE_FULL_COUNT, WakePolicy,
    },
    error::{ZeroLogError, io_error_to_zerolog_error},
    level_from_u8,
    output::{MAX_PENDING_BYTES, OutputBuffer, RotationConfig},
    poller::{self, HeapNode, PollerState, ReadyMessageHeap},
    spsc_queue::{
        MessageHeader, PAYLOAD_ARGS_OFFSET, PAYLOAD_INFO_OFFSET, PAYLOAD_SPAN_ID_OFFSET,
        SpscVarQueue,
    },
    thread_buffer::ThreadBuffer,
    timestamp_counter::TimestampCounter,
};

pub type DecodeFn = unsafe fn(*const u8, &mut Vec<u8>);
pub type QueueFullCallback = fn(u64);

const DEFAULT_LEVEL: u8 = Level::DBG as u8;
const DEFAULT_FLUSH_DELAY_NS: i64 = 3_000_000_000;
const DEFAULT_FLUSH_BUFFER_SIZE: u32 = 8 * 1024;
const DEFAULT_FLUSH_LEVEL: u8 = Level::OFF as u8;
const DEFAULT_QUEUE_FULL_POLICY: u8 = QueueFullPolicy::Drop as u8;

static CURRENT_LEVEL: AtomicU8 = AtomicU8::new(DEFAULT_LEVEL);
static FLUSH_DELAY_NS: AtomicI64 = AtomicI64::new(DEFAULT_FLUSH_DELAY_NS);
static FLUSH_BUFFER_SIZE: AtomicU32 = AtomicU32::new(DEFAULT_FLUSH_BUFFER_SIZE);
static FLUSH_LEVEL: AtomicU8 = AtomicU8::new(DEFAULT_FLUSH_LEVEL);
static QUEUE_FULL_POLICY: AtomicU8 = AtomicU8::new(DEFAULT_QUEUE_FULL_POLICY);
static QUEUE_FULL_CALLBACK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

const REENTRANT_CONSUMER: u8 = 1 << 0;
const REENTRANT_QUEUE_FULL_CB: u8 = 1 << 1;

#[thread_local]
static TLS_REENTRANCY: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };

struct ReentrancyGuard(u8);

impl Drop for ReentrancyGuard {
    #[inline]
    fn drop(&mut self) {
        TLS_REENTRANCY.set(TLS_REENTRANCY.get() & !self.0);
    }
}

/// Policy applied when a thread's SPSC log queue is full.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum QueueFullPolicy {
    Drop = 0,
    Block = 1,
}

pub(crate) enum QueueFullAction {
    Drop,
    Retry,
}

/// Compile-time metadata associated with a single log call site.
#[derive(Clone, Copy)]
pub struct StaticInfo {
    pub level: Level,
    pub location: &'static str,
    pub format_string: &'static str,
    pub decode: DecodeFn,
}

struct LoggerInner {
    timestamp_counter: TimestampCounter,
    output: OutputBuffer,
    next_flush_deadline_nanoseconds: Option<i64>,
    scratch_heap: ReadyMessageHeap,
}

struct GlobalLogger {
    inner: UnsafeCell<LoggerInner>,
    consumer_busy: AtomicBool,
    poller_state: PollerState,
}

// SAFETY: `LoggerInner` owns consumer-exclusive state protected by `consumer_busy`.
unsafe impl Send for LoggerInner {}

// SAFETY: `inner` is only accessed while holding `consumer_busy` with `Acquire`/`Release`.
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
        TLS_REENTRANCY.set(TLS_REENTRANCY.get() & !REENTRANT_CONSUMER);
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
                scratch_heap: ReadyMessageHeap::new(),
            }),
            consumer_busy: AtomicBool::new(false),
            poller_state: PollerState::default(),
        }
    }

    #[inline]
    fn try_acquire_consumer(&self) -> Option<ConsumerGuard<'_>> {
        let reentrancy = TLS_REENTRANCY.get();
        if (reentrancy & REENTRANT_CONSUMER) != 0 {
            return None;
        }
        while self
            .consumer_busy
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
            std::thread::yield_now();
        }
        TLS_REENTRANCY.set(reentrancy | REENTRANT_CONSUMER);
        Some(ConsumerGuard { logger: self })
    }

    #[inline]
    fn acquire_consumer(&self) -> ConsumerGuard<'_> {
        let Some(guard) = self.try_acquire_consumer() else {
            panic!("reentrant GlobalLogger::acquire_consumer on consumer thread");
        };
        guard
    }

    fn configure<T>(&self, apply: impl FnOnce(&mut LoggerInner) -> io::Result<T>) -> io::Result<T> {
        self.poller_state.configure(|| {
            let mut inner = self
                .try_acquire_consumer()
                .ok_or_else(|| io::Error::from(ZeroLogError::ConsumerBusy))?;
            apply(&mut inner)
        })
    }
}

fn global_logger() -> &'static GlobalLogger {
    static LOGGER: OnceLock<GlobalLogger> = OnceLock::new();
    LOGGER.get_or_init(GlobalLogger::new)
}

fn sync_level_gate(deep_park_override: Option<bool>) {
    let mut current = LEVEL_GATE.load(Ordering::Relaxed);
    loop {
        let min_level = CURRENT_LEVEL.load(Ordering::SeqCst);
        let flush_level = FLUSH_LEVEL.load(Ordering::SeqCst);
        let base_ring_level = if flush_level >= Level::OFF as u8 { 6 } else { flush_level };
        let word = LevelGateWord::new(current);
        let deep_park = deep_park_override.unwrap_or_else(|| word.test(LevelGateWord::DEEP_PARK));
        let ring_level = if deep_park { 0 } else { base_ring_level };
        let next_packed = LevelGateWord::pack(min_level, ring_level, deep_park);
        if current == next_packed {
            return;
        }
        match LEVEL_GATE.compare_exchange_weak(
            current,
            next_packed,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return,
            Err(observed) => current = observed,
        }
    }
}

#[doc(hidden)]
pub use crate::poller::set_membarrier_state_for_test;

static DEEP_PARK_TRANSITION_HOOK: AtomicPtr<()> = AtomicPtr::new(std::ptr::null_mut());

#[doc(hidden)]
pub fn set_deep_park_transition_hook(hook: Option<fn()>) {
    let ptr = hook.map_or(std::ptr::null_mut(), |f| f as *mut ());
    DEEP_PARK_TRANSITION_HOOK.store(ptr, Ordering::Release);
}

fn run_deep_park_transition_hook() {
    let ptr = DEEP_PARK_TRANSITION_HOOK.load(Ordering::Acquire);
    if !ptr.is_null() {
        // SAFETY: `ptr` was stored from a valid `fn()` pointer in `set_deep_park_transition_hook`.
        let hook: fn() = unsafe { std::mem::transmute(ptr) };
        hook();
    }
}

pub fn set_level(level: Level) {
    CURRENT_LEVEL.store(level as u8, Ordering::SeqCst);
    sync_level_gate(None);
}

#[hot_inline]
pub fn check_level(level: Level) -> bool {
    crate::thread_buffer::passes_level_gate(crate::thread_buffer::load_level_gate(), level as u8)
}

pub fn set_output_writer<W: Write + Send + 'static>(writer: W) {
    if global_logger()
        .configure(|inner| {
            inner.output.set_writer(Box::new(writer));
            Ok(())
        })
        .is_err()
    {
        panic!("set_output_writer requires the polling thread to be stopped first");
    }
}

pub fn set_queue_full_policy(policy: QueueFullPolicy) {
    QUEUE_FULL_POLICY.store(policy as u8, Ordering::Relaxed);
}

pub fn set_queue_full_callback(callback: Option<QueueFullCallback>) {
    let ptr = callback.map_or(std::ptr::null_mut(), |f| f as *mut ());
    QUEUE_FULL_CALLBACK.store(ptr, Ordering::Release);
}

pub fn queue_full_count() -> u64 {
    QUEUE_FULL_COUNT.load(Ordering::Relaxed)
}
pub fn reset_queue_full_count() {
    QUEUE_FULL_COUNT.store(0, Ordering::Relaxed);
}
pub fn set_flush_delay(nanoseconds: i64) {
    FLUSH_DELAY_NS.store(nanoseconds.max(0), Ordering::Relaxed);
}
pub fn set_flush_buffer_size(bytes: u32) {
    FLUSH_BUFFER_SIZE.store(bytes, Ordering::Relaxed);
}

pub fn flush_on(level: Level) {
    FLUSH_LEVEL.store(level as u8, Ordering::SeqCst);
    sync_level_gate(None);
}

pub fn set_flush_on(level: Level) {
    flush_on(level);
}
pub fn set_log_file(path: &str) -> io::Result<()> {
    global_logger().configure(|inner| inner.output.set_log_file(path))
}

pub fn set_log_file_with_rotation(path: &str, config: RotationConfig) -> io::Result<()> {
    global_logger().configure(|inner| inner.output.set_log_file_with_rotation(path, config))
}

pub fn set_header_pattern(pattern: &str) -> io::Result<()> {
    global_logger().configure(|inner| inner.output.set_header_pattern(pattern))
}

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
        let mut inner = logger
            .try_acquire_consumer()
            .ok_or_else(|| io::Error::from(ZeroLogError::ConsumerBusy))?;
        poll_inner(&mut inner, true)?;
        configure(&mut inner.output)
    })
}

pub(crate) fn shutdown_generation(generation: u64) -> io::Result<()> {
    global_logger().poller_state.stop_generation(generation, || poll(true)).map(|_| ())
}

pub(crate) fn cleanup_finished_buffers() {
    let Some(mut inner) = global_logger().try_acquire_consumer() else { return };
    // SAFETY: every registry snapshot, consumer and reclamation holds this guard.
    unsafe { poller::cleanup_finished_buffers_exclusive(&mut inner.scratch_heap) };
}

pub fn start_polling_thread(interval_nanoseconds: i64) -> io::Result<()> {
    global_logger().poller_state.start(interval_nanoseconds, crate::logger::poll)
}

pub fn start_polling_thread_on_core(interval_nanoseconds: i64, core_id: usize) -> io::Result<()> {
    global_logger().poller_state.start_on_core(
        interval_nanoseconds,
        crate::logger::poll,
        Some(core_id),
    )
}

pub fn stop_polling_thread() -> io::Result<()> {
    global_logger().poller_state.stop()
}

/// Format a resolved queue front record into `output` and return its [`Level`].
///
/// # Safety
/// `buffer_ptr` and `header` must refer to an acquired live front record under exclusive consumer access.
#[hot_inline]
unsafe fn format_front_record(
    timestamp_counter: &TimestampCounter,
    output: &mut OutputBuffer,
    node: HeapNode,
    buffer_ptr: NonNull<ThreadBuffer>,
    header: *const MessageHeader,
) -> io::Result<Level> {
    // SAFETY: acquired record remains valid until `consumer_pop`.
    let payload = unsafe { MessageHeader::payload_pointer_raw(header) as *const u8 };
    // SAFETY: payload header stores a valid static descriptor pointer.
    let info: &StaticInfo =
        unsafe { &*payload.add(PAYLOAD_INFO_OFFSET).cast::<*const StaticInfo>().read_unaligned() };
    let timestamp_ns = timestamp_counter.timestamp_to_nanoseconds(node.raw_timestamp());
    // SAFETY: payload header contains an 8-byte span id.
    let span_id = unsafe { payload.add(PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().read_unaligned() };
    // SAFETY: buffer and its immutable name chain live until reclamation.
    let thread_name = unsafe { (*buffer_ptr.as_ptr()).name_bytes() };
    // SAFETY: `info.decode` matches the payload encoded by this call site.
    unsafe {
        output.write_record(
            timestamp_ns,
            info.level,
            thread_name,
            info.location,
            span_id,
            |buf| (info.decode)(payload.add(PAYLOAD_ARGS_OFFSET), buf),
        )?;
    }
    Ok(info.level)
}

/// Sans-IO log consumer holding an exclusive [`ConsumerLease`].
pub struct LogConsumer {
    lease: Option<ConsumerLease>,
    wake_policy: WakePolicy,
    current_interval_ns: u64,
    idle_since_ns: Option<i64>,
    frozen_in_flight: bool,
    last_error: Option<ZeroLogError>,
}

impl LogConsumer {
    pub fn generation(&self) -> u32 {
        self.lease.as_ref().map_or(0, ConsumerLease::generation)
    }
    pub const fn wake_policy(&self) -> WakePolicy {
        self.wake_policy
    }
    pub const fn last_error(&self) -> Option<ZeroLogError> {
        self.last_error
    }

    pub fn read_nanoseconds(&self) -> i64 {
        global_logger().acquire_consumer().timestamp_counter.read_nanoseconds()
    }

    fn release_external_consumer(&mut self) {
        let logger = global_logger();
        if let Some(mut inner) = logger.try_acquire_consumer() {
            inner.output.set_external_consumer(false);
        }
        logger.poller_state.release_external_lease();
        sync_level_gate(Some(false));
    }

    pub fn poison_on_abandon(mut self) {
        self.release_external_consumer();
        if let Some(lease) = self.lease.take() {
            lease.poison_on_abandon();
        }
        core::mem::forget(self);
    }

    fn compute_next_deadline(
        &mut self,
        now_ns: i64,
        drained: u32,
        state: (&mut bool, &mut bool, Option<i64>),
        heap: &mut ReadyMessageHeap,
    ) -> i64 {
        let (more_work, idle, flush_deadline) = state;
        if *more_work {
            sync_level_gate(Some(false));
            self.idle_since_ns = None;
            return now_ns;
        }
        let poll_deadline = match self.wake_policy {
            WakePolicy::Fixed { interval_ns } => {
                sync_level_gate(Some(false));
                now_ns.saturating_add(interval_ns.max(1).min(i64::MAX as u64) as i64)
            }
            WakePolicy::AdaptiveTimer { min_interval_ns, max_idle_interval_ns } => {
                sync_level_gate(Some(false));
                let min_ns = min_interval_ns.max(1);
                let max_ns = max_idle_interval_ns.max(min_ns);
                self.current_interval_ns = if drained > 0 {
                    min_ns
                } else {
                    self.current_interval_ns.saturating_mul(2).min(max_ns)
                };
                now_ns.saturating_add(self.current_interval_ns.min(i64::MAX as u64) as i64)
            }
            WakePolicy::DeepPark { deep_park_after_ns } => {
                if drained > 0 || !*idle {
                    self.idle_since_ns = None;
                    sync_level_gate(Some(false));
                    now_ns.saturating_add(1_000_000)
                } else {
                    let start = *self.idle_since_ns.get_or_insert(now_ns);
                    let elapsed = now_ns.saturating_sub(start).max(0) as u64;
                    if elapsed < deep_park_after_ns {
                        sync_level_gate(Some(false));
                        let rem = (deep_park_after_ns - elapsed).max(1).min(i64::MAX as u64) as i64;
                        now_ns.saturating_add(rem)
                    } else if poller::membarrier_state() == 2 {
                        sync_level_gate(Some(false));
                        now_ns.saturating_add(100_000_000)
                    } else {
                        run_deep_park_transition_hook();
                        sync_level_gate(Some(true));
                        let barrier_ok = poller::membarrier_private_expedited();
                        // SAFETY: caller holds exclusive `ConsumerGuard` on `LoggerInner`.
                        unsafe { heap.fill_from_registry() };
                        if !heap.is_empty() {
                            sync_level_gate(Some(false));
                            self.idle_since_ns = None;
                            *more_work = true;
                            *idle = false;
                            return now_ns;
                        }
                        if barrier_ok {
                            i64::MAX
                        } else {
                            sync_level_gate(Some(false));
                            now_ns.saturating_add(100_000_000)
                        }
                    }
                }
            }
        };
        flush_deadline.map_or(poll_deadline, |deadline| poll_deadline.min(deadline))
    }
}

impl Drop for LogConsumer {
    fn drop(&mut self) {
        self.release_external_consumer();
        drop(self.lease.take());
    }
}

impl ConsumerCore for LogConsumer {
    fn drain(&mut self, now_ns: i64, budget: DrainBudget) -> DrainReport {
        let flush_buffer_size = FLUSH_BUFFER_SIZE.load(Ordering::Relaxed) as usize;
        let flush_level = level_from_u8(FLUSH_LEVEL.load(Ordering::Relaxed));
        let flush_delay_ns = FLUSH_DELAY_NS.load(Ordering::Relaxed);
        let frozen_in_flight = self.frozen_in_flight;

        let mut inner = global_logger().acquire_consumer();
        let LoggerInner {
            timestamp_counter,
            output,
            next_flush_deadline_nanoseconds,
            scratch_heap,
        } = &mut *inner;
        // SAFETY: `inner` holds exclusive consumer access.
        unsafe { scratch_heap.fill_from_registry() };

        let (mut drained_records, mut drained_bytes, mut urgent_flush) = (0u32, 0usize, false);
        let mut backpressure = output.total_unflushed_bytes() >= MAX_PENDING_BYTES;

        while drained_records < budget.max_records && drained_bytes < budget.max_bytes as usize {
            if output.total_unflushed_bytes() >= MAX_PENDING_BYTES {
                backpressure = true;
                break;
            }
            let Some(node) = scratch_heap.pop() else { break };
            // SAFETY: `inner` holds exclusive consumer access to all registered buffers.
            let Some((buffer_ptr, header)) = (unsafe { scratch_heap.resolve_front(node.cursor) })
            else {
                continue;
            };
            let before_len = output.buffered_len();
            // SAFETY: `buffer_ptr` and `header` are live under exclusive consumer access.
            let write_res =
                unsafe { format_front_record(timestamp_counter, output, node, buffer_ptr, header) };
            let Ok(level) = write_res else {
                scratch_heap.reinsert(node);
                backpressure = true;
                break;
            };
            drained_bytes =
                drained_bytes.saturating_add(output.buffered_len().saturating_sub(before_len));
            drained_records += 1;
            urgent_flush |= level >= flush_level;
            // SAFETY: record has been formatted into `output`; advance queue and refresh slot.
            unsafe {
                SpscVarQueue::consumer_pop(&raw mut (*buffer_ptr.as_ptr()).queue);
                scratch_heap.refresh_cursor(node.cursor);
            }
        }

        // SAFETY: `inner` holds exclusive consumer access; slots in `scratch_heap` are retained.
        unsafe { poller::cleanup_finished_buffers_exclusive(scratch_heap) };

        let mut more_work = !scratch_heap.is_empty();
        let buffered_len = output.buffered_len();
        let output_ready = buffered_len > 0;
        if output_ready {
            if next_flush_deadline_nanoseconds.is_none() {
                *next_flush_deadline_nanoseconds = Some(now_ns.saturating_add(flush_delay_ns));
            }
        } else if !frozen_in_flight {
            *next_flush_deadline_nanoseconds = None;
        }
        let flush_deadline = *next_flush_deadline_nanoseconds;
        let deadline_due =
            output_ready && flush_deadline.is_some_and(|deadline| now_ns >= deadline);
        let flush_due = urgent_flush
            || buffered_len >= flush_buffer_size
            || output.is_flush_required()
            || deadline_due;
        let backpressure_flag = backpressure
            || output.total_unflushed_bytes() >= MAX_PENDING_BYTES
            || output.is_flush_required();
        let mut idle = drained_records == 0 && !more_work && !output_ready && !frozen_in_flight;

        let next_deadline_ns = self.compute_next_deadline(
            now_ns,
            drained_records,
            (&mut more_work, &mut idle, flush_deadline),
            scratch_heap,
        );
        drop(inner);
        let flags = DrainFlags::new(0).patch(
            BitPatch::EMPTY
                .with_flag(DrainFlags::MORE_WORK, more_work)
                .with_flag(DrainFlags::OUTPUT_READY, output_ready)
                .with_flag(DrainFlags::FLUSH_DUE, flush_due)
                .with_flag(DrainFlags::IDLE, idle)
                .with_flag(DrainFlags::BACKPRESSURE, backpressure_flag)
                .with_field(DrainFlags::DRAINED, drained_records as u64),
        );
        DrainReport { next_deadline_ns, flags }
    }

    fn freeze_output(&mut self) -> Option<FrozenSlab> {
        let mut inner = global_logger().acquire_consumer();
        let slab = inner.output.freeze_pending()?;
        if inner.output.buffered_len() == 0 {
            inner.next_flush_deadline_nanoseconds = None;
        }
        self.frozen_in_flight = true;
        Some(slab)
    }

    fn complete_output(&mut self, slab: FrozenSlab, written: u32) -> Option<FrozenSlab> {
        let remaining = global_logger().acquire_consumer().output.complete_frozen(slab, written);
        if remaining.is_none() {
            self.frozen_in_flight = false;
        }
        remaining
    }

    fn write_failed(&mut self, slab: FrozenSlab, error: ZeroLogError) {
        global_logger().acquire_consumer().output.restore_failed_slab(slab);
        self.frozen_in_flight = false;
        self.last_error = Some(error);
    }

    fn has_unflushed(&self) -> bool {
        self.frozen_in_flight || global_logger().acquire_consumer().output.has_pending()
    }

    fn flush_sync(&mut self) -> Result<(), ZeroLogError> {
        let mut inner = global_logger().acquire_consumer();
        inner.output.flush().map_err(io_error_to_zerolog_error)?;
        inner.next_flush_deadline_nanoseconds = None;
        Ok(())
    }
}

/// Acquires the exclusive Sans-IO [`LogConsumer`] with the specified [`WakePolicy`].
pub fn acquire_consumer(wake_policy: WakePolicy) -> Result<LogConsumer, ZeroLogError> {
    let logger = global_logger();
    let lease = logger.poller_state.try_acquire_external_lease()?;
    {
        let mut inner = logger.acquire_consumer();
        inner.output.set_external_consumer(true);
        inner.timestamp_counter.calibrate();
    }
    let initial_interval = match wake_policy {
        WakePolicy::AdaptiveTimer { min_interval_ns, .. } => min_interval_ns.max(1),
        WakePolicy::Fixed { interval_ns } => interval_ns.max(1),
        WakePolicy::DeepPark { .. } => 1_000_000,
    };
    sync_level_gate(Some(false));
    Ok(LogConsumer {
        lease: Some(lease),
        wake_policy,
        current_interval_ns: initial_interval,
        idle_since_ns: None,
        frozen_in_flight: false,
        last_error: None,
    })
}

/// Drains up to `budget` records/bytes in a single lease-guarded batch.
pub fn poll_batch(budget: DrainBudget, force_flush: bool) -> Result<DrainReport, ZeroLogError> {
    let mut consumer = acquire_consumer(WakePolicy::default())?;
    let mut report = consumer.drain(consumer.read_nanoseconds(), budget);
    if force_flush || report.flags.test(DrainFlags::FLUSH_DUE) {
        consumer.flush_sync()?;
        let has_more_output = global_logger().acquire_consumer().output.buffered_len() > 0;
        let patch = BitPatch::EMPTY
            .with_flag(DrainFlags::OUTPUT_READY, has_more_output)
            .with_flag(DrainFlags::FLUSH_DUE, false);
        report.flags = report.flags.patch(patch);
    }
    Ok(report)
}

pub fn poll(force_flush: bool) -> io::Result<()> {
    let logger = global_logger();
    if logger.poller_state.has_external_lease() {
        return Err(io::Error::from(ZeroLogError::ConsumerBusy));
    }
    let Some(mut inner) = logger.try_acquire_consumer() else {
        return Err(io::Error::from(ZeroLogError::ConsumerBusy));
    };
    if logger.poller_state.has_external_lease() {
        return Err(io::Error::from(ZeroLogError::ConsumerBusy));
    }
    poll_inner(&mut inner, force_flush)
}

fn poll_inner(inner: &mut LoggerInner, force_flush: bool) -> io::Result<()> {
    let flush_buffer_size = FLUSH_BUFFER_SIZE.load(Ordering::Relaxed) as usize;
    let flush_level = level_from_u8(FLUSH_LEVEL.load(Ordering::Relaxed));
    let flush_delay_nanoseconds = FLUSH_DELAY_NS.load(Ordering::Relaxed);
    let LoggerInner { timestamp_counter, output, next_flush_deadline_nanoseconds, scratch_heap } =
        inner;
    let flush_after_drain = force_flush || output.is_flush_required();
    output.retry_pending()?;
    timestamp_counter.calibrate();
    scratch_heap.clear();
    // SAFETY: `inner` excludes every consumer and reclamation while `scratch_heap` is populated.
    unsafe { scratch_heap.fill_from_registry() };

    while let Some(node) = scratch_heap.pop() {
        // SAFETY: `inner` holds exclusive consumer access to all registered buffers.
        let Some((buffer_ptr, header)) = (unsafe { scratch_heap.resolve_front(node.cursor) })
        else {
            continue;
        };
        // SAFETY: `buffer_ptr` and `header` are live under exclusive consumer access.
        let level =
            unsafe { format_front_record(timestamp_counter, output, node, buffer_ptr, header)? };
        // SAFETY: the acquired payload is decoded; advance cursor and refresh live queue.
        unsafe {
            SpscVarQueue::consumer_pop(&raw mut (*buffer_ptr.as_ptr()).queue);
            scratch_heap.refresh_cursor(node.cursor);
        }
        if output.buffered_len() >= flush_buffer_size
            || output.buffered_len() >= MAX_PENDING_BYTES
            || level >= flush_level
        {
            if let Err(err) = output.flush() {
                scratch_heap.clear();
                return Err(err);
            }
            *next_flush_deadline_nanoseconds = None;
        }
    }
    scratch_heap.clear();
    // SAFETY: consumer exclusion still covers the registry after `scratch_heap` is cleared.
    unsafe { poller::cleanup_finished_buffers_exclusive(scratch_heap) };

    if flush_after_drain {
        output.flush()?;
        *next_flush_deadline_nanoseconds = None;
    } else if output.has_pending() {
        let now = timestamp_counter.read_nanoseconds();
        match *next_flush_deadline_nanoseconds {
            Some(deadline) if now > deadline => {
                output.flush()?;
                *next_flush_deadline_nanoseconds = None;
            }
            None => {
                *next_flush_deadline_nanoseconds = Some(now.saturating_add(flush_delay_nanoseconds))
            }
            _ => {}
        }
    } else {
        *next_flush_deadline_nanoseconds = None;
    }
    Ok(())
}

pub(crate) fn handle_queue_full() -> QueueFullAction {
    let count = QUEUE_FULL_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
    let reentrancy = TLS_REENTRANCY.get();
    if (reentrancy & REENTRANT_QUEUE_FULL_CB) != 0 {
        return QueueFullAction::Drop;
    }
    let ptr = QUEUE_FULL_CALLBACK.load(Ordering::Acquire);
    if !ptr.is_null() {
        TLS_REENTRANCY.set(reentrancy | REENTRANT_QUEUE_FULL_CB);
        let _guard = ReentrancyGuard(REENTRANT_QUEUE_FULL_CB);
        // SAFETY: ptr was converted from a valid fn(u64) pointer by set_queue_full_callback.
        let callback: QueueFullCallback = unsafe { std::mem::transmute(ptr) };
        callback(count);
    }
    if (reentrancy & REENTRANT_CONSUMER) != 0 {
        return QueueFullAction::Drop;
    }
    match queue_full_policy_from_u8(QUEUE_FULL_POLICY.load(Ordering::Relaxed)) {
        QueueFullPolicy::Drop => QueueFullAction::Drop,
        QueueFullPolicy::Block => QueueFullAction::Retry,
    }
}

#[inline]
pub(crate) fn queue_full_policy_from_u8(value: u8) -> QueueFullPolicy {
    if value == 1 { QueueFullPolicy::Block } else { QueueFullPolicy::Drop }
}

#[cfg(test)]
#[path = "logger_tests.rs"]
mod tests;

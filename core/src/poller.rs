use std::{
    cell::UnsafeCell,
    cmp::Ordering,
    collections::BinaryHeap,
    io,
    ops::{Deref, DerefMut},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering as AtomicOrdering},
    },
    thread::{self, JoinHandle, ThreadId},
    time::Duration,
};

use crate::{
    spsc_queue::{MessageHeader, PAYLOAD_TIMESTAMP_OFFSET, SpscVarQueue},
    thread_buffer::{THREAD_BUFFER_REGISTRY, ThreadBuffer},
};

const WORKER_WAIT_START: u8 = 0;
const WORKER_ABORT: u8 = 1;
const WORKER_RUNNING: u8 = 2;
const WORKER_STOP_AND_FLUSH: u8 = 3;
const WORKER_HANDOFF: u8 = 4;

/// Internal control handle for managing the poller thread lifecycle.
struct PollerControl {
    stop_mode: Arc<AtomicU8>,
    handle: JoinHandle<io::Result<()>>,
}

/// Owns a spawned worker until configuration and handoff have committed.
struct PendingWorker {
    control: Option<PollerControl>,
}

impl PendingWorker {
    fn commit(mut self) -> PollerControl {
        self.control.take().expect("pending worker is committed only once")
    }
}

impl Drop for PendingWorker {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            control.stop_mode.store(WORKER_ABORT, AtomicOrdering::Release);
            control.handle.thread().unpark();
            let _ = control.handle.join();
        }
    }
}

#[derive(Default)]
struct PollerLifecycle {
    control: Option<PollerControl>,
    generation: u64,
    stopping: bool,
    worker_id: Option<ThreadId>,
}

/// Serializes background-worker publication, configuration and joined shutdown.
pub struct PollerState {
    lifecycle: UnsafeCell<PollerLifecycle>,
    lock: AtomicBool,
    running: AtomicBool,
}

// SAFETY: `lifecycle` is accessed exclusively while holding `LifecycleGuard`,
// which synchronizes via `lock` with `Acquire`/`Release` ordering.
unsafe impl Sync for PollerState {}

struct LifecycleGuard<'a> {
    state: &'a PollerState,
}

impl Deref for LifecycleGuard<'_> {
    type Target = PollerLifecycle;

    fn deref(&self) -> &Self::Target {
        // SAFETY: `self.state.lock` is held exclusively for the lifetime of this guard.
        unsafe { &*self.state.lifecycle.get() }
    }
}

impl DerefMut for LifecycleGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: `self.state.lock` is held exclusively for the lifetime of this guard.
        unsafe { &mut *self.state.lifecycle.get() }
    }
}

impl Drop for LifecycleGuard<'_> {
    fn drop(&mut self) {
        self.state.lock.store(false, AtomicOrdering::Release);
    }
}

impl Default for PollerState {
    fn default() -> Self {
        Self {
            lifecycle: UnsafeCell::new(PollerLifecycle::default()),
            lock: AtomicBool::new(false),
            running: AtomicBool::new(false),
        }
    }
}

/// A node in the ready-message min-heap, representing the front message
/// of a particular thread buffer.
#[derive(Clone, Copy)]
pub struct HeapNode {
    raw_timestamp: i64,
    thread_buffer: *mut ThreadBuffer,
    header: *const MessageHeader,
}

impl HeapNode {
    /// Returns the raw timestamp of the message.
    #[inline]
    pub fn raw_timestamp(self) -> i64 {
        self.raw_timestamp
    }

    /// Returns a raw pointer to the originating thread buffer.
    #[inline]
    pub fn thread_buffer(self) -> *mut ThreadBuffer {
        self.thread_buffer
    }

    /// Returns a raw pointer to the message header.
    #[inline]
    pub fn header(self) -> *const MessageHeader {
        self.header
    }
}

impl Eq for HeapNode {}

impl PartialEq for HeapNode {
    fn eq(&self, other: &Self) -> bool {
        self.raw_timestamp == other.raw_timestamp
            && self.thread_buffer == other.thread_buffer
            && self.header == other.header
    }
}

impl Ord for HeapNode {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .raw_timestamp
            .cmp(&self.raw_timestamp)
            .then_with(|| (other.thread_buffer as usize).cmp(&(self.thread_buffer as usize)))
    }
}

impl PartialOrd for HeapNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// Merges currently published queue fronts by raw timestamp.
///
/// This is best-effort cross-thread ordering on invariant-TSC systems, not a
/// global watermark: a paused producer can publish an older record later.
#[derive(Default)]
pub struct ReadyMessageHeap {
    heap: BinaryHeap<HeapNode>,
}

impl ReadyMessageHeap {
    /// Collects the current front of each buffer.
    ///
    /// # Safety
    /// Every non-null pointer must reference a live `ThreadBuffer` under
    /// exclusive consumer access until this heap is cleared or dropped.
    pub unsafe fn from_buffers(buffers: &[*mut ThreadBuffer]) -> Self {
        let mut heap = Self::default();
        // SAFETY: the caller provides live buffers and exclusive consumer access.
        unsafe { heap.clear_and_fill(buffers) };
        heap
    }

    /// Reuses the heap's allocation to collect currently published fronts.
    ///
    /// # Safety
    /// The same lifetime and exclusive-consumer requirements as `from_buffers`
    /// apply to every non-null pointer and every subsequent heap operation.
    pub unsafe fn clear_and_fill(&mut self, buffers: &[*mut ThreadBuffer]) {
        self.heap.clear();
        for &buffer in buffers {
            push_front_message(&mut self.heap, buffer);
        }
    }

    pub(crate) fn clear(&mut self) {
        self.heap.clear();
    }

    /// Pops the earliest currently available front.
    pub fn pop(&mut self) -> Option<HeapNode> {
        self.heap.pop()
    }

    /// Refreshes a queue front after consuming its prior record.
    ///
    /// # Safety
    /// The pointer must be null or reference a live `ThreadBuffer` under
    /// exclusive consumer access until the resulting heap node is removed.
    pub unsafe fn refresh_buffer(&mut self, buffer: *mut ThreadBuffer) {
        push_front_message(&mut self.heap, buffer);
    }
}

/// Attempts to pin the calling thread to the given CPU core.
/// On Linux, uses `sched_setaffinity`. On other platforms, this is a no-op.
fn pin_current_thread_to_core(core_id: usize) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use libc::{CPU_SET, CPU_ZERO, cpu_set_t, sched_setaffinity};
        // SAFETY: `cpu_set_t` is a plain-old-data bitmask.  `CPU_ZERO` and
        // `CPU_SET` write to the caller-owned `cpuset` value on the stack.
        // `sched_setaffinity(0, ...)` operates on the calling thread only.
        // All pointers passed are valid and properly aligned.
        unsafe {
            let mut cpuset = std::mem::zeroed::<cpu_set_t>();
            CPU_ZERO(&mut cpuset);
            CPU_SET(core_id, &mut cpuset);
            if sched_setaffinity(0, std::mem::size_of::<cpu_set_t>(), &cpuset) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = core_id;
        Ok(())
    }
}

impl PollerState {
    /// Reports an installed worker, including the period before its join completes.
    pub fn is_running(&self) -> bool {
        self.running.load(AtomicOrdering::Acquire)
    }

    fn lock_lifecycle(&self) -> LifecycleGuard<'_> {
        if self
            .lock
            .compare_exchange(false, true, AtomicOrdering::Acquire, AtomicOrdering::Relaxed)
            .is_err()
        {
            while self
                .lock
                .compare_exchange_weak(
                    false,
                    true,
                    AtomicOrdering::Acquire,
                    AtomicOrdering::Relaxed,
                )
                .is_err()
            {
                std::hint::spin_loop();
                thread::yield_now();
            }
        }
        LifecycleGuard { state: self }
    }

    fn stable(&self) -> io::Result<LifecycleGuard<'_>> {
        loop {
            let state = self.lock_lifecycle();
            if state.worker_id == Some(thread::current().id()) {
                return Err(io::Error::other("poller lifecycle cannot be changed by its worker"));
            }
            if !state.stopping {
                return Ok(state);
            }
            drop(state);
            std::hint::spin_loop();
            thread::yield_now();
        }
    }

    fn join_worker<'a>(&'a self, state: LifecycleGuard<'a>) -> io::Result<LifecycleGuard<'a>> {
        self.join_worker_with_mode(state, WORKER_STOP_AND_FLUSH)
    }

    fn join_worker_with_mode<'a>(
        &'a self,
        mut state: LifecycleGuard<'a>,
        stop_mode: u8,
    ) -> io::Result<LifecycleGuard<'a>> {
        let Some(control) = state.control.take() else { return Ok(state) };
        state.stopping = true;
        control.stop_mode.store(stop_mode, AtomicOrdering::Release);
        control.handle.thread().unpark();
        drop(state);
        let result = control
            .handle
            .join()
            .map_err(|_| io::Error::other("zerolog polling thread panicked"))
            .and_then(|result| result);
        let mut state = self.lock_lifecycle();
        state.stopping = false;
        state.worker_id = None;
        self.running.store(false, AtomicOrdering::Release);
        result?;
        Ok(state)
    }

    pub(crate) fn configure<T>(&self, apply: impl FnOnce() -> io::Result<T>) -> io::Result<T> {
        let state = self.stable()?;
        if state.control.is_some() {
            return Err(io::Error::other(
                "polling thread is running; stop it before configuration",
            ));
        }
        // Keep the lifecycle guard alive through configuration, excluding starts.
        let result = apply();
        drop(state);
        result
    }

    /// Starts a worker, joining any previous worker first.
    pub fn start<F>(&self, interval_nanoseconds: i64, poll_fn: F) -> io::Result<()>
    where
        F: Fn(bool) -> io::Result<()> + Send + 'static,
    {
        self.start_on_core(interval_nanoseconds, poll_fn, None)
    }

    /// Starts a worker with optional CPU affinity. Publication follows spawn and setup.
    pub fn start_on_core<F>(
        &self,
        interval_nanoseconds: i64,
        poll_fn: F,
        core_id: Option<usize>,
    ) -> io::Result<()>
    where
        F: Fn(bool) -> io::Result<()> + Send + 'static,
    {
        self.start_configured(interval_nanoseconds, poll_fn, core_id, || Ok(())).map(|_| ())
    }

    pub(crate) fn start_configured<F>(
        &self,
        interval_nanoseconds: i64,
        poll_fn: F,
        core_id: Option<usize>,
        configure: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<u64>
    where
        F: Fn(bool) -> io::Result<()> + Send + 'static,
    {
        let state = self.stable()?;
        let generation = state
            .generation
            .checked_add(1)
            .ok_or_else(|| io::Error::other("poller generation exhausted"))?;
        let stop_mode = Arc::new(AtomicU8::new(WORKER_WAIT_START));
        let worker_stop = Arc::clone(&stop_mode);
        let interval = poll_interval(interval_nanoseconds);
        let handle = thread::Builder::new().name("zerolog-poller".into()).spawn(move || {
            // No polling or final flush if spawning/configuration is aborted.
            loop {
                match worker_stop.load(AtomicOrdering::Acquire) {
                    WORKER_WAIT_START => thread::park(),
                    WORKER_ABORT => return Ok(()),
                    _ => break,
                }
            }
            if let Some(core) = core_id {
                let _ = pin_current_thread_to_core(core);
            }
            while worker_stop.load(AtomicOrdering::Acquire) == WORKER_RUNNING {
                // Recoverable writer errors are retried; the final flush error is returned by stop.
                let _ = poll_fn(false);
                thread::park_timeout(interval);
            }
            if worker_stop.load(AtomicOrdering::Acquire) == WORKER_STOP_AND_FLUSH {
                poll_fn(true)
            } else {
                Ok(())
            }
        })?;
        let pending = PendingWorker { control: Some(PollerControl { stop_mode, handle }) };
        configure()?;
        let mut state = self.join_worker_with_mode(state, WORKER_HANDOFF)?;
        let control = pending.commit();
        state.worker_id = Some(control.handle.thread().id());
        state.control = Some(control);
        state.generation = generation;
        // Publish installed state before permitting the worker's first callback.
        self.running.store(true, AtomicOrdering::Release);
        let control = state.control.as_ref().expect("worker was just installed");
        control.stop_mode.store(WORKER_RUNNING, AtomicOrdering::Release);
        control.handle.thread().unpark();
        Ok(generation)
    }

    /// Waits for every in-progress stop and joins the current worker before returning.
    pub fn stop(&self) -> io::Result<()> {
        drop(self.join_worker(self.stable()?)?);
        Ok(())
    }

    pub(crate) fn stop_generation(
        &self,
        generation: u64,
        finish: impl FnOnce() -> io::Result<()>,
    ) -> io::Result<bool> {
        let state = self.stable()?;
        if state.generation != generation || state.control.is_none() {
            return Ok(false);
        }
        let state = self.join_worker(state)?;
        // Exclude a newer generation until the matching guard's final drain completes.
        let result = finish();
        drop(state);
        result.map(|_| true)
    }
}

/// Scans the thread buffer registry and deallocates any buffers that have
/// been marked for deallocation and are fully drained.
pub fn cleanup_finished_buffers() {
    crate::logger::cleanup_finished_buffers();
}

/// # Safety
/// The caller must hold the global logger consumer lock.
pub(crate) unsafe fn cleanup_finished_buffers_exclusive() {
    for index in 0..THREAD_BUFFER_REGISTRY.count() {
        let pointer = THREAD_BUFFER_REGISTRY.get(index);
        if pointer.is_null() {
            continue;
        }

        // SAFETY: the caller holds exclusive consumer/reclamation access and
        // every non-null registry slot contains a live initialized buffer.
        let should_deallocate =
            unsafe { (*pointer).should_deallocate.load(AtomicOrdering::Acquire) };
        // SAFETY: the same consumer permit protects this live queue cursor.
        let is_empty = unsafe {
            let queue_pointer = &raw const (*pointer).queue;
            SpscVarQueue::consumer_front(queue_pointer).is_none()
        };

        if should_deallocate && is_empty {
            // SAFETY: retirement was acquired and the drained buffer has no
            // remaining producer or consumer payload accesses.
            unsafe {
                ThreadBuffer::free_on_heap(pointer);
            }
            THREAD_BUFFER_REGISTRY.clear_slot(index);
        }
    }
}

/// Reads the front message from the given thread buffer's SPSC queue and
/// pushes it into the heap. Does nothing if the buffer is null or empty.
fn push_front_message(heap: &mut BinaryHeap<HeapNode>, buffer: *mut ThreadBuffer) {
    if buffer.is_null() {
        return;
    }

    // SAFETY: callers maintain exclusive consumer ownership of live buffers.
    let header = unsafe {
        let queue_pointer = &raw const (*buffer).queue;
        SpscVarQueue::consumer_front(queue_pointer)
    };

    let Some(header) = header else {
        return;
    };

    // SAFETY: consumer_front acquired publication; metadata remains live
    // until the unique consumer pops this record.
    let raw_timestamp = unsafe {
        let payload = crate::spsc_queue::MessageHeader::payload_pointer_raw(header) as *const u8;

        #[cfg(not(miri))]
        {
            // Prefetch only affects native cache behavior; interpretation skips it.
            core::intrinsics::prefetch_read_data::<u8, 2>(payload);
            let info_ptr = payload
                .add(crate::spsc_queue::PAYLOAD_INFO_OFFSET)
                .cast::<*const u8>()
                .read_unaligned();
            if !info_ptr.is_null() {
                core::intrinsics::prefetch_read_data::<u8, 2>(info_ptr);
            }
        }

        payload.add(PAYLOAD_TIMESTAMP_OFFSET).cast::<i64>().read_unaligned()
    };

    heap.push(HeapNode { raw_timestamp, thread_buffer: buffer, header });
}

/// Converts the given interval in nanoseconds to a [`Duration`], clamping
/// non-positive values to a default of 1 ms.
fn poll_interval(interval_nanoseconds: i64) -> Duration {
    let nanoseconds =
        if interval_nanoseconds <= 0 { 1_000_000 } else { interval_nanoseconds as u64 };
    Duration::from_nanos(nanoseconds)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicUsize;

    use super::*;

    fn wait_until(mut ready: impl FnMut() -> bool) {
        for _ in 0..10_000 {
            if ready() {
                return;
            }
            thread::yield_now();
        }
        panic!("worker failed to reach the expected state");
    }

    #[test]
    fn worker_observes_running_and_generation_handoff() {
        let state = Arc::new(PollerState::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let flushes = Arc::new(AtomicUsize::new(0));
        let begin = |state: &Arc<PollerState>| {
            let worker_state = state.clone();
            let calls = calls.clone();
            let flushes = flushes.clone();
            state
                .start_configured(
                    1_000_000,
                    move |flush| {
                        assert!(worker_state.is_running());
                        if flush {
                            flushes.fetch_add(1, AtomicOrdering::Relaxed);
                        } else {
                            calls.fetch_add(1, AtomicOrdering::Release);
                        }
                        Ok(())
                    },
                    None,
                    || Ok(()),
                )
                .unwrap()
        };
        let first = begin(&state);
        wait_until(|| calls.load(AtomicOrdering::Acquire) > 0);
        assert!(
            state.configure::<()>(|| panic!("active worker must exclude configuration")).is_err()
        );
        let second = begin(&state);
        assert!(second > first);
        assert_eq!(flushes.load(AtomicOrdering::Relaxed), 0);
        assert!(
            !state.stop_generation(first, || panic!("stale generation must not finish")).unwrap()
        );
        assert!(state.is_running());
        let finished = AtomicBool::new(false);
        assert!(
            state
                .stop_generation(second, || {
                    assert!(!state.is_running());
                    finished.store(true, AtomicOrdering::Relaxed);
                    Ok(())
                })
                .unwrap()
        );
        assert!(finished.load(AtomicOrdering::Relaxed));
        assert_eq!(flushes.load(AtomicOrdering::Relaxed), 1);
        state.configure(|| Ok(())).unwrap();
        state.stop().unwrap();
    }

    struct Dropped(Arc<AtomicUsize>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.fetch_add(1, AtomicOrdering::Release);
        }
    }

    #[test]
    fn configuration_error_and_unwind_join_the_waiting_worker() {
        let state = PollerState::default();
        state.start(1_000_000, |_| Ok(())).unwrap();
        let generation = state.lock_lifecycle().generation;
        for unwind in [false, true] {
            let dropped = Arc::new(AtomicUsize::new(0));
            let token = Dropped(dropped.clone());
            let calls = Arc::new(AtomicUsize::new(0));
            let worker_calls = calls.clone();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                state.start_configured(
                    1_000_000,
                    move |_| {
                        let _keep_alive = &token;
                        worker_calls.fetch_add(1, AtomicOrdering::Relaxed);
                        Ok(())
                    },
                    None,
                    || {
                        if unwind {
                            panic!("configuration unwind");
                        }
                        Err(io::Error::other("configuration failure"))
                    },
                )
            }));
            if unwind {
                assert!(result.is_err());
            } else {
                assert_eq!(result.unwrap().unwrap_err().to_string(), "configuration failure");
            }
            // Capture destruction proves the waiting thread has actually exited,
            // rather than merely dropping/detaching its join handle.
            assert_eq!(dropped.load(AtomicOrdering::Acquire), 1);
            assert_eq!(calls.load(AtomicOrdering::Relaxed), 0);
            assert!(state.is_running());
            let lifecycle = state.lock_lifecycle();
            assert_eq!(lifecycle.generation, generation);
            assert!(lifecycle.control.is_some());
            assert!(!lifecycle.stopping);
        }
        state.stop().unwrap();
        state.configure(|| Ok(())).unwrap();
    }

    struct ExclusiveUse<'a>(&'a AtomicUsize);
    impl<'a> ExclusiveUse<'a> {
        fn enter(active: &'a AtomicUsize) -> Self {
            assert_eq!(active.fetch_add(1, AtomicOrdering::AcqRel), 0);
            Self(active)
        }
    }
    impl Drop for ExclusiveUse<'_> {
        fn drop(&mut self) {
            assert_eq!(self.0.fetch_sub(1, AtomicOrdering::Release), 1);
        }
    }

    #[test]
    fn concurrent_start_configure_and_stop_serialize_lifecycle() {
        let state = Arc::new(PollerState::default());
        let active = Arc::new(AtomicUsize::new(0));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let state = state.clone();
            let active = active.clone();
            threads.push(thread::spawn(move || {
                for _ in 0..if cfg!(miri) { 2 } else { 8 } {
                    let worker_active = active.clone();
                    state
                        .start(1_000_000, move |_| {
                            let _use = ExclusiveUse::enter(&worker_active);
                            thread::yield_now();
                            Ok(())
                        })
                        .unwrap();
                    let _ = state.configure(|| {
                        let _use = ExclusiveUse::enter(&active);
                        thread::yield_now();
                        Ok(())
                    });
                    state.stop().unwrap();
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        state.stop().unwrap();
        assert!(!state.is_running());
        assert_eq!(active.load(AtomicOrdering::Relaxed), 0);
        let lifecycle = state.lock_lifecycle();
        assert!(lifecycle.control.is_none());
        assert!(!lifecycle.stopping);
    }

    #[test]
    fn worker_panic_clears_stopping_and_allows_restart() {
        let state = PollerState::default();
        let entered = Arc::new(AtomicBool::new(false));
        let worker_entered = entered.clone();
        state
            .start(1_000_000, move |_| {
                worker_entered.store(true, AtomicOrdering::Release);
                panic!("worker failure");
            })
            .unwrap();
        wait_until(|| entered.load(AtomicOrdering::Acquire));
        assert!(state.stop().is_err());
        assert!(!state.is_running());
        assert!(!state.lock_lifecycle().stopping);
        state.configure(|| Ok(())).unwrap();
        state.start(1_000_000, |_| Ok(())).unwrap();
        state.stop().unwrap();
    }
}

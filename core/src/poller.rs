use std::{
    cell::UnsafeCell,
    cmp::Ordering,
    collections::BinaryHeap,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU8, Ordering as AtomicOrdering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use crate::{
    spsc_queue::{MessageHeader, PAYLOAD_INFO_OFFSET, PAYLOAD_TIMESTAMP_OFFSET, SpscVarQueue},
    thread_buffer::{THREAD_BUFFER_REGISTRY, ThreadBuffer},
};

/// Internal control handle for managing the poller thread lifecycle.
struct PollerControl {
    stop_requested: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

const POLLER_STOPPED: u8 = 0;
const POLLER_RUNNING: u8 = 1;
const POLLER_STOPPING: u8 = 2;

/// Thread-safe poller state that manages the background polling thread
/// using a lock-free state machine.
///
/// # State transitions
///
/// ```text
/// STOPPED(0) -> RUNNING(1)  : start() CAS succeeds
/// RUNNING(1) -> STOPPING(2) : stop() CAS succeeds, enters join phase
/// STOPPING(2) -> STOPPED(0) : store after join completes
/// ```
///
/// CAS guarantees that only one thread can enter the state-transition
/// critical section at a time, so access to the inner
/// `UnsafeCell<Option<PollerControl>>` is exclusive.
pub struct PollerState {
    status: AtomicU8,
    control: UnsafeCell<Option<PollerControl>>,
}

unsafe impl Sync for PollerState {}

impl Default for PollerState {
    fn default() -> Self {
        Self { status: AtomicU8::new(POLLER_STOPPED), control: UnsafeCell::new(None) }
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

/// A min-heap of ready messages collected from multiple thread buffers,
/// ordered by raw timestamp so that the earliest message is popped first.
pub struct ReadyMessageHeap {
    heap: BinaryHeap<HeapNode>,
}

impl ReadyMessageHeap {
    /// Creates a new heap by reading the front message from each buffer.
    pub fn from_buffers(buffers: &[*mut ThreadBuffer]) -> Self {
        let mut heap = BinaryHeap::new();
        for buffer in buffers {
            push_front_message(&mut heap, *buffer);
        }
        Self { heap }
    }

    /// Pops the message with the smallest (earliest) timestamp.
    pub fn pop(&mut self) -> Option<HeapNode> {
        self.heap.pop()
    }

    /// Re-reads the front message from the given buffer and pushes it
    /// into the heap if available.
    pub fn refresh_buffer(&mut self, buffer: *mut ThreadBuffer) {
        push_front_message(&mut self.heap, buffer);
    }
}

/// Attempts to pin the calling thread to the given CPU core.
/// On Linux, uses `sched_setaffinity`. On other platforms, this is a no-op.
fn pin_current_thread_to_core(core_id: usize) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use libc::{CPU_SET, CPU_ZERO, cpu_set_t, sched_setaffinity};
        // Safety: `cpu_set_t` is a plain-old-data bitmask.  `CPU_ZERO` and
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
    /// Returns `true` if the poller is currently running.
    pub fn is_running(&self) -> bool {
        self.status.load(AtomicOrdering::Acquire) == POLLER_RUNNING
    }

    /// Starts the background polling thread with the given interval and
    /// poll function. If the poller is already running, it is stopped first.
    pub fn start<F>(&self, interval_nanoseconds: i64, poll_fn: F) -> io::Result<()>
    where
        F: Fn(bool) -> io::Result<()> + Send + 'static,
    {
        self.start_on_core(interval_nanoseconds, poll_fn, None)
    }

    /// Starts the background polling thread with the given interval and
    /// poll function, optionally pinning it to the specified CPU core.
    /// If the poller is already running, it is stopped first.
    pub fn start_on_core<F>(
        &self,
        interval_nanoseconds: i64,
        poll_fn: F,
        core_id: Option<usize>,
    ) -> io::Result<()>
    where
        F: Fn(bool) -> io::Result<()> + Send + 'static,
    {
        // If already running, stop first
        if self.status.load(AtomicOrdering::Acquire) == POLLER_RUNNING {
            self.stop()?;
        }

        // CAS(STOPPED -> RUNNING) to acquire exclusive ownership
        if self
            .status
            .compare_exchange(
                POLLER_STOPPED,
                POLLER_RUNNING,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            )
            .is_err()
        {
            return Err(io::Error::other(
                "ferrilog: poller state transition failed (concurrent call or stopping in progress)",
            ));
        }

        let stop_requested = Arc::new(AtomicBool::new(false));
        let thread_stop_requested = Arc::clone(&stop_requested);
        let sleep_duration = poll_interval(interval_nanoseconds);
        let handle =
            thread::Builder::new().name("ferrilog-poller".to_string()).spawn(move || {
                if let Some(core) = core_id {
                    let _ = pin_current_thread_to_core(core);
                }
                while !thread_stop_requested.load(AtomicOrdering::Acquire) {
                    let _ = poll_fn(false);
                    thread::sleep(sleep_duration);
                }
                let _ = poll_fn(true);
            })?;

        // Safety: after CAS succeeds, only the current thread can write to control
        unsafe {
            *self.control.get() = Some(PollerControl { stop_requested, handle });
        }
        Ok(())
    }

    /// Stops the background polling thread and waits for it to finish.
    /// Returns `Ok(())` if already stopped or stopping.
    pub fn stop(&self) -> io::Result<()> {
        // CAS(RUNNING -> STOPPING) to acquire exclusive ownership
        if self
            .status
            .compare_exchange(
                POLLER_RUNNING,
                POLLER_STOPPING,
                AtomicOrdering::AcqRel,
                AtomicOrdering::Acquire,
            )
            .is_err()
        {
            // Already stopped or currently stopping; nothing to do
            return Ok(());
        }

        // Safety: after CAS succeeds, only the current thread can read/write control
        let control = unsafe { (*self.control.get()).take() };
        if let Some(PollerControl { stop_requested, handle }) = control {
            stop_requested.store(true, AtomicOrdering::Release);
            handle.join().map_err(|_| io::Error::other("ferrilog polling thread panicked"))?;
        }

        self.status.store(POLLER_STOPPED, AtomicOrdering::Release);
        Ok(())
    }
}

/// Scans the thread buffer registry and deallocates any buffers that have
/// been marked for deallocation and are fully drained.
pub fn cleanup_finished_buffers() {
    for index in 0..THREAD_BUFFER_REGISTRY.count() {
        let pointer = THREAD_BUFFER_REGISTRY.get(index);
        if pointer.is_null() {
            continue;
        }

        let should_deallocate =
            unsafe { (*pointer).should_deallocate.load(AtomicOrdering::Acquire) };
        let is_empty = unsafe {
            let queue_pointer = &raw const (*pointer).queue;
            SpscVarQueue::consumer_front(queue_pointer).is_none()
        };

        if should_deallocate && is_empty {
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

    let header = unsafe {
        let queue_pointer = &raw const (*buffer).queue;
        SpscVarQueue::consumer_front(queue_pointer)
    };

    let Some(header) = header else {
        return;
    };

    let raw_timestamp = unsafe {
        let payload = (*header).payload_pointer() as *const u8;

        // Prefetch the payload's info pointer and the StaticInfo it points to.
        // By the time this HeapNode reaches the top of the min-heap, the data
        // will already be in L2/L1 cache, avoiding a pointer-chasing stall.
        // Locality hint 2 = moderate temporal locality (will be used soon).
        core::intrinsics::prefetch_read_data::<u8, 2>(payload);
        let info_ptr = payload.add(PAYLOAD_INFO_OFFSET).cast::<*const u8>().read_unaligned();
        if !info_ptr.is_null() {
            core::intrinsics::prefetch_read_data::<u8, 2>(info_ptr);
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

use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
};

use crate::spsc_queue::{BLOCK_COUNT, MessageHeader, SpscVarQueue};

/// Per-thread buffer containing an SPSC queue and thread metadata.
/// Allocated on the heap via `new_on_heap()` (1MB+, too large for the stack).
#[repr(C)]
pub struct ThreadBuffer {
    /// The SPSC variable-length message queue.
    pub queue: SpscVarQueue,
    /// Flag indicating whether this buffer should be deallocated by the background poller.
    pub should_deallocate: AtomicBool,
    /// Thread name stored as raw UTF-8 bytes (up to 32 bytes).
    pub name: [u8; 32],
    /// Number of valid bytes in `name`.
    pub name_length: usize,
}

unsafe impl Send for ThreadBuffer {}

impl ThreadBuffer {
    /// Allocates a zero-initialized `ThreadBuffer` on the heap.
    pub fn new_on_heap() -> *mut Self {
        unsafe {
            let layout = std::alloc::Layout::new::<Self>();
            let pointer = std::alloc::alloc_zeroed(layout) as *mut Self;
            if pointer.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            let queue_pointer = &raw mut (*pointer).queue;
            let free_write_count_pointer = (queue_pointer as *mut u8)
                .add(core::mem::offset_of!(SpscVarQueue, free_write_count))
                as *mut u32;
            free_write_count_pointer.write(BLOCK_COUNT);
            (*pointer).should_deallocate = AtomicBool::new(false);
            pointer
        }
    }

    /// Frees a heap-allocated `ThreadBuffer`.
    ///
    /// # Safety
    /// `pointer` must have been returned by `new_on_heap()` and must only be freed once.
    pub unsafe fn free_on_heap(pointer: *mut Self) {
        let layout = std::alloc::Layout::new::<Self>();
        unsafe { std::alloc::dealloc(pointer as *mut u8, layout) };
    }

    /// Sets the thread name, truncated to the buffer capacity (32 bytes).
    pub fn set_name(&mut self, thread_name: &str) {
        let length = thread_name.len().min(self.name.len());
        self.name[..length].copy_from_slice(&thread_name.as_bytes()[..length]);
        self.name_length = length;
    }

    /// Returns the thread name as a byte slice.
    pub fn name_bytes(&self) -> &[u8] {
        &self.name[..self.name_length]
    }
}

/// Maximum number of concurrent writer threads supported. Log calls from threads
/// exceeding this limit will panic. 1MB queue x 256 = 256MB memory ceiling,
/// which is sufficient for most use cases.
pub const MAX_THREADS: usize = 256;

/// Lock-free thread buffer registry.
///
/// # Design
///
/// - Fixed-size `AtomicPtr` array with capacity [`MAX_THREADS`].
/// - Writer (application thread): atomic `fetch_add` to claim a slot index,
///   then `store` the pointer.
/// - Reader (poller): iterates `0..count`, loading each slot's pointer.
/// - No Mutex, no heap allocation; registration path is a single atomic
///   operation.
///
/// The writer appends once during `preallocate()` (fetch_add + store),
/// and the reader traverses lock-free during `poll()`. Zero contention.
pub struct ThreadBufferRegistry {
    /// Number of registered slots (monotonically increasing).
    count: AtomicUsize,
    /// Fixed-size array where each slot stores a `*mut ThreadBuffer`.
    /// Unused slots hold null.
    slots: [AtomicPtr<ThreadBuffer>; MAX_THREADS],
}

unsafe impl Sync for ThreadBufferRegistry {}

impl Default for ThreadBufferRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ThreadBufferRegistry {
    /// Returns a null atomic pointer used to initialize every slot in the array.
    ///
    /// This is a function rather than a `const` because `AtomicPtr` has interior
    /// mutability, and clippy warns against `const` items with interior mutability
    /// since each use site gets a distinct instance rather than sharing one.
    #[inline]
    const fn null_slot() -> AtomicPtr<ThreadBuffer> {
        AtomicPtr::new(std::ptr::null_mut())
    }

    /// Creates a new empty registry.
    pub const fn new() -> Self {
        Self { count: AtomicUsize::new(0), slots: [const { Self::null_slot() }; MAX_THREADS] }
    }

    /// Registers a `ThreadBuffer`. Called by the writer side, once per thread.
    ///
    /// Internally performs a single fetch_add (claim slot) + a single store (write pointer).
    pub fn register(&self, thread_buffer: *mut ThreadBuffer) {
        let index = self.count.fetch_add(1, Ordering::AcqRel);
        assert!(index < MAX_THREADS, "ferrilog: exceeded maximum thread count {MAX_THREADS}");
        self.slots[index].store(thread_buffer, Ordering::Release);
    }

    /// Returns the current number of registered slots.
    #[inline]
    pub fn count(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }

    /// Loads the pointer at the given slot index. Called by the reader side, lock-free.
    ///
    /// Returns null if the slot has not yet been fully written (a very brief window).
    #[inline]
    pub fn get(&self, index: usize) -> *mut ThreadBuffer {
        self.slots[index].load(Ordering::Acquire)
    }

    /// Clears a slot by storing null (called by the poller after reclaiming a `ThreadBuffer`).
    pub fn clear_slot(&self, index: usize) {
        self.slots[index].store(std::ptr::null_mut(), Ordering::Release);
    }
}

/// Global lock-free registry.
pub static THREAD_BUFFER_REGISTRY: ThreadBufferRegistry = ThreadBufferRegistry::new();

// --- Thread-local management ---

/// Thread-local pointer to the current thread's `ThreadBuffer`.
#[thread_local]
static BUFFER_POINTER: Cell<*mut ThreadBuffer> = Cell::new(std::ptr::null_mut());

/// RAII guard that sets `should_deallocate` on thread exit so the background
/// poller can reclaim the buffer.
struct BufferGuard {
    _non_zero_sized: u8,
}

impl Drop for BufferGuard {
    fn drop(&mut self) {
        let pointer = BUFFER_POINTER.get();
        if !pointer.is_null() {
            unsafe {
                (*pointer).should_deallocate.store(true, Ordering::Release);
            }
            BUFFER_POINTER.set(std::ptr::null_mut());
        }
    }
}

thread_local! {
    static BUFFER_GUARD: BufferGuard = const { BufferGuard { _non_zero_sized: 0 } };
}

/// Ensures the current thread has an allocated `ThreadBuffer`.
/// If not yet allocated, creates one on the heap and registers it in the
/// global lock-free registry.
#[doc(hidden)]
pub fn preallocate() {
    if !BUFFER_POINTER.get().is_null() {
        return;
    }

    let thread_buffer = ThreadBuffer::new_on_heap();

    unsafe {
        let thread_id = std::thread::current().id();
        let name = format!("{:?}", thread_id);
        (*thread_buffer).set_name(&name);
    }

    BUFFER_POINTER.set(thread_buffer);
    BUFFER_GUARD.with(|_| {});

    // Lock-free registration: one fetch_add + one store
    THREAD_BUFFER_REGISTRY.register(thread_buffer);
}

/// Returns the current thread's `ThreadBuffer` pointer, allocating one first if needed.
///
/// The null check is kept on the hot path but the allocation branch is
/// marked `#[cold]` + `#[inline(never)]` so the compiler lays out the
/// fast path (non-null return) contiguously in the instruction cache.
#[inline(always)]
pub fn get_thread_buffer() -> *mut ThreadBuffer {
    let pointer = BUFFER_POINTER.get();
    if core::intrinsics::unlikely(pointer.is_null()) { get_thread_buffer_slow() } else { pointer }
}

/// Cold path: allocate and register a new `ThreadBuffer` for this thread.
#[cold]
#[inline(never)]
fn get_thread_buffer_slow() -> *mut ThreadBuffer {
    preallocate();
    BUFFER_POINTER.get()
}

/// Sets the logging thread name for the current thread.
///
/// The name is truncated to 32 bytes and used for the `{thread}` token in the
/// header pattern.
pub fn set_thread_name(thread_name: &str) {
    let thread_buffer = get_thread_buffer();
    unsafe {
        (*thread_buffer).set_name(thread_name);
    }
}

/// Allocates space for a message in the current thread's SPSC queue.
#[inline(always)]
pub fn alloc_message(size: u32) -> Option<*mut MessageHeader> {
    let thread_buffer = get_thread_buffer();
    let mut spin_count = 0u32;

    loop {
        let result = unsafe {
            let queue_pointer = &raw mut (*thread_buffer).queue;
            SpscVarQueue::producer_alloc(queue_pointer, size)
        };

        if core::intrinsics::likely(result.is_some()) {
            return result;
        }

        match crate::logger::handle_queue_full() {
            crate::logger::QueueFullAction::Drop => return None,
            crate::logger::QueueFullAction::Retry => {
                spin_count += 1;
                if spin_count <= 64 {
                    std::hint::spin_loop();
                } else {
                    std::thread::yield_now();
                    spin_count = 0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preallocate_and_get() {
        preallocate();
        let pointer = get_thread_buffer();
        assert!(!pointer.is_null());
        let pointer2 = get_thread_buffer();
        assert_eq!(pointer, pointer2);
    }

    #[test]
    fn test_alloc_message() {
        let header = alloc_message(16);
        assert!(header.is_some());
    }

    #[test]
    fn test_thread_buffer_registered() {
        preallocate();
        let count = THREAD_BUFFER_REGISTRY.count();
        assert!(count > 0);
    }

    #[test]
    fn test_set_thread_name() {
        set_thread_name("worker-a");
        let pointer = get_thread_buffer();
        let name = unsafe { std::str::from_utf8((*pointer).name_bytes()).unwrap() };
        assert_eq!(name, "worker-a");
    }
}

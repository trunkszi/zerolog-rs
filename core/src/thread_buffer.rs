use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering},
};

use crate::spsc_queue::{MAX_MESSAGE_PAYLOAD_SIZE, MessageHeader, SpscVarQueue};

/// Per-thread buffer containing an SPSC queue and thread metadata.
/// Allocated on the heap via `new_on_heap()` (1MB+, too large for the stack).
#[repr(C)]
pub struct ThreadBuffer {
    /// The SPSC variable-length message queue.
    pub queue: SpscVarQueue,
    /// Flag indicating whether this buffer should be deallocated by the background poller.
    pub should_deallocate: AtomicBool,
    /// Published immutable name; old versions live until buffer reclamation.
    name: AtomicPtr<ThreadName>,
}

struct ThreadName {
    previous: *mut ThreadName,
    length: usize,
    bytes: [u8; 32],
}

// SAFETY: an owned buffer can move before publication or after its producer
// retires. Concurrent access still requires the queue and registry protocols.
unsafe impl Send for ThreadBuffer {}

impl ThreadBuffer {
    /// Allocates a zero-initialized `ThreadBuffer` on the heap.
    pub fn new_on_heap() -> *mut Self {
        // SAFETY: zero is valid for all fields; producer state is initialized
        // before publication, without putting the large queue on the stack.
        unsafe {
            let layout = std::alloc::Layout::new::<Self>();
            let pointer = std::alloc::alloc_zeroed(layout) as *mut Self;
            if pointer.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            let queue_pointer = &raw mut (*pointer).queue;
            SpscVarQueue::init_on_heap(queue_pointer);
            pointer
        }
    }

    /// Frees a heap-allocated `ThreadBuffer`.
    ///
    /// # Safety
    /// The pointer must come from new_on_heap and be freed exactly once, after
    /// the producer has retired and every consumer borrow has ended.
    pub unsafe fn free_on_heap(pointer: *mut Self) {
        // SAFETY: the allocation and all name versions are exclusively owned;
        // the producer is retired and no consumer retains a name slice.
        unsafe {
            let mut name = (*pointer).name.load(Ordering::Relaxed);
            while !name.is_null() {
                let version = Box::from_raw(name);
                name = version.previous;
            }
            std::alloc::dealloc(pointer.cast(), std::alloc::Layout::new::<Self>());
        }
    }

    /// Publishes a UTF-8 name of up to 32 bytes.
    ///
    /// Old versions remain immutable until the entire buffer is reclaimed, so
    /// a consumer's borrowed name remains valid across any number of renames.
    pub fn set_name(&self, thread_name: &str) {
        let length = thread_name.floor_char_boundary(32);
        let mut bytes = [0; 32];
        bytes[..length].copy_from_slice(&thread_name.as_bytes()[..length]);
        let name =
            Box::into_raw(Box::new(ThreadName { previous: std::ptr::null_mut(), length, bytes }));
        let mut previous = self.name.load(Ordering::Acquire);
        loop {
            // SAFETY: this allocation is unpublished until our CAS succeeds.
            // No Box/reference is retained across concurrent publication.
            unsafe { (*name).previous = previous };
            match self.name.compare_exchange_weak(
                previous,
                name,
                Ordering::Release,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => previous = actual,
            }
        }
    }

    /// Returns an immutable snapshot valid for the lifetime of this buffer.
    pub fn name_bytes(&self) -> &[u8] {
        let name = self.name.load(Ordering::Acquire);
        if name.is_null() {
            return &[];
        }
        // SAFETY: published names are immutable and retained until this buffer
        // is exclusively reclaimed, which outlives this shared borrow.
        unsafe {
            let name = &*name;
            &name.bytes[..name.length]
        }
    }
}

/// Maximum number of concurrent writer buffers. Registration failure drops
/// log attempts without growing the registry scan bound.
pub const MAX_THREADS: usize = 256;

/// Lock-free registry with reusable atomic pointer slots.
///
/// Producers claim null slots and publish initialized buffers. The sole
/// consumer drains retired buffers, frees them, and clears their slots.
/// The count is a bounded high-water mark, not a lifetime registration count.///
/// Registration is internal so safe callers cannot inject arbitrary pointers.
///
/// ```compile_fail
/// let registry = zerolog_core::thread_buffer::ThreadBufferRegistry::new();
/// registry.register(std::ptr::dangling_mut());
/// ```
pub struct ThreadBufferRegistry {
    /// Bounded high-water mark of slot indices that have been used.
    count: AtomicUsize,
    /// Fixed-size array where each slot stores a `*mut ThreadBuffer`.
    /// Unused slots hold null.
    slots: [AtomicPtr<ThreadBuffer>; MAX_THREADS],
}

// Atomic slots and the atomic scan bound provide registry synchronization.

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

    /// Publishes a live initialized buffer in a free slot, or reports capacity.
    pub(crate) fn register(&self, thread_buffer: *mut ThreadBuffer) -> bool {
        if thread_buffer.is_null() {
            return false;
        }
        for (index, slot) in self.slots.iter().enumerate() {
            if slot
                .compare_exchange(
                    std::ptr::null_mut(),
                    thread_buffer,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                self.count.fetch_max(index + 1, Ordering::AcqRel);
                return true;
            }
        }
        false
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

#[thread_local]
static THREAD_EXITED: Cell<bool> = const { Cell::new(false) };

/// RAII guard that sets `should_deallocate` on thread exit so the background
/// poller can reclaim the buffer.
struct BufferGuard {
    _non_zero_sized: u8,
}

impl Drop for BufferGuard {
    fn drop(&mut self) {
        THREAD_EXITED.set(true);
        let pointer = BUFFER_POINTER.replace(std::ptr::null_mut());
        if !pointer.is_null() {
            // SAFETY: this is the final producer access; release permits
            // the consumer to reclaim only after all producer writes finish.
            unsafe { (*pointer).should_deallocate.store(true, Ordering::Release) };
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
    if !BUFFER_POINTER.get().is_null() || THREAD_EXITED.get() {
        return;
    }
    // Initialize the destructor before allocating/publishing. A destroyed
    // guard must not result in another registered buffer on the exiting thread.
    if BUFFER_GUARD.try_with(|_| {}).is_err() {
        THREAD_EXITED.set(true);
        return;
    }
    let thread_buffer = ThreadBuffer::new_on_heap();
    let name = format!("{:?}", std::thread::current().id());
    // SAFETY: the new buffer is still private to this thread.
    unsafe { (*thread_buffer).set_name(&name) };
    if THREAD_BUFFER_REGISTRY.register(thread_buffer) {
        BUFFER_POINTER.set(thread_buffer);
    } else {
        // SAFETY: registration failed; no other thread can hold this buffer.
        unsafe { ThreadBuffer::free_on_heap(thread_buffer) };
    }
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
    if !thread_buffer.is_null() {
        // SAFETY: this thread owns the live producer buffer; name publication
        // touches only atomic metadata and unpublished immutable allocations.
        unsafe { (*thread_buffer).set_name(thread_name) };
    }
}

/// Allocates space for a message in the current thread's SPSC queue.
#[inline(always)]
pub fn alloc_message(size: u32) -> Option<*mut MessageHeader> {
    if core::intrinsics::unlikely(size as usize > MAX_MESSAGE_PAYLOAD_SIZE) {
        return alloc_message_slow(size);
    }
    let buffer = BUFFER_POINTER.get();
    if core::intrinsics::likely(!buffer.is_null()) {
        // SAFETY: this thread is the sole producer for its live TLS buffer.
        let header = unsafe { SpscVarQueue::producer_alloc(&raw mut (*buffer).queue, size) };
        if core::intrinsics::likely(header.is_some()) {
            return header;
        }
    }
    alloc_message_slow(size)
}

#[cold]
#[inline(never)]
fn alloc_message_slow(size: u32) -> Option<*mut MessageHeader> {
    if size as usize > MAX_MESSAGE_PAYLOAD_SIZE {
        let _ = crate::logger::handle_queue_full();
        return None;
    }
    let thread_buffer = get_thread_buffer();
    if thread_buffer.is_null() {
        return None;
    }
    let mut spin_count = 0u32;
    loop {
        // SAFETY: this thread is the unique producer of the registered buffer.
        let result = unsafe { SpscVarQueue::producer_alloc(&raw mut (*thread_buffer).queue, size) };
        if let Some(header) = result {
            return Some(header);
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
    #[test]
    fn registry_reuses_retired_slots_and_bounds_count() {
        let registry = ThreadBufferRegistry::new();
        for _ in 0..350 {
            let buffer = ThreadBuffer::new_on_heap();
            assert!(registry.register(buffer));
            assert_eq!(registry.get(0), buffer);
            assert_eq!(registry.count(), 1);
            // SAFETY: this local registry has no concurrent readers.
            unsafe { ThreadBuffer::free_on_heap(buffer) };
            registry.clear_slot(0);
        }
        assert!(!registry.register(std::ptr::null_mut()));
        assert!(registry.count() <= MAX_THREADS);
    }

    #[test]
    fn full_registry_failure_preserves_scan_bound() {
        let registry = ThreadBufferRegistry::new();
        let buffer = ThreadBuffer::new_on_heap();
        // This isolated registry is never consumed. Repeated pointer identities
        // exercise capacity accounting without allocating 256 large queues.
        for _ in 0..MAX_THREADS {
            assert!(registry.register(buffer));
        }
        assert_eq!(registry.count(), MAX_THREADS);
        assert!(!registry.register(buffer));
        assert_eq!(registry.count(), MAX_THREADS);
        for index in 0..MAX_THREADS {
            registry.clear_slot(index);
        }
        assert!(registry.register(buffer));
        registry.clear_slot(0);
        // SAFETY: the local registry has no remaining pointers or readers.
        unsafe { ThreadBuffer::free_on_heap(buffer) };
    }

    #[derive(Clone, Copy)]
    struct BufferPointer(*mut ThreadBuffer);
    // SAFETY: the test shares only immutable names plus their atomic head;
    // the buffer remains alive until the writer and reader have both finished.
    unsafe impl Send for BufferPointer {}
    impl BufferPointer {
        fn get(self) -> *mut ThreadBuffer {
            self.0
        }
    }

    #[test]
    fn name_snapshots_survive_concurrent_renaming_and_utf8_truncation() {
        let pointer = ThreadBuffer::new_on_heap();
        // SAFETY: the allocation is live and its immutable name chain is
        // published atomically; no one frees it until both threads terminate.
        let initial = unsafe {
            (*pointer).set_name("original");
            (*pointer).name_bytes()
        };
        let writer_pointer = BufferPointer(pointer);
        let writer = std::thread::spawn(move || {
            for i in 0..if cfg!(miri) { 16 } else { 1000 } {
                let name = if i % 2 == 0 { "é".repeat(20) } else { "a".repeat(31) + "界" };
                // SAFETY: only atomic publication and new immutable nodes change.
                unsafe { (*writer_pointer.get()).set_name(&name) };
                std::thread::yield_now();
            }
        });
        for _ in 0..if cfg!(miri) { 16 } else { 1000 } {
            // SAFETY: the name is immutable and allocation remains live.
            let bytes = unsafe { (*pointer).name_bytes() };
            assert!(bytes.len() <= 32);
            assert!(std::str::from_utf8(bytes).is_ok());
            assert_eq!(initial, b"original");
            std::thread::yield_now();
        }
        writer.join().unwrap();
        assert_eq!(initial, b"original");
        // SAFETY: all snapshots are used for the last time before reclamation.
        unsafe {
            (*pointer).set_name(&("a".repeat(31) + "界"));
            assert_eq!((*pointer).name_bytes(), "a".repeat(31).as_bytes());
            ThreadBuffer::free_on_heap(pointer);
        }
    }
}

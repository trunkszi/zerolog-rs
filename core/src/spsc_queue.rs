use core::{
    cell::UnsafeCell,
    mem::{MaybeUninit, size_of},
};
use std::sync::atomic::{AtomicU32, Ordering};

/// 1 MiB in bytes.
const MIB: usize = 1024 * 1024;

/// Per-thread log buffer size in bytes, selected at compile time via Cargo
/// features.
///
/// # Configuration
///
/// Add one of the `buffer-*` features in your `Cargo.toml`:
///
/// ```toml
/// zerolog = { version = "0.1", features = ["buffer-4m"] }
/// ```
///
/// Available features: `buffer-1m`, `buffer-2m` (default), `buffer-4m`,
/// `buffer-8m`, `buffer-16m`, `buffer-32m`, `buffer-64m`.
///
/// # Feature unification
///
/// If multiple buffer-size features are enabled (e.g. by Cargo dependency
/// unification), the **largest** size wins. This is consistent with the
/// additive semantics of Cargo features.
#[cfg(feature = "buffer-64m")]
pub const BUFFER_SIZE: usize = 64 * MIB;

/// See [`BUFFER_SIZE`] for documentation.
#[cfg(all(feature = "buffer-32m", not(feature = "buffer-64m")))]
pub const BUFFER_SIZE: usize = 32 * MIB;

/// See [`BUFFER_SIZE`] for documentation.
#[cfg(all(feature = "buffer-16m", not(any(feature = "buffer-32m", feature = "buffer-64m"))))]
pub const BUFFER_SIZE: usize = 16 * MIB;

/// See [`BUFFER_SIZE`] for documentation.
#[cfg(all(
    feature = "buffer-8m",
    not(any(feature = "buffer-16m", feature = "buffer-32m", feature = "buffer-64m"))
))]
pub const BUFFER_SIZE: usize = 8 * MIB;

/// See [`BUFFER_SIZE`] for documentation.
#[cfg(all(
    feature = "buffer-4m",
    not(any(
        feature = "buffer-8m",
        feature = "buffer-16m",
        feature = "buffer-32m",
        feature = "buffer-64m"
    ))
))]
pub const BUFFER_SIZE: usize = 4 * MIB;

/// See [`BUFFER_SIZE`] for documentation.
#[cfg(all(
    feature = "buffer-2m",
    not(any(
        feature = "buffer-4m",
        feature = "buffer-8m",
        feature = "buffer-16m",
        feature = "buffer-32m",
        feature = "buffer-64m"
    ))
))]
pub const BUFFER_SIZE: usize = 2 * MIB;

/// See [`BUFFER_SIZE`] for documentation.
#[cfg(all(
    feature = "buffer-1m",
    not(any(
        feature = "buffer-2m",
        feature = "buffer-4m",
        feature = "buffer-8m",
        feature = "buffer-16m",
        feature = "buffer-32m",
        feature = "buffer-64m"
    ))
))]
pub const BUFFER_SIZE: usize = MIB;

/// See [`BUFFER_SIZE`] for documentation. Fallback when no feature is selected.
#[cfg(not(any(
    feature = "buffer-1m",
    feature = "buffer-2m",
    feature = "buffer-4m",
    feature = "buffer-8m",
    feature = "buffer-16m",
    feature = "buffer-32m",
    feature = "buffer-64m",
)))]
pub const BUFFER_SIZE: usize = 2 * MIB;

const _: () = assert!(BUFFER_SIZE.is_power_of_two(), "BUFFER_SIZE must be a power of two");
const _: () = assert!(BUFFER_SIZE >= MIB, "BUFFER_SIZE must be at least 1 MiB");
const _: () = assert!(BUFFER_SIZE <= 64 * MIB, "BUFFER_SIZE must not exceed 64 MiB");

/// Message header size (8 bytes).
pub const HEADER_SIZE: usize = core::mem::size_of::<MessageHeader>();

/// Largest payload guaranteed to fit after draining at any ring position.
pub const MAX_MESSAGE_PAYLOAD_SIZE: usize = (BUFFER_SIZE / 2) - (HEADER_SIZE * 2);

/// Number of 8-byte blocks in the ring buffer.
pub const BLOCK_COUNT: u32 = (BUFFER_SIZE / HEADER_SIZE) as u32;

/// Byte offset of the `*const StaticInfo` pointer within the payload.
pub const PAYLOAD_INFO_OFFSET: usize = 0;

/// Byte offset of the `i64` timestamp within the payload.
pub const PAYLOAD_TIMESTAMP_OFFSET: usize = PAYLOAD_INFO_OFFSET + size_of::<usize>();

/// Byte offset of the `u64` current span id within the payload.
pub const PAYLOAD_SPAN_ID_OFFSET: usize = PAYLOAD_TIMESTAMP_OFFSET + size_of::<i64>();

/// Byte offset where encoded arguments begin within the payload.
pub const PAYLOAD_ARGS_OFFSET: usize = PAYLOAD_SPAN_ID_OFFSET + size_of::<u64>();

/// Fixed overhead per message (info pointer + timestamp + span id) before encoded arguments.
pub const PAYLOAD_HEADER_SIZE: usize = PAYLOAD_ARGS_OFFSET;

/// Message header. `size` is the cross-thread control word (`AtomicU32`).
///
/// The header occupies exactly 8 bytes (one ring-buffer block). The payload
/// immediately follows and stores:
///
/// ```text
/// [info_ptr: *const StaticInfo (8B)] [timestamp: i64 (8B)] [span_id: u64 (8B)] [encoded args...]
/// ```
///
/// Layout constants [`PAYLOAD_INFO_OFFSET`], [`PAYLOAD_TIMESTAMP_OFFSET`],
/// [`PAYLOAD_SPAN_ID_OFFSET`], [`PAYLOAD_ARGS_OFFSET`], and
/// [`PAYLOAD_HEADER_SIZE`] describe the payload structure.
#[repr(C)]
pub struct MessageHeader {
    /// Total message size in bytes (header + payload), used as an atomic control word.
    pub size: AtomicU32,
    /// Padding to maintain 8-byte header alignment.
    _padding: u32,
}

impl MessageHeader {
    /// Publish a message: performs a `Release` store of the total size,
    /// making the payload visible to the consumer.
    #[inline(always)]
    pub fn push(&self, payload_size: u32) {
        self.size.store(payload_size + HEADER_SIZE as u32, Ordering::Release);
    }

    /// Returns a pointer just past this borrowed header.
    ///
    /// This compatibility accessor does not extend the provenance of the
    /// header borrow. Queue readers/writers should use payload_pointer_raw
    /// with their original reservation pointer to access the payload.
    #[inline(always)]
    pub fn payload_pointer(&self) -> *mut u8 {
        // SAFETY: computing a one-past-header pointer is valid.
        unsafe { (self as *const Self).add(1).cast_mut().cast() }
    }

    /// Returns a payload pointer retaining the original queue provenance.
    ///
    /// # Safety
    /// The header must point into a live queue allocation with sufficient
    /// trailing space. Payload writes require a unique unpublished producer
    /// reservation; reads require an acquired record not yet popped.
    #[inline(always)]
    pub unsafe fn payload_pointer_raw(header: *const Self) -> *mut u8 {
        // SAFETY: the caller supplies a live queue pointer with trailing space.
        unsafe { header.add(1).cast_mut().cast() }
    }
}

/// Single-producer single-consumer variable-length lock-free ring queue.
///
/// The underlying storage uses `MaybeUninit<u64>` as raw byte blocks instead of
/// `[MessageHeader; N]`. This allows subsequent blocks to be safely used as raw
/// payload byte regions without overwriting initialized `MessageHeader`/`AtomicU32`
/// objects with invalid values.
#[repr(C)]
pub struct SpscVarQueue {
    /// Raw ring buffer storage.
    storage: UnsafeCell<[MaybeUninit<u64>; BLOCK_COUNT as usize]>,
    /// Current write position in blocks (producer only).
    write_index: UnsafeCell<u32>,
    /// Number of free blocks available for writing (producer only).
    free_write_count: UnsafeCell<u32>,
    /// Padding to prevent false sharing between producer and consumer fields.
    _cache_line_padding: [u8; 120],
    /// Current read position in blocks (consumer only, atomically shared).
    read_index: AtomicU32,
}

impl SpscVarQueue {
    /// Initializes producer state in a zeroed, unpublished heap allocation.
    ///
    /// # Safety
    /// All bytes of this must be zeroed and the allocation exclusively owned.
    pub(crate) unsafe fn init_on_heap(this: *mut Self) {
        // SAFETY: the caller owns the unpublished zeroed allocation.
        unsafe { UnsafeCell::raw_get(&raw const (*this).free_write_count).write(BLOCK_COUNT) };
    }

    #[inline(always)]
    unsafe fn header_at(this: *const Self, index: u32) -> *mut MessageHeader {
        debug_assert!(index < BLOCK_COUNT);
        // SAFETY: storage has interior mutability; only the requested block is
        // projected, without borrowing the queue or any other payload bytes.
        unsafe {
            UnsafeCell::raw_get(&raw const (*this).storage)
                .cast::<MaybeUninit<u64>>()
                .add(index as usize)
                .cast()
        }
    }

    /// Reserves payload bytes on the producer side.
    ///
    /// # Safety
    /// This must point to a live initialized queue. Exactly one producer may
    /// reserve and publish records, with at most one reservation outstanding.
    /// The consumer may concurrently access only published records.
    #[inline(always)]
    pub unsafe fn producer_alloc(this: *mut Self, size: u32) -> Option<*mut MessageHeader> {
        if size as usize > MAX_MESSAGE_PAYLOAD_SIZE {
            return None;
        }
        // SAFETY: producer fields are accessed only by the unique producer.
        // Atomic references cover only atomic fields, never the whole queue.
        unsafe {
            // The checked payload bound leaves room for header and rounding in u32.
            let block_size = (size + HEADER_SIZE as u32).div_ceil(HEADER_SIZE as u32);
            let write_ptr = UnsafeCell::raw_get(&raw const (*this).write_index);
            let free_ptr = UnsafeCell::raw_get(&raw const (*this).free_write_count);
            let mut write = write_ptr.read();
            let mut free = free_ptr.read();
            if core::intrinsics::unlikely(block_size >= free) {
                let read = (&*std::ptr::addr_of!((*this).read_index)).load(Ordering::Acquire);
                if read <= write {
                    free = BLOCK_COUNT - write;
                    if block_size >= free && read != 0 {
                        // Initialize the head before publishing the tail sentinel.
                        // The consumer can acknowledge an empty wrap by moving
                        // read_index to zero even before a head record exists.
                        (*Self::header_at(this, 0)).size.store(0, Ordering::Relaxed);
                        (*Self::header_at(this, write)).size.store(1, Ordering::Release);
                        write = 0;
                        free = read;
                    }
                } else {
                    free = read - write;
                }
                write_ptr.write(write);
                free_ptr.write(free);
                if block_size >= free {
                    return None;
                }
            }

            let header = Self::header_at(this, write);
            write += block_size;
            write_ptr.write(write);
            free_ptr.write(free - block_size);
            (*Self::header_at(this, write)).size.store(0, Ordering::Relaxed);
            Some(header)
        }
    }

    /// Peeks at the front published message, acknowledging wrap sentinels.
    ///
    /// # Safety
    /// This must point to a live initialized queue. Only the unique consumer
    /// may call this method or consumer_pop. The producer may run concurrently.
    #[inline]
    pub unsafe fn consumer_front(this: *const Self) -> Option<*const MessageHeader> {
        // SAFETY: field-local atomic accesses synchronize publication and reuse.
        unsafe {
            let read = &*std::ptr::addr_of!((*this).read_index);
            let index = read.load(Ordering::Relaxed);
            let header = Self::header_at(this, index);
            match (*header).size.load(Ordering::Acquire) {
                0 => None,
                1 => {
                    // Retire the tail even when the new head is still empty.
                    read.store(0, Ordering::Release);
                    let head = Self::header_at(this, 0);
                    ((*head).size.load(Ordering::Acquire) != 0).then_some(head.cast_const())
                }
                _ => Some(header.cast_const()),
            }
        }
    }

    /// Releases the record returned by consumer_front.
    ///
    /// # Safety
    /// Only the unique consumer may call this method, after observing a
    /// published record with consumer_front and finishing all payload reads.
    #[inline]
    pub unsafe fn consumer_pop(this: *mut Self) {
        // SAFETY: the consumer owns its cursor and has acquired this header's
        // publication; release hands consumed bytes back to the producer.
        unsafe {
            let read = &*std::ptr::addr_of!((*this).read_index);
            let index = read.load(Ordering::Relaxed);
            let size = (*Self::header_at(this, index)).size.load(Ordering::Relaxed);
            debug_assert!(size > 1);
            read.store(index + size.div_ceil(HEADER_SIZE as u32), Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alloc_queue_on_heap() -> *mut SpscVarQueue {
        // SAFETY: initialize valid zeroed storage before any queue access.
        unsafe {
            let layout = std::alloc::Layout::new::<SpscVarQueue>();
            let pointer = std::alloc::alloc_zeroed(layout) as *mut SpscVarQueue;
            if pointer.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            SpscVarQueue::init_on_heap(pointer);
            pointer
        }
    }

    unsafe fn free_queue(pointer: *mut SpscVarQueue) {
        // SAFETY: callers retain exclusive ownership after endpoints stop.
        unsafe {
            let layout = std::alloc::Layout::new::<SpscVarQueue>();
            std::alloc::dealloc(pointer as *mut u8, layout);
        }
    }

    #[test]
    fn test_basic_alloc_push_front_pop() {
        // SAFETY: this test exclusively owns its allocation and both endpoints.
        unsafe {
            let queue = alloc_queue_on_heap();

            let header = SpscVarQueue::producer_alloc(queue, 16).unwrap();
            assert!(!header.is_null());

            let payload = MessageHeader::payload_pointer_raw(header);
            (payload as *mut u64).write_unaligned(0xDEADBEEF);
            (*header).push(16);

            let front = SpscVarQueue::consumer_front(queue).unwrap();
            let front_payload = MessageHeader::payload_pointer_raw(front) as *const u64;
            assert_eq!(front_payload.read_unaligned(), 0xDEADBEEF);

            SpscVarQueue::consumer_pop(queue);
            assert!(SpscVarQueue::consumer_front(queue).is_none());

            free_queue(queue);
        }
    }

    #[test]
    fn test_multiple_messages() {
        // SAFETY: this test exclusively owns its allocation and both endpoints.
        unsafe {
            let queue = alloc_queue_on_heap();

            for i in 0..4u32 {
                let header = SpscVarQueue::producer_alloc(queue, 4).unwrap();
                (MessageHeader::payload_pointer_raw(header) as *mut u32).write_unaligned(i * 10);
                (*header).push(4);
            }

            for i in 0..4u32 {
                let front = SpscVarQueue::consumer_front(queue).unwrap();
                let value =
                    (MessageHeader::payload_pointer_raw(front) as *const u32).read_unaligned();
                assert_eq!(value, i * 10);
                SpscVarQueue::consumer_pop(queue);
            }

            assert!(SpscVarQueue::consumer_front(queue).is_none());
            free_queue(queue);
        }
    }

    #[test]
    fn test_queue_full_returns_none() {
        // SAFETY: this test exclusively owns its allocation and both endpoints.
        unsafe {
            let queue = alloc_queue_on_heap();

            let big_size = MAX_MESSAGE_PAYLOAD_SIZE as u32;
            let result1 = SpscVarQueue::producer_alloc(queue, big_size);
            assert!(result1.is_some());
            let header = result1.unwrap();
            (*header).push(big_size);

            // Two maximum-size records fit, but a third cannot.
            let second = SpscVarQueue::producer_alloc(queue, big_size).unwrap();
            (*second).push(big_size);
            assert!(SpscVarQueue::producer_alloc(queue, big_size).is_none());

            free_queue(queue);
        }
    }
    #[test]
    fn rejects_oversize_without_overflow_or_state_change() {
        // SAFETY: one thread owns allocation, queue operations, and destruction.
        unsafe {
            let queue = alloc_queue_on_heap();
            for size in [MAX_MESSAGE_PAYLOAD_SIZE as u32 + 1, u32::MAX] {
                assert!(SpscVarQueue::producer_alloc(queue, size).is_none());
                assert!(SpscVarQueue::consumer_front(queue).is_none());
            }
            let h = SpscVarQueue::producer_alloc(queue, 0).unwrap();
            (*h).push(0);
            assert!(SpscVarQueue::consumer_front(queue).is_some());
            SpscVarQueue::consumer_pop(queue);
            free_queue(queue);
        }
    }

    #[test]
    fn maximum_records_progress_from_nonzero_empty_positions() {
        // SAFETY: one producer/consumer on this thread, no outstanding borrows.
        unsafe {
            let queue = alloc_queue_on_heap();
            for i in 0..12u64 {
                for size in [8, MAX_MESSAGE_PAYLOAD_SIZE as u32] {
                    let header = SpscVarQueue::producer_alloc(queue, size).unwrap();
                    MessageHeader::payload_pointer_raw(header).cast::<u64>().write_unaligned(i);
                    (*header).push(size);
                    let front = SpscVarQueue::consumer_front(queue).unwrap();
                    assert_eq!(
                        MessageHeader::payload_pointer_raw(front).cast::<u64>().read_unaligned(),
                        i
                    );
                    SpscVarQueue::consumer_pop(queue);
                    assert!(SpscVarQueue::consumer_front(queue).is_none());
                }
            }
            free_queue(queue);
        }
    }

    #[test]
    fn empty_wrap_sentinel_advances_before_head_publication() {
        // SAFETY: an empty queue is owned exclusively; the manual cursor setup
        // models an already-drained tail with only one block remaining.
        unsafe {
            let queue = alloc_queue_on_heap();
            UnsafeCell::raw_get(&raw const (*queue).write_index).write(BLOCK_COUNT - 1);
            UnsafeCell::raw_get(&raw const (*queue).free_write_count).write(1);
            (*queue).read_index.store(BLOCK_COUNT - 1, Ordering::Relaxed);
            let header =
                SpscVarQueue::producer_alloc(queue, MAX_MESSAGE_PAYLOAD_SIZE as u32).unwrap();
            assert_eq!(header, SpscVarQueue::header_at(queue, 0));
            assert!(SpscVarQueue::consumer_front(queue).is_none());
            assert_eq!((*queue).read_index.load(Ordering::Relaxed), 0);
            (*header).push(MAX_MESSAGE_PAYLOAD_SIZE as u32);
            assert_eq!(SpscVarQueue::consumer_front(queue).unwrap(), header);
            SpscVarQueue::consumer_pop(queue);
            free_queue(queue);
        }
    }

    #[derive(Clone, Copy)]
    struct QueuePointer(*mut SpscVarQueue);

    // SAFETY: the test creates exactly one producer and one consumer; accesses
    // use only the documented raw queue methods and publish/consume protocol.
    unsafe impl Send for QueuePointer {}

    impl QueuePointer {
        fn get(self) -> *mut SpscVarQueue {
            self.0
        }
    }

    #[test]
    fn concurrent_single_producer_single_consumer() {
        let count = if cfg!(miri) { 24 } else { 20_000 };
        let queue = alloc_queue_on_heap();
        let producer_queue = QueuePointer(queue);
        let consumer_queue = QueuePointer(queue);
        let producer = std::thread::spawn(move || {
            for value in 0..count {
                let size = if value % 3 == 0 { MAX_MESSAGE_PAYLOAD_SIZE as u32 } else { 8 };
                loop {
                    // SAFETY: this is the only producer; each reservation is
                    // initialized and published before reserving the next.
                    unsafe {
                        if let Some(header) =
                            SpscVarQueue::producer_alloc(producer_queue.get(), size)
                        {
                            MessageHeader::payload_pointer_raw(header)
                                .cast::<u64>()
                                .write_unaligned(value);
                            (*header).push(size);
                            break;
                        }
                    }
                    std::thread::yield_now();
                }
            }
        });
        let consumer = std::thread::spawn(move || {
            for expected in 0..count {
                loop {
                    // SAFETY: this is the only consumer; payload reads finish
                    // before consumer_pop releases the storage.
                    unsafe {
                        if let Some(header) = SpscVarQueue::consumer_front(consumer_queue.get()) {
                            assert_eq!(
                                MessageHeader::payload_pointer_raw(header)
                                    .cast::<u64>()
                                    .read_unaligned(),
                                expected
                            );
                            SpscVarQueue::consumer_pop(consumer_queue.get());
                            break;
                        }
                    }
                    std::thread::yield_now();
                }
            }
        });
        producer.join().unwrap();
        consumer.join().unwrap();
        // SAFETY: both endpoints have terminated and retain no queue pointers.
        unsafe {
            assert!(SpscVarQueue::consumer_front(queue).is_none());
            free_queue(queue);
        }
    }
}

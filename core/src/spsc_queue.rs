#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
use core::{
    cell::UnsafeCell,
    mem::{MaybeUninit, size_of},
};
use std::sync::atomic::{AtomicU32, Ordering};

use uarch::prelude::*;

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

bitword! {
    #[derive(Debug)]
    pub struct ProducerMeta(u64) {
        [0] pub WATERMARK_RUNG,
        [32..=63] pub FREE_BLOCKS: u32,
    }
}
assert_layout!(ProducerMeta, size = 8, align = 8);
#[repr(C)]
struct ProducerCursor {
    write_index: UnsafeCell<u32>,
    _reserved: u32,
    meta: UnsafeCell<ProducerMeta>,
}
#[repr(C)]
struct ConsumerCursor {
    read_index: AtomicU32,
}
assert_layout!(ProducerCursor, size = 16, align = 8, fields = { write_index: 0, meta: 8 });
assert_layout!(ConsumerCursor, size = 4, align = 4);
assert_layout!(Isolated<ProducerCursor>, size = 128, align = 128);
assert_layout!(Isolated<ConsumerCursor>, size = 128, align = 128);
/// Single-producer single-consumer variable-length lock-free ring queue.
///
/// The underlying storage uses `MaybeUninit<u64>` as raw byte blocks instead of
/// `[MessageHeader; N]`. This allows subsequent blocks to be safely used as raw
/// payload byte regions without overwriting initialized `MessageHeader`/`AtomicU32`
/// objects with invalid values.
#[repr(C)]
pub struct SpscVarQueue {
    storage: UnsafeCell<[MaybeUninit<u64>; BLOCK_COUNT as usize]>,
    producer: Isolated<ProducerCursor>,
    consumer: Isolated<ConsumerCursor>,
}
assert_layout!(SpscVarQueue, size = BUFFER_SIZE + 256, align = 128,
    fields = { storage: 0, producer: BUFFER_SIZE, consumer: BUFFER_SIZE + 128 });

impl SpscVarQueue {
    /// # Safety
    /// The caller exclusively owns a zeroed, unpublished allocation.
    pub(crate) unsafe fn init_on_heap(this: *mut Self) {
        let meta = ProducerMeta::new(0)
            .patch(BitPatch::EMPTY.with_field(ProducerMeta::FREE_BLOCKS, (BLOCK_COUNT / 8) as u64));
        // SAFETY: only this field is projected in the unpublished allocation.
        unsafe { UnsafeCell::raw_get(&raw const (*this).producer.value.meta).write(meta) };
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
    #[hot_inline]
    pub unsafe fn producer_alloc(this: *mut Self, size: u32) -> Option<*mut MessageHeader> {
        if unlikely(size as usize > MAX_MESSAGE_PAYLOAD_SIZE) {
            return None;
        }
        let required = PowerOfTwo::<3>::div_u64(size as u64 + (HEADER_SIZE * 2 - 1) as u64) as u32;
        // SAFETY: the sole producer owns its cursor; no payload-wide borrow is created.
        unsafe {
            let meta_ptr = UnsafeCell::raw_get(&raw const (*this).producer.value.meta);
            if unlikely(required > (*meta_ptr).get(ProducerMeta::FREE_BLOCKS)) {
                return Self::producer_alloc_slow(this, required);
            }
            Self::reserve_blocks(this, required)
        }
    }

    #[hot_inline]
    unsafe fn reserve_blocks(this: *mut Self, required: u32) -> Option<*mut MessageHeader> {
        // SAFETY: cached credits exclude the sentinel block; only the producer
        // mutates these fields and the unpublished next header.
        unsafe {
            let write_ptr = UnsafeCell::raw_get(&raw const (*this).producer.value.write_index);
            let meta_ptr = UnsafeCell::raw_get(&raw const (*this).producer.value.meta);
            let write = write_ptr.read();
            let meta = meta_ptr.read();
            let header = Self::header_at(this, write);
            let next = write + required;
            write_ptr.write(next);
            meta_ptr.write(meta.patch(BitPatch::EMPTY.with_field(
                ProducerMeta::FREE_BLOCKS,
                (meta.get(ProducerMeta::FREE_BLOCKS) - required) as u64,
            )));
            (*Self::header_at(this, next)).size.store(0, Ordering::Relaxed);
            Some(header)
        }
    }

    #[cold_sink]
    unsafe fn producer_alloc_slow(this: *mut Self, required: u32) -> Option<*mut MessageHeader> {
        // SAFETY: acquire observes released storage; the producer exclusively
        // owns the write cursor and can publish only its unused tail sentinel.
        unsafe {
            let write_ptr = UnsafeCell::raw_get(&raw const (*this).producer.value.write_index);
            let meta_ptr = UnsafeCell::raw_get(&raw const (*this).producer.value.meta);
            let read_pointer = &raw const (*this).consumer.value.read_index;
            let read = (*read_pointer).load(Ordering::Acquire);
            let mut write = write_ptr.read();
            let mut contiguous = if read <= write { BLOCK_COUNT - write } else { read - write };
            if required >= contiguous && read <= write && read != 0 {
                (*Self::header_at(this, 0)).size.store(0, Ordering::Relaxed);
                (*Self::header_at(this, write)).size.store(1, Ordering::Release);
                write = 0;
                write_ptr.write(write);
                contiguous = read;
            }
            let actual_free = if read <= write { BLOCK_COUNT - write + read } else { read - write };
            let occupied = BLOCK_COUNT - actual_free;
            let mut meta = meta_ptr.read();
            if occupied < BLOCK_COUNT / 4 {
                meta &= !ProducerMeta::WATERMARK_RUNG;
            }
            let post_occupied =
                if required < contiguous { occupied.saturating_add(required) } else { occupied };
            if post_occupied >= BLOCK_COUNT / 2 && !meta.test(ProducerMeta::WATERMARK_RUNG) {
                meta |= ProducerMeta::WATERMARK_RUNG;
                crate::contract::ring_doorbell(false);
            }
            // One block always remains available for the unpublished end marker.
            let credits = (contiguous - 1).min((BLOCK_COUNT / 8).max(required));
            meta_ptr.write(
                meta.patch(BitPatch::EMPTY.with_field(ProducerMeta::FREE_BLOCKS, credits as u64)),
            );
            if required > credits {
                return None;
            }
            Self::reserve_blocks(this, required)
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
            let read = &*std::ptr::addr_of!((*this).consumer.value.read_index);
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
            let read = &*std::ptr::addr_of!((*this).consumer.value.read_index);
            let index = read.load(Ordering::Relaxed);
            let size = (*Self::header_at(this, index)).size.load(Ordering::Relaxed);
            debug_assert!(size > 1);
            read.store(
                index + PowerOfTwo::<3>::div_u64(size as u64 + (HEADER_SIZE - 1) as u64) as u32,
                Ordering::Release,
            );
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn hold_doorbell() -> std::sync::MutexGuard<'static, ()> {
        crate::contract::DOORBELL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

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
        let _doorbell = hold_doorbell();
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
        let _doorbell = hold_doorbell();
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
        let _doorbell = hold_doorbell();
        // SAFETY: an empty queue is owned exclusively; the manual cursor setup
        // models an already-drained tail with only one block remaining.
        unsafe {
            let queue = alloc_queue_on_heap();
            UnsafeCell::raw_get(&raw const (*queue).producer.value.write_index)
                .write(BLOCK_COUNT - 1);
            UnsafeCell::raw_get(&raw const (*queue).producer.value.meta)
                .write(ProducerMeta::new(0));
            (*queue).consumer.value.read_index.store(BLOCK_COUNT - 1, Ordering::Relaxed);
            let header =
                SpscVarQueue::producer_alloc(queue, MAX_MESSAGE_PAYLOAD_SIZE as u32).unwrap();
            assert_eq!(header, SpscVarQueue::header_at(queue, 0));
            assert!(SpscVarQueue::consumer_front(queue).is_none());
            assert_eq!((*queue).consumer.value.read_index.load(Ordering::Relaxed), 0);
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
        let _doorbell = hold_doorbell();
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

    #[test]
    fn two_large_reservations_ring_watermark_doorbell() {
        use crate::contract::{DOORBELL, DOORBELL_PARKED_BIT, DoorbellWord};
        let _doorbell = hold_doorbell();
        let queue = alloc_queue_on_heap();
        let half_minus_one_blocks = BLOCK_COUNT / 2 - 1;
        let payload_bytes = (half_minus_one_blocks as usize * HEADER_SIZE - HEADER_SIZE) as u32;
        DOORBELL.store(DOORBELL_PARKED_BIT, Ordering::SeqCst);
        // SAFETY: single-threaded test owns the freshly allocated queue.
        unsafe {
            let h1 = SpscVarQueue::producer_alloc(queue, payload_bytes).unwrap();
            (*h1).push(payload_bytes);
            let meta1 = *UnsafeCell::raw_get(&raw const (*queue).producer.value.meta);
            assert!(!meta1.test(ProducerMeta::WATERMARK_RUNG));

            let seq_before =
                DoorbellWord::new(DOORBELL.load(Ordering::SeqCst)).get(DoorbellWord::RING_SEQ);
            let h2 = SpscVarQueue::producer_alloc(queue, payload_bytes).unwrap();
            (*h2).push(payload_bytes);
            let meta2 = *UnsafeCell::raw_get(&raw const (*queue).producer.value.meta);
            assert!(meta2.test(ProducerMeta::WATERMARK_RUNG));
            let seq_after =
                DoorbellWord::new(DOORBELL.load(Ordering::SeqCst)).get(DoorbellWord::RING_SEQ);
            assert!(seq_after > seq_before);
            free_queue(queue);
        }
        DOORBELL.store(0, Ordering::SeqCst);
    }
}

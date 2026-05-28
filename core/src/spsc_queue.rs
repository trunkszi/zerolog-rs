use core::mem::{MaybeUninit, size_of};
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
/// ferrilog = { version = "0.1", features = ["buffer-4m"] }
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

    /// Returns a pointer to the beginning of the payload (immediately after the header).
    #[inline(always)]
    pub fn payload_pointer(&self) -> *mut u8 {
        unsafe { (self as *const Self).add(1) as *mut u8 }
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
    storage: [MaybeUninit<u64>; BLOCK_COUNT as usize],
    /// Current write position in blocks (producer only).
    pub(crate) write_index: u32,
    /// Number of free blocks available for writing (producer only).
    pub(crate) free_write_count: u32,
    /// Padding to prevent false sharing between producer and consumer fields.
    _cache_line_padding: [u8; 120],
    /// Current read position in blocks (consumer only, atomically shared).
    read_index: AtomicU32,
}

impl SpscVarQueue {
    /// Returns an immutable pointer to the [`MessageHeader`] at the given block index.
    #[inline(always)]
    fn header_ptr_at(&self, block_index: u32) -> *const MessageHeader {
        debug_assert!(block_index < BLOCK_COUNT);
        unsafe { self.storage.as_ptr().add(block_index as usize) as *const MessageHeader }
    }

    /// Returns a mutable pointer to the [`MessageHeader`] at the given block index.
    #[inline(always)]
    fn header_mut_ptr_at(&mut self, block_index: u32) -> *mut MessageHeader {
        debug_assert!(block_index < BLOCK_COUNT);
        unsafe { self.storage.as_mut_ptr().add(block_index as usize) as *mut MessageHeader }
    }

    /// Allocates space on the producer side (called only by the writer thread).
    ///
    /// # Safety
    /// Must be called via a raw pointer, and only the writer thread may call this method.
    #[inline(always)]
    pub unsafe fn producer_alloc(this: *mut Self, size: u32) -> Option<*mut MessageHeader> {
        unsafe {
            let queue = &mut *this;
            let block_size = (size + HEADER_SIZE as u32 * 2 - 1) / HEADER_SIZE as u32;

            // Fast path: enough space available without checking read_index.
            // The unlikely hint tells LLVM to lay out the slow path out-of-line.
            if core::intrinsics::unlikely(block_size >= queue.free_write_count) {
                let read_index_cache = queue.read_index.load(Ordering::Acquire);

                if read_index_cache <= queue.write_index {
                    queue.free_write_count = BLOCK_COUNT - queue.write_index;
                    if block_size >= queue.free_write_count && read_index_cache != 0 {
                        // wrap around: write sentinel and reset write_index to zero
                        (*queue.header_mut_ptr_at(0)).size.store(0, Ordering::Release);
                        (*queue.header_mut_ptr_at(queue.write_index))
                            .size
                            .store(1, Ordering::Release);
                        queue.write_index = 0;
                        queue.free_write_count = read_index_cache;
                    }
                } else {
                    queue.free_write_count = read_index_cache - queue.write_index;
                }

                if queue.free_write_count <= block_size {
                    return None;
                }
            }

            let result = queue.header_mut_ptr_at(queue.write_index);
            queue.write_index += block_size;
            queue.free_write_count -= block_size;
            (*queue.header_mut_ptr_at(queue.write_index)).size.store(0, Ordering::Relaxed);
            Some(result)
        }
    }

    /// Peeks at the front message on the consumer side (called only by the background thread).
    /// This only observes the message without advancing `read_index`.
    ///
    /// # Safety
    /// Must be called via a raw pointer, and only the reader thread may call this method.
    #[inline]
    pub unsafe fn consumer_front(this: *const Self) -> Option<*const MessageHeader> {
        unsafe {
            let queue = &*this;
            let index = queue.read_index.load(Ordering::Relaxed);
            let size = (*queue.header_ptr_at(index)).size.load(Ordering::Acquire);

            if size == 1 {
                let size_at_zero = (*queue.header_ptr_at(0)).size.load(Ordering::Acquire);
                if size_at_zero == 0 { None } else { Some(queue.header_ptr_at(0)) }
            } else if size == 0 {
                None
            } else {
                Some(queue.header_ptr_at(index))
            }
        }
    }

    /// Pops the front message on the consumer side (called only by the background thread).
    ///
    /// # Safety
    /// Must be called only after `consumer_front` has returned `Some`.
    #[inline]
    pub unsafe fn consumer_pop(this: *mut Self) {
        unsafe {
            let queue = &mut *this;
            let current_read_index = queue.read_index.load(Ordering::Relaxed);
            let current_size =
                (*queue.header_ptr_at(current_read_index)).size.load(Ordering::Relaxed);

            if current_size == 1 {
                let size_at_zero = (*queue.header_ptr_at(0)).size.load(Ordering::Relaxed);
                let block_size = size_at_zero.div_ceil(HEADER_SIZE as u32);
                queue.read_index.store(block_size, Ordering::Release);
            } else {
                let block_size = current_size.div_ceil(HEADER_SIZE as u32);
                queue.read_index.store(current_read_index + block_size, Ordering::Release);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alloc_queue_on_heap() -> *mut SpscVarQueue {
        unsafe {
            let layout = std::alloc::Layout::new::<SpscVarQueue>();
            let pointer = std::alloc::alloc_zeroed(layout) as *mut SpscVarQueue;
            if pointer.is_null() {
                std::alloc::handle_alloc_error(layout);
            }
            (*pointer).free_write_count = BLOCK_COUNT;
            pointer
        }
    }

    unsafe fn free_queue(pointer: *mut SpscVarQueue) {
        unsafe {
            let layout = std::alloc::Layout::new::<SpscVarQueue>();
            std::alloc::dealloc(pointer as *mut u8, layout);
        }
    }

    #[test]
    fn test_basic_alloc_push_front_pop() {
        unsafe {
            let queue = alloc_queue_on_heap();

            let header = SpscVarQueue::producer_alloc(queue, 16).unwrap();
            assert!(!header.is_null());

            let payload = (*header).payload_pointer();
            (payload as *mut u64).write_unaligned(0xDEADBEEF);
            (*header).push(16);

            let front = SpscVarQueue::consumer_front(queue).unwrap();
            let front_payload = (*front).payload_pointer() as *const u64;
            assert_eq!(front_payload.read_unaligned(), 0xDEADBEEF);

            SpscVarQueue::consumer_pop(queue);
            assert!(SpscVarQueue::consumer_front(queue).is_none());

            free_queue(queue);
        }
    }

    #[test]
    fn test_multiple_messages() {
        unsafe {
            let queue = alloc_queue_on_heap();

            for i in 0..4u32 {
                let header = SpscVarQueue::producer_alloc(queue, 4).unwrap();
                ((*header).payload_pointer() as *mut u32).write_unaligned(i * 10);
                (*header).push(4);
            }

            for i in 0..4u32 {
                let front = SpscVarQueue::consumer_front(queue).unwrap();
                let value = ((*front).payload_pointer() as *const u32).read_unaligned();
                assert_eq!(value, i * 10);
                SpscVarQueue::consumer_pop(queue);
            }

            assert!(SpscVarQueue::consumer_front(queue).is_none());
            free_queue(queue);
        }
    }

    #[test]
    fn test_queue_full_returns_none() {
        unsafe {
            let queue = alloc_queue_on_heap();

            let big_size = BUFFER_SIZE as u32 / 2;
            let result1 = SpscVarQueue::producer_alloc(queue, big_size);
            assert!(result1.is_some());
            let header = result1.unwrap();
            (*header).push(big_size);

            // Allocating the same size again should fail
            let result2 = SpscVarQueue::producer_alloc(queue, big_size);
            assert!(result2.is_none());

            free_queue(queue);
        }
    }
}

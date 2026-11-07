#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Microarchitecture probes for logging contracts.

use std::sync::atomic::Ordering;

use uarch::prelude::{predicate, unlikely};
use zerolog_core::{
    contract::{HeapNode, LEVEL_GATE, LevelGateWord, RecordCursor},
    header::HeaderDateCache,
    number_format::format_u64,
    spsc_queue::{MessageHeader, SpscVarQueue},
};

/// Payload bytes reserved by [`probe_spsc_alloc_push`].
const MESSAGE_PAYLOAD: u32 = 32;

/// One relaxed load of `LEVEL_GATE`, then a cold doorbell when the level rings.
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn probe_level_gate_check(level: u8) -> u32 {
    let raw = LEVEL_GATE.load(Ordering::Relaxed);
    let gate = LevelGateWord::new(raw);
    let min_level = gate.get(LevelGateWord::MIN_LEVEL);
    let ring_level = gate.get(LevelGateWord::RING_LEVEL);
    if unlikely(level >= ring_level) {
        zerolog_core::contract::ring_doorbell(false);
    }
    predicate(level >= min_level).select_register(raw, 0)
}

/// Reserves a 32-byte payload, stores `payload_val`, and publishes it.
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn probe_spsc_alloc_push(queue: &SpscVarQueue, payload_val: u64) -> bool {
    // SAFETY: the caller keeps this queue live and is its only producer.
    unsafe { publish_payload(queue, payload_val) }
}

/// # Safety
/// `queue` is live and this thread is its only producer, with no outstanding reservation.
unsafe fn publish_payload(queue: &SpscVarQueue, payload_val: u64) -> bool {
    let queue_ptr = std::ptr::from_ref(queue).cast_mut();
    // SAFETY: exclusive producer, one reservation, payload fits the ring bound.
    let Some(header) = (unsafe { SpscVarQueue::producer_alloc(queue_ptr, MESSAGE_PAYLOAD) }) else {
        return false;
    };
    // SAFETY: the reservation is unpublished and holds 32 payload bytes.
    unsafe {
        MessageHeader::payload_pointer_raw(header).cast::<u64>().write_unaligned(payload_val);
        (*header).push(MESSAGE_PAYLOAD);
    }
    true
}

/// Returns the earlier node in registers. Equal timestamps keep `a`.
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn probe_heap_node_pass(a: HeapNode, b: HeapNode) -> HeapNode {
    let earlier = predicate(a.raw_timestamp <= b.raw_timestamp);
    HeapNode {
        raw_timestamp: earlier.select_register(a.raw_timestamp, b.raw_timestamp),
        cursor: RecordCursor::new(earlier.select_register(a.cursor.raw(), b.cursor.raw())),
    }
}

/// Writes `YYYY-MM-DD HH:MM:SS.nnnnnnnnn` into `out` through the date cache.
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn probe_header_format(
    cache: &mut HeaderDateCache,
    ts_ns: i64,
    out: &mut [u8; 64],
) -> usize {
    let mut bytes = Vec::with_capacity(64);
    cache.append_date(&mut bytes, ts_ns);
    bytes.push(b' ');
    cache.append_time(ts_ns, &mut bytes);
    bytes.push(b'.');
    cache.append_nanoseconds(ts_ns, &mut bytes);
    let written = bytes.len();
    if written > out.len() {
        return 0;
    }
    out[..written].copy_from_slice(&bytes);
    written
}

/// Formats `val` with [`format_u64`] into `out`.
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn probe_format_u64(val: u64, out: &mut [u8; 24]) -> usize {
    format_u64(out, val)
}

/// Publishes the same fixed u32 payload and gate sequence as a producer log site.
/// The caller is the queue's only producer; `info` remains valid for consumer decoding.
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn probe_log_u32(
    queue: &SpscVarQueue,
    info: &'static zerolog_core::StaticInfo,
    value: u32,
) -> bool {
    use zerolog_core::{
        EncodeFast, PAYLOAD_ARGS_OFFSET, PAYLOAD_INFO_OFFSET, PAYLOAD_SPAN_ID_OFFSET,
        PAYLOAD_TIMESTAMP_OFFSET, TimestampCounter, thread_buffer,
    };
    if !zerolog_core::check_level(info.level) {
        return false;
    }
    let span = zerolog_core::current_span_id_u64();
    let size = (PAYLOAD_ARGS_OFFSET + 4) as u32;
    // SAFETY: this call has exclusive producer access and no prior reservation.
    let Some(header) =
        (unsafe { SpscVarQueue::producer_alloc(std::ptr::from_ref(queue).cast_mut(), size) })
    else {
        return false;
    };
    // SAFETY: the reservation includes the metadata and fixed-width u32 payload.
    unsafe {
        let payload = MessageHeader::payload_pointer_raw(header);
        payload
            .add(PAYLOAD_INFO_OFFSET)
            .cast::<*const zerolog_core::StaticInfo>()
            .write_unaligned(info);
        payload
            .add(PAYLOAD_TIMESTAMP_OFFSET)
            .cast::<i64>()
            .write_unaligned(TimestampCounter::read_timestamp_counter());
        payload.add(PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().write_unaligned(span);
        value.encode_fast_at_offset(payload, PAYLOAD_ARGS_OFFSET);
        (*header).push(size);
    }
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    if thread_buffer::should_ring_gate(thread_buffer::load_level_gate(), info.level as u8) {
        thread_buffer::notify_urgent();
    }
    true
}

/// Removes the earliest available queue-front node using the real merge heap.
#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn probe_merge_pop(heap: &mut zerolog_core::poller::ReadyMessageHeap) -> HeapNode {
    heap.pop().unwrap_or(HeapNode { raw_timestamp: i64::MAX, cursor: RecordCursor::new(0) })
}

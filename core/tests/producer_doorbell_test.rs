#![deny(clippy::unwrap_used, clippy::expect_used)]
use std::sync::atomic::Ordering;

use zerolog_core::{
    contract::{DOORBELL, DoorbellWord},
    spsc_queue::{BLOCK_COUNT, HEADER_SIZE, SpscVarQueue},
    thread_buffer::ThreadBuffer,
};

struct Buffer(*mut ThreadBuffer);
impl Buffer {
    fn new() -> Self {
        Self(ThreadBuffer::new_on_heap())
    }
    fn queue(&self) -> *mut SpscVarQueue {
        // SAFETY: the buffer allocation is live until this owner's drop.
        unsafe { &raw mut (*self.0).queue }
    }
}
impl Drop for Buffer {
    fn drop(&mut self) {
        // SAFETY: this unregistered allocation has no other owners or borrows.
        unsafe { ThreadBuffer::free_on_heap(self.0) };
    }
}
fn sequence() -> u32 {
    DoorbellWord::new(DOORBELL.load(Ordering::Acquire)).get(DoorbellWord::RING_SEQ)
}

fn watermark_rearms_after_drain() {
    let buffer = Buffer::new();
    let payload = (BLOCK_COUNT / 8) * HEADER_SIZE as u32 - HEADER_SIZE as u32;
    let initial = sequence();
    // SAFETY: this thread exclusively owns both endpoints of an unpublished queue.
    unsafe {
        for cycle in 0..2 {
            let mut accepted = 0;
            while let Some(header) = SpscVarQueue::producer_alloc(buffer.queue(), payload) {
                (*header).push(payload);
                accepted += 1;
                assert!(accepted <= 8);
            }
            assert!(accepted >= 4);
            assert_eq!(sequence(), initial + cycle + 1);
            assert!(SpscVarQueue::producer_alloc(buffer.queue(), payload).is_none());
            assert_eq!(sequence(), initial + cycle + 1);
            while SpscVarQueue::consumer_front(buffer.queue()).is_some() {
                SpscVarQueue::consumer_pop(buffer.queue());
            }
        }
    }
}

#[test]
fn producer_contract_scenarios() {
    trace_follows_check_level_not_gate_min();
    watermark_rearms_after_drain();
    cached_gate_cannot_miss_park();
    use zerolog_core::{
        contract::{LEVEL_GATE, LevelGateWord},
        thread_buffer::*,
    };
    let gate = LevelGateWord::pack(2, 4, false);
    LEVEL_GATE.store(gate, Ordering::Release);
    assert_eq!(load_level_gate(), gate);
    assert!(!passes_level_gate(gate, 1));
    assert!(passes_level_gate(gate, 2));
    assert!(!should_ring_gate(gate, 3));
    assert!(should_ring_gate(gate, 4));
    let before = sequence();
    notify_urgent();
    assert_eq!(sequence(), before + 1);
    assert!(DoorbellWord::new(DOORBELL.load(Ordering::Acquire)).test(DoorbellWord::URGENT));
    preallocate();
    retired_slots_increment_epoch();
    let pointer = get_thread_buffer();
    assert!(!pointer.is_null());
    // SAFETY: this thread owns the producer, which cannot retire during this access.
    unsafe {
        let slot = (*pointer).slot_id() as usize;
        assert!((*pointer).slot_epoch() > 0);
        assert_eq!(THREAD_BUFFER_REGISTRY.get(slot), pointer);
        assert_ne!(
            zerolog_core::contract::REGISTERED_SLOTS[slot / 64].load(Ordering::Acquire)
                & (1 << (slot % 64)),
            0
        );
        #[cfg(all(target_os = "linux", not(miri)))]
        assert_eq!(pointer.addr() & ((2 * 1024 * 1024) - 1), 0);
    }
}

fn retired_slots_increment_epoch() {
    use zerolog_core::thread_buffer::{THREAD_BUFFER_REGISTRY, get_thread_buffer, preallocate};
    let mut previous = None;
    for _ in 0..2 {
        let before = sequence();
        let worker = std::thread::spawn(|| {
            preallocate();
            let buffer = get_thread_buffer();
            // SAFETY: immutable identity belongs to this live producer.
            unsafe { ((*buffer).slot_id(), (*buffer).slot_epoch(), buffer.addr()) }
        });
        let (slot, epoch, address) = match worker.join() {
            Ok(value) => value,
            Err(_) => panic!("registration worker panicked"),
        };
        assert_eq!(sequence(), before + 1);
        assert_eq!(THREAD_BUFFER_REGISTRY.get(slot as usize).addr(), address);
        if let Some((old_slot, old_epoch)) = previous {
            assert_eq!(slot, old_slot);
            assert_eq!(epoch, old_epoch + 1);
        }
        zerolog_core::poller::cleanup_finished_buffers();
        assert!(THREAD_BUFFER_REGISTRY.get(slot as usize).is_null());
        assert_eq!(
            zerolog_core::contract::REGISTERED_SLOTS[slot as usize / 64].load(Ordering::Acquire)
                & (1 << (slot % 64)),
            0
        );
        previous = Some((slot, epoch));
    }
}

fn cached_gate_cannot_miss_park() {
    use zerolog_core::{
        contract::{DOORBELL_PARKED_BIT, LEVEL_GATE, LevelGateWord},
        thread_buffer::*,
    };
    LEVEL_GATE.store(LevelGateWord::pack(2, 4, false), Ordering::SeqCst);
    DOORBELL.store(0, Ordering::SeqCst);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (resume_tx, resume_rx) = std::sync::mpsc::channel();
    let producer = std::thread::spawn(move || {
        let buffer = Buffer::new();
        let cached = load_level_gate();
        assert!(passes_level_gate(cached, 2));
        assert!(ready_tx.send(()).is_ok());
        assert!(resume_rx.recv().is_ok());
        // SAFETY: the producer exclusively owns this local queue allocation.
        unsafe {
            let header = match SpscVarQueue::producer_alloc(buffer.queue(), 8) {
                Some(header) => header,
                None => panic!("empty queue rejected record"),
            };
            (*header).push(8);
        }
        std::sync::atomic::compiler_fence(Ordering::SeqCst);
        if should_ring_gate(load_level_gate(), 2) {
            notify_urgent();
        }
    });
    assert!(ready_rx.recv().is_ok());
    // The producer has cached the old gate but has not published its record.
    LEVEL_GATE.store(LevelGateWord::pack(2, 0, true), Ordering::SeqCst);
    DOORBELL.store(DOORBELL_PARKED_BIT, Ordering::SeqCst);
    assert!(resume_tx.send(()).is_ok());
    assert!(producer.join().is_ok());
    let observed = sequence();
    DOORBELL.store(0, Ordering::SeqCst);
    assert_eq!(observed, 1, "publication after deep-park arming must notify");
}

fn trace_follows_check_level_not_gate_min() {
    use zerolog_core::{
        Level, check_level,
        contract::{LEVEL_GATE, LevelGateWord},
        set_level,
        thread_buffer::{load_level_gate, passes_level_gate},
    };
    LEVEL_GATE.store(LevelGateWord::pack(1, 5, false), Ordering::SeqCst);
    set_level(Level::TRC);
    assert!(check_level(Level::TRC));
    assert!(passes_level_gate(load_level_gate(), Level::TRC as u8));
    set_level(Level::DBG);
}

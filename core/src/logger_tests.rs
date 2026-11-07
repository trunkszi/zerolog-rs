#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

use super::*;

std::thread_local! { static ALLOCATIONS: Cell<Option<usize>> = const { Cell::new(None) }; }
static REGISTRY_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct CountingAllocator;
fn count_allocation() {
    let _ = ALLOCATIONS.try_with(|c| {
        if let Some(v) = c.get() {
            c.set(Some(v + 1))
        }
    });
}
// SAFETY: all allocations retain System's layouts, ownership and pointer contracts.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        // SAFETY: forwarded with the caller's unchanged allocation contract.
        unsafe { System.alloc(layout) }
    }
    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        count_allocation();
        // SAFETY: forwarded with the caller's unchanged allocation contract.
        unsafe { System.alloc_zeroed(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: System created the allocation under this exact layout.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation();
        // SAFETY: forwarded with the caller's unchanged reallocation contract.
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn publish_empty_body() {
    unsafe fn decode(_: *const u8, _: &mut Vec<u8>) {}
    static INFO: StaticInfo =
        StaticInfo { level: Level::INF, location: "scratch-test", format_string: "", decode };
    let size = crate::spsc_queue::PAYLOAD_HEADER_SIZE;
    let header = crate::thread_buffer::alloc_message(size as u32).unwrap();
    // SAFETY: this thread owns the reservation until Release publication.
    unsafe {
        let payload = MessageHeader::payload_pointer_raw(header);
        payload.add(PAYLOAD_INFO_OFFSET).cast::<*const StaticInfo>().write_unaligned(&INFO);
        payload
            .add(crate::spsc_queue::PAYLOAD_TIMESTAMP_OFFSET)
            .cast::<i64>()
            .write_unaligned(TimestampCounter::read_timestamp_counter());
        payload.add(PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().write_unaligned(0);
        (*header).push(size as u32);
    }
}

#[test]
fn warmed_poll_reuses_heap_storage() {
    let _guard = REGISTRY_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    set_output_writer(io::sink());
    set_header_pattern("").unwrap();
    publish_empty_body();
    poll(true).unwrap();
    for _ in 0..10 {
        publish_empty_body();
        ALLOCATIONS.set(Some(0));
        let result = poll(true);
        let allocations = ALLOCATIONS.replace(None).unwrap();
        result.unwrap();
        assert_eq!(allocations, 0, "warmed scalar consumer must reuse scratch");
    }
}

#[test]
fn local_consumer_serializes_poll_and_config() {
    let _guard = REGISTRY_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let logger = std::sync::Arc::new(GlobalLogger::new());
    logger
        .configure(|inner| {
            inner.output.set_writer(Box::new(io::sink()));
            Ok(())
        })
        .unwrap();
    let (config_logger, poll_logger) = (logger.clone(), logger.clone());
    let config = std::thread::spawn(move || {
        for _ in 0..if cfg!(miri) { 4 } else { 50 } {
            config_logger
                .configure(|inner| {
                    inner.next_flush_deadline_nanoseconds = Some(123);
                    std::thread::yield_now();
                    assert_eq!(inner.next_flush_deadline_nanoseconds, Some(123));
                    Ok(())
                })
                .unwrap();
        }
    });
    let poller = std::thread::spawn(move || {
        for _ in 0..if cfg!(miri) { 4 } else { 50 } {
            let mut inner = poll_logger.acquire_consumer();
            poll_inner(&mut inner, true).unwrap();
            std::thread::yield_now();
            assert!(inner.next_flush_deadline_nanoseconds.is_none());
        }
    });
    config.join().unwrap();
    poller.join().unwrap();
    assert!(logger.acquire_consumer().scratch_heap.pop().is_none());
}

#[test]
fn guards_release_after_config_unwind() {
    let _guard = REGISTRY_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let logger = GlobalLogger::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: io::Result<()> = logger.configure(|inner| {
            inner.next_flush_deadline_nanoseconds = Some(456);
            panic!("configuration panic");
        });
    }));
    assert!(result.is_err());
    logger
        .configure(|inner| {
            assert_eq!(inner.next_flush_deadline_nanoseconds, Some(456));
            inner.output.set_writer(Box::new(io::sink()));
            Ok(())
        })
        .unwrap();
    let mut inner = logger.acquire_consumer();
    poll_inner(&mut inner, true).unwrap();
    assert!(inner.next_flush_deadline_nanoseconds.is_none());
}

#[test]
fn reentrant_poll_and_block_and_callback_safety() {
    let _guard = REGISTRY_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    set_output_writer(io::sink());
    set_header_pattern("").unwrap();
    set_level(Level::TRC);
    set_queue_full_policy(QueueFullPolicy::Block);

    static CALLBACK_CALLS: AtomicU32 = AtomicU32::new(0);
    fn recursive_cb(_count: u64) {
        CALLBACK_CALLS.fetch_add(1, Ordering::Relaxed);
        let nested = crate::thread_buffer::alloc_message(u32::MAX / 2);
        assert!(nested.is_none());
    }
    CALLBACK_CALLS.store(0, Ordering::Relaxed);
    reset_queue_full_count();
    set_queue_full_callback(Some(recursive_cb));

    unsafe fn reentrant_decode(_: *const u8, out: &mut Vec<u8>) {
        for force_flush in [false, true] {
            match poll(force_flush) {
                Err(error) => assert_eq!(error.kind(), io::ErrorKind::WouldBlock),
                Ok(()) => panic!("reentrant poll returned success"),
            }
        }
        assert!(matches!(handle_queue_full(), QueueFullAction::Drop));
        let over = crate::thread_buffer::alloc_message(u32::MAX / 2);
        assert!(over.is_none());
        out.extend_from_slice(b"reentrant-ok");
    }
    static REENTRANT_INFO: StaticInfo = StaticInfo {
        level: Level::INF,
        location: "reentrant-test",
        format_string: "",
        decode: reentrant_decode,
    };
    let size = crate::spsc_queue::PAYLOAD_HEADER_SIZE;
    let header = crate::thread_buffer::alloc_message(size as u32).unwrap();
    // SAFETY: this thread owns the reservation until Release publication.
    unsafe {
        let payload = MessageHeader::payload_pointer_raw(header);
        payload
            .add(PAYLOAD_INFO_OFFSET)
            .cast::<*const StaticInfo>()
            .write_unaligned(&REENTRANT_INFO);
        payload
            .add(crate::spsc_queue::PAYLOAD_TIMESTAMP_OFFSET)
            .cast::<i64>()
            .write_unaligned(TimestampCounter::read_timestamp_counter());
        payload.add(PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().write_unaligned(0);
        (*header).push(size as u32);
    }

    poll(true).unwrap();
    assert_eq!(CALLBACK_CALLS.load(Ordering::Relaxed), 2);
    assert_eq!(queue_full_count(), 4);

    set_queue_full_callback(None);
    set_queue_full_policy(QueueFullPolicy::Drop);
    reset_queue_full_count();
}

#[test]
fn level_gate_concurrent_updates_preserve_fields() {
    let _guard = REGISTRY_TEST_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let iterations = if cfg!(miri) { 8 } else { 200 };
    let t1 = std::thread::spawn(move || {
        for i in 0..iterations {
            set_level(if i % 2 == 0 { Level::WRN } else { Level::DBG });
        }
        set_level(Level::WRN);
    });
    let t2 = std::thread::spawn(move || {
        for i in 0..iterations {
            flush_on(if i % 2 == 0 { Level::ERR } else { Level::OFF });
        }
        flush_on(Level::ERR);
    });
    let t3 = std::thread::spawn(move || {
        for i in 0..iterations {
            sync_level_gate(Some(i % 2 == 0));
        }
        sync_level_gate(Some(false));
    });
    t1.join().unwrap();
    t2.join().unwrap();
    t3.join().unwrap();

    let gate = LevelGateWord::new(LEVEL_GATE.load(Ordering::SeqCst));
    assert_eq!(gate.get(LevelGateWord::MIN_LEVEL), Level::WRN as u8);
    assert_eq!(gate.get(LevelGateWord::RING_LEVEL), Level::ERR as u8);
    assert!(!gate.test(LevelGateWord::DEEP_PARK));

    set_level(Level::DBG);
    flush_on(Level::OFF);
}

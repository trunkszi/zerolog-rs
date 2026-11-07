#![deny(clippy::unwrap_used, clippy::expect_used)]

use std::{
    io::{self, Write},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use zerolog_core::{
    ConsumerCore, DrainBudget, DrainFlags, INFO, Level, LevelGateWord, OFF, PAYLOAD_ARGS_OFFSET,
    PAYLOAD_HEADER_SIZE, PAYLOAD_INFO_OFFSET, PAYLOAD_SPAN_ID_OFFSET, PAYLOAD_TIMESTAMP_OFFSET,
    StaticInfo, TimestampCounter, WRN, WakePolicy, ZeroLogError,
    builder::{acquire_consumer, builder, poll_batch},
    contract::{LEVEL_GATE, QUEUE_FULL_COUNT, ring_doorbell},
    flush_on,
    logger::{set_deep_park_transition_hook, set_flush_on, set_membarrier_state_for_test},
    output::OutputBuffer,
    poll,
    poller::{PollerState, ReadyMessageHeap},
    queue_full_count, reset_queue_full_count, set_flush_buffer_size, set_flush_delay,
    set_header_pattern, set_level, set_log_file, set_output_writer,
    spsc_queue::{MessageHeader, SpscVarQueue},
    start_polling_thread, stop_polling_thread,
    thread_buffer::{THREAD_BUFFER_REGISTRY, ThreadBuffer, alloc_message},
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn engine_test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

struct SharedVecWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl Write for SharedVecWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Ok(mut guard) = self.bytes.lock() {
            guard.extend_from_slice(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

unsafe fn decode_u64_tag(payload: *const u8, out: &mut Vec<u8>) {
    // SAFETY: caller encodes an 8-byte u64 at payload offset 0.
    let val = unsafe { payload.cast::<u64>().read_unaligned() };
    out.extend_from_slice(b"msg=");
    zerolog_core::number_format::append_u64(out, val);
}

static TAG_INFO: StaticInfo = StaticInfo {
    level: INFO,
    location: "engine_test",
    format_string: "msg={}",
    decode: decode_u64_tag,
};

fn publish_tag(value: u64, raw_ts: i64) -> Result<(), &'static str> {
    let total = (PAYLOAD_HEADER_SIZE + core::mem::size_of::<u64>()) as u32;
    let header = alloc_message(total).ok_or("alloc_message failed")?;
    // SAFETY: reservation is owned by this thread until `push`.
    unsafe {
        let payload = MessageHeader::payload_pointer_raw(header);
        payload.add(PAYLOAD_INFO_OFFSET).cast::<*const StaticInfo>().write_unaligned(&TAG_INFO);
        payload.add(PAYLOAD_TIMESTAMP_OFFSET).cast::<i64>().write_unaligned(raw_ts);
        payload.add(PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().write_unaligned(0);
        payload.add(PAYLOAD_ARGS_OFFSET).cast::<u64>().write_unaligned(value);
        (*header).push(total);
    }
    Ok(())
}

#[test]
fn slab_freeze_short_write_and_restore() -> TestResult {
    // 验证 OutputBuffer 的 FrozenSlab 冻结、短写推进、缓冲池回收与失败恢复保序
    let mut output = OutputBuffer::new();
    output.set_header_pattern("")?;
    let captured = Arc::new(Mutex::new(Vec::new()));
    output.set_writer(Box::new(SharedVecWriter { bytes: Arc::clone(&captured) }));

    assert!(output.freeze_pending().is_none());

    output.write_record(0, INFO, b"t", "loc", 0, |buf| buf.extend_from_slice(b"first"))?;
    let slab = output.freeze_pending().ok_or("expected frozen slab")?;
    let first_slab_ptr = slab.active_slice().as_ptr();
    assert_eq!(slab.active_slice(), b"first\n");
    assert_eq!(output.buffered_len(), 0);

    let slab =
        output.complete_frozen(slab, 2).ok_or("expected remaining suffix after short write")?;
    assert_eq!(slab.active_slice(), b"rst\n");

    output.write_record(0, INFO, b"t", "loc", 0, |buf| buf.extend_from_slice(b"second"))?;
    output.restore_failed_slab(slab);
    assert!(output.has_pending());
    assert_eq!(output.buffered_len(), b"rst\nsecond\n".len());

    let merged_slab = output.freeze_pending().ok_or("expected merged slab")?;
    let second_slab_ptr = merged_slab.active_slice().as_ptr();
    assert_eq!(merged_slab.active_slice(), b"rst\nsecond\n");
    assert!(output.complete_frozen(merged_slab, 11).is_none());

    output.write_record(0, INFO, b"t", "loc", 0, |buf| buf.extend_from_slice(b"reused1"))?;
    let reused_first = output.freeze_pending().ok_or("expected reused first slab")?;
    assert_eq!(reused_first.active_slice().as_ptr(), first_slab_ptr);
    assert_eq!(reused_first.active_slice(), b"reused1\n");
    assert!(output.complete_frozen(reused_first, 8).is_none());

    output.write_record(0, INFO, b"t", "loc", 0, |buf| buf.extend_from_slice(b"reused2"))?;
    let reused_second = output.freeze_pending().ok_or("expected reused second slab")?;
    assert_eq!(reused_second.active_slice().as_ptr(), second_slab_ptr);
    assert_eq!(reused_second.active_slice(), b"reused2\n");
    assert!(output.complete_frozen(reused_second, 8).is_none());
    Ok(())
}

#[test]
fn merge_heap_orders_by_timestamp_and_cursor() -> TestResult {
    // 验证 16B HeapNode 按 (raw_timestamp, cursor) 最小堆排序与槽位解析
    let first = ThreadBuffer::new_on_heap();
    let second = ThreadBuffer::new_on_heap();
    let push_raw = |buffer: *mut ThreadBuffer, ts: i64| -> Result<(), &'static str> {
        // SAFETY: test exclusively owns both buffers.
        unsafe {
            let header =
                SpscVarQueue::producer_alloc(&raw mut (*buffer).queue, PAYLOAD_HEADER_SIZE as u32)
                    .ok_or("alloc failed")?;
            let payload = MessageHeader::payload_pointer_raw(header);
            payload.write_bytes(0, PAYLOAD_HEADER_SIZE);
            payload.add(PAYLOAD_TIMESTAMP_OFFSET).cast::<i64>().write_unaligned(ts);
            (*header).push(PAYLOAD_HEADER_SIZE as u32);
        }
        Ok(())
    };
    push_raw(first, 100)?;
    push_raw(first, 300)?;
    push_raw(second, 100)?;
    push_raw(second, 200)?;

    // SAFETY: test exclusively owns `first` and `second`.
    unsafe {
        let mut heap = ReadyMessageHeap::from_buffers(&[first, second]);
        assert_eq!(heap.len(), 2);
        assert!(heap.has_slot(0));
        assert!(heap.has_slot(1));

        let n1 = heap.pop().ok_or("expected node 1")?;
        assert_eq!(n1.raw_timestamp(), 100);
        assert_eq!(n1.slot(), 0);
        let b1 = heap.resolve_slot(n1.cursor()).ok_or("expected slot 0")?;
        SpscVarQueue::consumer_pop(&raw mut (*b1.as_ptr()).queue);
        heap.refresh_cursor(n1.cursor());

        let n2 = heap.pop().ok_or("expected node 2")?;
        assert_eq!(n2.raw_timestamp(), 100);
        assert_eq!(n2.slot(), 1);
        let b2 = heap.resolve_slot(n2.cursor()).ok_or("expected slot 1")?;
        SpscVarQueue::consumer_pop(&raw mut (*b2.as_ptr()).queue);
        heap.refresh_cursor(n2.cursor());

        let n3 = heap.pop().ok_or("expected node 3")?;
        assert_eq!(n3.raw_timestamp(), 200);
        assert_eq!(n3.slot(), 1);
        let b3 = heap.resolve_slot(n3.cursor()).ok_or("expected slot 1")?;
        SpscVarQueue::consumer_pop(&raw mut (*b3.as_ptr()).queue);
        heap.refresh_cursor(n3.cursor());

        let n4 = heap.pop().ok_or("expected node 4")?;
        assert_eq!(n4.raw_timestamp(), 300);
        assert_eq!(n4.slot(), 0);
        let b4 = heap.resolve_slot(n4.cursor()).ok_or("expected slot 0")?;
        SpscVarQueue::consumer_pop(&raw mut (*b4.as_ptr()).queue);
        heap.refresh_cursor(n4.cursor());

        assert!(heap.is_empty());
        ThreadBuffer::free_on_heap(first);
        ThreadBuffer::free_on_heap(second);
    }
    Ok(())
}

#[test]
fn level_gate_and_queue_full_sync() -> TestResult {
    // 验证 set_level / set_flush_on 与 LEVEL_GATE 及 QUEUE_FULL_COUNT 同步
    let _serial = engine_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread()?;

    set_level(WRN);
    set_flush_on(Level::ERR);
    let gate = LevelGateWord::new(LEVEL_GATE.load(Ordering::Acquire));
    assert_eq!(gate.get(LevelGateWord::MIN_LEVEL), WRN as u8);
    assert_eq!(gate.get(LevelGateWord::RING_LEVEL), Level::ERR as u8);
    assert!(!gate.test(LevelGateWord::DEEP_PARK));

    flush_on(OFF);
    let gate_off = LevelGateWord::new(LEVEL_GATE.load(Ordering::Acquire));
    assert_eq!(gate_off.get(LevelGateWord::RING_LEVEL), 6);

    reset_queue_full_count();
    assert_eq!(QUEUE_FULL_COUNT.load(Ordering::Relaxed), 0);
    QUEUE_FULL_COUNT.store(7, Ordering::Relaxed);
    assert_eq!(queue_full_count(), 7);
    reset_queue_full_count();
    assert_eq!(queue_full_count(), 0);
    set_level(INFO);
    Ok(())
}

#[test]
fn consumer_lease_excludes_concurrent_poll() -> TestResult {
    // 验证 LogConsumer 持有 ConsumerLease 期间互斥拒绝其它 acquire_consumer 与 poll_batch
    let _serial = engine_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread()?;
    poll(true)?;

    let consumer = acquire_consumer(WakePolicy::default())?;
    let gen1 = consumer.generation();
    assert!(gen1 > 0);

    assert!(matches!(acquire_consumer(WakePolicy::default()), Err(ZeroLogError::ConsumerBusy)));
    assert!(matches!(
        poll_batch(DrainBudget { max_records: 8, max_bytes: 4096 }, false),
        Err(ZeroLogError::ConsumerBusy)
    ));

    drop(consumer);

    let consumer2 = builder().level(INFO).header_pattern("").build_consumer()?;
    assert_eq!(consumer2.generation(), gen1 + 1);
    drop(consumer2);
    Ok(())
}

#[test]
fn consumer_drain_budget_and_slab_lifecycle() -> TestResult {
    // 验证 LogConsumer::drain 预算分片、MORE_WORK、freeze_output、complete_output 与 write_failed
    let _serial = engine_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread()?;
    poll(true)?;
    set_header_pattern("")?;
    set_level(INFO);
    set_flush_delay(60_000_000_000);
    set_flush_buffer_size(64 * 1024);
    flush_on(OFF);
    let sink_bytes = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedVecWriter { bytes: Arc::clone(&sink_bytes) });

    let base_ts = TimestampCounter::read_timestamp_counter();
    publish_tag(1, base_ts + 1)?;
    publish_tag(2, base_ts + 2)?;
    publish_tag(3, base_ts + 3)?;

    let mut consumer = acquire_consumer(WakePolicy::AdaptiveTimer {
        min_interval_ns: 500_000,
        max_idle_interval_ns: 8_000_000,
    })?;

    let r1 = consumer.drain(1_000_000, DrainBudget { max_records: 2, max_bytes: u32::MAX });
    assert_eq!(r1.flags.get(DrainFlags::DRAINED), 2);
    assert!(r1.flags.test(DrainFlags::MORE_WORK));
    assert!(r1.flags.test(DrainFlags::OUTPUT_READY));
    assert!(!r1.flags.test(DrainFlags::IDLE));
    assert_eq!(r1.next_deadline_ns, 1_000_000);

    let slab1 = consumer.freeze_output().ok_or("expected frozen slab after first drain")?;
    assert_eq!(slab1.active_slice(), b"msg=1\nmsg=2\n");
    let rem = consumer.complete_output(slab1, 6).ok_or("expected suffix after short write")?;
    assert_eq!(rem.active_slice(), b"msg=2\n");

    let r2 = consumer.drain(2_000_000, DrainBudget { max_records: 2, max_bytes: u32::MAX });
    assert_eq!(r2.flags.get(DrainFlags::DRAINED), 1);
    assert!(!r2.flags.test(DrainFlags::MORE_WORK));
    assert!(r2.flags.test(DrainFlags::OUTPUT_READY));

    consumer.write_failed(rem, ZeroLogError::Write(libc::EAGAIN));
    assert_eq!(consumer.last_error(), Some(ZeroLogError::Write(libc::EAGAIN)));
    assert!(consumer.has_unflushed());

    let slab2 = consumer.freeze_output().ok_or("expected combined slab after write_failed")?;
    assert_eq!(slab2.active_slice(), b"msg=2\nmsg=3\n");
    assert!(consumer.complete_output(slab2, 12).is_none());
    assert!(!consumer.has_unflushed());

    let r3 = consumer.drain(3_000_000, DrainBudget { max_records: 8, max_bytes: u32::MAX });
    assert_eq!(r3.flags.get(DrainFlags::DRAINED), 0);
    assert!(r3.flags.test(DrainFlags::IDLE));
    drop(consumer);
    Ok(())
}

#[test]
fn epoch_safe_buffer_reclaim_during_drain() -> TestResult {
    // 验证已退出线程的 ThreadBuffer 在 ReadyMessageHeap 仍持有其记录期间绝不提前释放
    let _serial = engine_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread()?;
    poll(true)?;
    set_header_pattern("")?;
    set_level(INFO);
    set_flush_buffer_size(64 * 1024);

    let base_ts = TimestampCounter::read_timestamp_counter();
    let producer_addr = thread::spawn(move || -> Result<usize, &'static str> {
        publish_tag(10, base_ts + 10)?;
        publish_tag(20, base_ts + 20)?;
        let buf = zerolog_core::thread_buffer::get_thread_buffer();
        if buf.is_null() {
            return Err("missing thread buffer");
        }
        Ok(buf as usize)
    })
    .join()
    .map_err(|_| "producer thread panicked")??;

    let find_producer_slot = || -> Option<usize> {
        let bound = THREAD_BUFFER_REGISTRY.count();
        (0..bound).find(|&i| THREAD_BUFFER_REGISTRY.get(i) as usize == producer_addr)
    };
    let slot_idx = find_producer_slot().ok_or("producer buffer not found in registry")?;

    let mut consumer = acquire_consumer(WakePolicy::default())?;

    let first_slice = consumer.drain(1_000, DrainBudget { max_records: 1, max_bytes: u32::MAX });
    assert_eq!(first_slice.flags.get(DrainFlags::DRAINED), 1);
    assert!(first_slice.flags.test(DrainFlags::MORE_WORK));
    assert_eq!(
        THREAD_BUFFER_REGISTRY.get(slot_idx) as usize,
        producer_addr,
        "retired buffer must stay alive while its second record is in the heap"
    );

    let second_slice = consumer.drain(2_000, DrainBudget { max_records: 1, max_bytes: u32::MAX });
    assert_eq!(second_slice.flags.get(DrainFlags::DRAINED), 1);
    assert!(!second_slice.flags.test(DrainFlags::MORE_WORK));
    assert!(
        THREAD_BUFFER_REGISTRY.get(slot_idx).is_null(),
        "retired buffer must be reclaimed once its final record leaves the heap"
    );

    let slab = consumer.freeze_output().ok_or("expected slab from retired thread")?;
    assert_eq!(slab.active_slice(), b"msg=10\nmsg=20\n");
    assert!(consumer.complete_output(slab, 14).is_none());
    drop(consumer);
    Ok(())
}

#[test]
#[cfg(all(target_os = "linux", not(miri)))]
fn doorbell_wakes_background_poller_fast() -> TestResult {
    // 验证后台轮询线程阻塞于长超时 futex 时可被 ring_doorbell 立即唤醒
    let _serial = engine_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread()?;
    poll(true)?;
    set_header_pattern("")?;
    set_level(INFO);
    set_flush_delay(0);
    set_flush_buffer_size(1);

    let bytes = Arc::new(Mutex::new(Vec::new()));
    let write_count = Arc::new(AtomicUsize::new(0));
    struct NotifyWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
        writes: Arc<AtomicUsize>,
    }
    impl Write for NotifyWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Ok(mut guard) = self.bytes.lock() {
                guard.extend_from_slice(buf);
            }
            self.writes.fetch_add(1, Ordering::Release);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    set_output_writer(NotifyWriter { bytes: Arc::clone(&bytes), writes: Arc::clone(&write_count) });

    start_polling_thread(5_000_000_000)?;
    thread::sleep(Duration::from_millis(20));

    let start = Instant::now();
    publish_tag(777, TimestampCounter::read_timestamp_counter())?;
    ring_doorbell(true);

    while write_count.load(Ordering::Acquire) == 0 {
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "background poller did not wake on ring_doorbell within 500ms"
        );
        thread::sleep(Duration::from_millis(2));
    }
    stop_polling_thread()?;
    let content = bytes.lock().map_err(|_| "mutex poisoned")?.clone();
    assert_eq!(content, b"msg=777\n");
    Ok(())
}

#[test]
fn poll_and_config_nonblocking_with_consumer() -> TestResult {
    // 验证持有 LogConsumer 时 poll()、set_log_file() 与 start_polling_thread() 绝不挂起且 poll 返回 WouldBlock
    let _serial = engine_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread()?;
    poll(true)?;

    let consumer = acquire_consumer(WakePolicy::default())?;
    let poll_false_err =
        poll(false).err().ok_or("poll(false) must return WouldBlock while lease held")?;
    assert_eq!(poll_false_err.kind(), io::ErrorKind::WouldBlock);
    let poll_true_err =
        poll(true).err().ok_or("poll(true) must return WouldBlock while lease held")?;
    assert_eq!(poll_true_err.kind(), io::ErrorKind::WouldBlock);

    let tmp_path = std::env::temp_dir().join(format!("zerolog_t3_cfg_{}.log", std::process::id()));
    let tmp_str = tmp_path.to_str().ok_or("non-utf8 temp path")?;
    set_log_file(tmp_str)?;
    set_header_pattern("")?;
    set_output_writer(io::sink());
    let _ = std::fs::remove_file(&tmp_path);

    let start_err = start_polling_thread(1_000_000)
        .err()
        .ok_or("start_polling_thread must fail while LogConsumer holds lease")?;
    assert_eq!(start_err.kind(), io::ErrorKind::WouldBlock);

    drop(consumer);
    Ok(())
}

#[test]
fn deep_park_rescan_catches_late_publication() -> TestResult {
    // 验证 DeepPark 进入时开启 DEEP_PARK + membarrier 后重扫队列捕获临界发布消息
    let _serial = engine_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread()?;
    poll(true)?;
    set_header_pattern("")?;
    set_level(INFO);
    flush_on(OFF);
    set_output_writer(io::sink());

    struct ResetHook;
    impl Drop for ResetHook {
        fn drop(&mut self) {
            set_deep_park_transition_hook(None);
        }
    }
    let _reset = ResetHook;

    let mut consumer = acquire_consumer(WakePolicy::DeepPark { deep_park_after_ns: 1_000 })?;
    let budget = DrainBudget { max_records: 8, max_bytes: u32::MAX };

    let r1 = consumer.drain(1_000, budget);
    assert!(r1.flags.test(DrainFlags::IDLE));
    assert_eq!(r1.next_deadline_ns, 2_000);
    assert!(!LevelGateWord::new(LEVEL_GATE.load(Ordering::Acquire)).test(LevelGateWord::DEEP_PARK));

    fn inject_late_record() {
        let _ = publish_tag(999, TimestampCounter::read_timestamp_counter());
    }
    set_deep_park_transition_hook(Some(inject_late_record));

    let r2 = consumer.drain(2_000, budget);
    set_deep_park_transition_hook(None);
    assert_eq!(r2.flags.get(DrainFlags::DRAINED), 0);
    assert!(r2.flags.test(DrainFlags::MORE_WORK));
    assert!(!r2.flags.test(DrainFlags::IDLE));
    assert_eq!(r2.next_deadline_ns, 2_000);
    assert!(!LevelGateWord::new(LEVEL_GATE.load(Ordering::Acquire)).test(LevelGateWord::DEEP_PARK));

    let r3 = consumer.drain(2_001, budget);
    assert_eq!(r3.flags.get(DrainFlags::DRAINED), 1);
    let slab = consumer.freeze_output().ok_or("expected late record slab")?;
    assert_eq!(slab.active_slice(), b"msg=999\n");
    assert!(consumer.complete_output(slab, 8).is_none());

    let r4 = consumer.drain(3_000, budget);
    assert!(r4.flags.test(DrainFlags::IDLE));
    let r5 = consumer.drain(4_000, budget);
    assert!(r5.flags.test(DrainFlags::IDLE));
    assert!(LevelGateWord::new(LEVEL_GATE.load(Ordering::Acquire)).test(LevelGateWord::DEEP_PARK));
    assert!(r5.next_deadline_ns >= 100_004_000);

    drop(consumer);
    assert!(!LevelGateWord::new(LEVEL_GATE.load(Ordering::Acquire)).test(LevelGateWord::DEEP_PARK));
    Ok(())
}

#[test]
fn deep_park_membarrier_failure_reverts_gate() -> TestResult {
    // 验证 membarrier 失败或不支持时 DeepPark 退回有限超时并确保 DEEP_PARK=false 且 RING_LEVEL!=0
    let _serial = engine_test_lock().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    stop_polling_thread()?;
    poll(true)?;
    set_header_pattern("")?;
    set_level(INFO);
    set_flush_on(Level::ERR);
    set_output_writer(io::sink());

    struct ResetMembarrier;
    impl Drop for ResetMembarrier {
        fn drop(&mut self) {
            set_membarrier_state_for_test(0);
        }
    }
    let _reset = ResetMembarrier;

    let mut consumer = acquire_consumer(WakePolicy::DeepPark { deep_park_after_ns: 1_000 })?;
    let budget = DrainBudget { max_records: 8, max_bytes: u32::MAX };

    let _ = consumer.drain(1_000, budget);

    set_membarrier_state_for_test(3);
    let r_transient = consumer.drain(2_000, budget);
    assert_eq!(r_transient.next_deadline_ns, 100_002_000);
    let gate1 = LevelGateWord::new(LEVEL_GATE.load(Ordering::Acquire));
    assert!(!gate1.test(LevelGateWord::DEEP_PARK));
    assert_eq!(gate1.get(LevelGateWord::RING_LEVEL), Level::ERR as u8);
    assert_ne!(gate1.get(LevelGateWord::RING_LEVEL), 0);

    set_membarrier_state_for_test(2);
    let r_permanent = consumer.drain(3_000, budget);
    assert_eq!(r_permanent.next_deadline_ns, 100_003_000);
    let gate2 = LevelGateWord::new(LEVEL_GATE.load(Ordering::Acquire));
    assert!(!gate2.test(LevelGateWord::DEEP_PARK));
    assert_eq!(gate2.get(LevelGateWord::RING_LEVEL), Level::ERR as u8);
    assert_ne!(gate2.get(LevelGateWord::RING_LEVEL), 0);

    flush_on(OFF);
    drop(consumer);
    Ok(())
}

#[test]
fn poller_state_lifecycle_and_panic_recovery() -> TestResult {
    // 验证 PollerState 启动、活跃期互斥及工作线程 panic 后可安全恢复重启
    let state = PollerState::default();
    let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_entered = Arc::clone(&entered);
    state.start(1_000_000, move |_| {
        worker_entered.store(true, Ordering::Release);
        panic!("simulated worker panic");
    })?;
    for _ in 0..10_000 {
        if entered.load(Ordering::Acquire) {
            break;
        }
        thread::yield_now();
    }
    assert!(state.stop().is_err());
    assert!(!state.is_running());

    let runs = Arc::new(AtomicUsize::new(0));
    let worker_runs = Arc::clone(&runs);
    state.start(1_000_000, move |_| {
        worker_runs.fetch_add(1, Ordering::Release);
        Ok(())
    })?;
    assert!(state.is_running());
    state.stop()?;
    assert!(!state.is_running());
    assert!(runs.load(Ordering::Acquire) >= 1);
    Ok(())
}

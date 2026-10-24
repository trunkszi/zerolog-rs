use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use zerolog::{
    QueueFullPolicy, queue_full_count, reset_queue_full_count, set_queue_full_callback,
    set_queue_full_policy, stop_polling_thread,
};
use zerolog_core::{
    spsc_queue::{MAX_MESSAGE_PAYLOAD_SIZE, SpscVarQueue},
    thread_buffer::{ThreadBuffer, get_thread_buffer},
};

static CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn on_queue_full(count: u64) {
    CALLBACK_COUNT.store(count, Ordering::Release);
}

fn reset_queue_full_state() {
    set_queue_full_policy(QueueFullPolicy::Drop);
    set_queue_full_callback(None);
    reset_queue_full_count();
    CALLBACK_COUNT.store(0, Ordering::Release);
}

#[test]
fn drop_policy_counts_and_calls_callback() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_queue_full_state();
    set_queue_full_callback(Some(on_queue_full));

    thread::spawn(|| {
        let big_size = MAX_MESSAGE_PAYLOAD_SIZE as u32;
        let header = zerolog::alloc_message(big_size).unwrap();
        // SAFETY: this producer owns the freshly reserved header.
        unsafe {
            (*header).push(big_size);
        }
        let second = zerolog::alloc_message(big_size).unwrap();
        // SAFETY: the producer owns this fresh reservation.
        unsafe { (*second).push(big_size) };
        let third = zerolog::alloc_message(big_size);
        assert!(third.is_none());
    })
    .join()
    .unwrap();

    assert_eq!(queue_full_count(), 1);
    assert_eq!(CALLBACK_COUNT.load(Ordering::Acquire), 1);
    reset_queue_full_state();
}

#[test]
fn block_policy_retries_until_consumer_frees_space() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_queue_full_state();
    set_queue_full_policy(QueueFullPolicy::Block);

    let (sender, receiver) = mpsc::channel::<usize>();
    let producer = thread::spawn(move || {
        let first_size = MAX_MESSAGE_PAYLOAD_SIZE as u32;
        let second_size = first_size - 8;

        let header = zerolog::alloc_message(first_size).unwrap();
        // SAFETY: this producer owns the freshly reserved header.
        unsafe {
            (*header).push(first_size);
        }

        let extra = zerolog::alloc_message(first_size).unwrap();
        // SAFETY: the producer owns this fresh reservation.
        unsafe { (*extra).push(first_size) };

        let thread_buffer = get_thread_buffer();
        sender.send(thread_buffer as usize).unwrap();

        let start = Instant::now();
        let second = zerolog::alloc_message(second_size).unwrap();
        let elapsed = start.elapsed();
        // SAFETY: this producer owns the freshly reserved header.
        unsafe {
            (*second).push(second_size);
        }
        elapsed
    });

    let thread_buffer = receiver.recv().unwrap() as *mut ThreadBuffer;
    thread::sleep(Duration::from_millis(50));

    // SAFETY: the poller is stopped; this is the only consumer, and the
    // producer remains alive until its blocked reservation succeeds.
    unsafe {
        let queue_pointer = &raw mut (*thread_buffer).queue;
        assert!(SpscVarQueue::consumer_front(queue_pointer).is_some());
        SpscVarQueue::consumer_pop(queue_pointer);
    }

    let elapsed = producer.join().unwrap();
    assert!(elapsed >= Duration::from_millis(10), "elapsed: {elapsed:?}");
    assert!(queue_full_count() > 0);

    reset_queue_full_state();
}

#[test]
fn oversized_record_is_dropped_once_even_with_block_policy() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_queue_full_state();
    set_queue_full_callback(Some(on_queue_full));
    set_queue_full_policy(QueueFullPolicy::Block);
    for (index, size) in [MAX_MESSAGE_PAYLOAD_SIZE as u32 + 1, u32::MAX].into_iter().enumerate() {
        assert!(zerolog::alloc_message(size).is_none());
        assert_eq!(queue_full_count(), index as u64 + 1);
        assert_eq!(CALLBACK_COUNT.load(Ordering::Acquire), index as u64 + 1);
    }
    reset_queue_full_state();
}

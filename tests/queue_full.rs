use std::{
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use ferrilog::{
    QueueFullPolicy, queue_full_count, reset_queue_full_count, set_queue_full_callback,
    set_queue_full_policy, stop_polling_thread,
};
use ferrilog_core::{
    spsc_queue::{BUFFER_SIZE, SpscVarQueue},
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
        let big_size = BUFFER_SIZE as u32 / 2;
        let header = ferrilog::alloc_message(big_size).unwrap();
        unsafe {
            (*header).push(big_size);
        }
        let second = ferrilog::alloc_message(big_size);
        assert!(second.is_none());
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
        let first_size = BUFFER_SIZE as u32 / 2;
        let second_size = first_size - 8;

        let header = ferrilog::alloc_message(first_size).unwrap();
        unsafe {
            (*header).push(first_size);
        }

        let thread_buffer = get_thread_buffer();
        sender.send(thread_buffer as usize).unwrap();

        let start = Instant::now();
        let second = ferrilog::alloc_message(second_size).unwrap();
        let elapsed = start.elapsed();
        unsafe {
            (*second).push(second_size);
        }
        elapsed
    });

    let thread_buffer = receiver.recv().unwrap() as *mut ThreadBuffer;
    thread::sleep(Duration::from_millis(50));

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

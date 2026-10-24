use std::{
    fs,
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use zerolog::{
    INFO, OFF, flush_on, poll, set_flush_buffer_size, set_flush_delay, set_header_pattern,
    set_level, set_log_file, set_output_writer, set_thread_name, start_polling_thread,
    stop_polling_thread,
};

#[derive(Clone, Copy, zerolog::Encode)]
struct Point {
    x: i32,
    y: i32,
}

impl std::fmt::Display for Point {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "({}, {})", self.x, self.y)
    }
}

const DEFAULT_HEADER_PATTERN: &str = "[{date} {time}.{ns}] {level} [{thread}] {location} - ";
struct SharedWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl Write for SharedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn test_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn temp_log_path(name: &str) -> std::path::PathBuf {
    let unique = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("zerolog-{name}-{}-{unique}.log", std::process::id()))
}

fn wait_until_contains(path: &Path, needle: &str, timeout: Duration) -> String {
    let start = std::time::Instant::now();
    loop {
        let content = fs::read_to_string(path).unwrap_or_default();
        if content.contains(needle) {
            return content;
        }

        assert!(
            start.elapsed() < timeout,
            "did not observe `{needle}` within {timeout:?}, current content: {content}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn reset_flush_policy() {
    set_flush_delay(3_000_000_000);
    set_flush_buffer_size(8 * 1024);
    flush_on(OFF);
}

/// Waits until the file at `path` contains at least `expected` non-empty lines,
/// or panics after `timeout`.
fn wait_for_line_count(path: &Path, expected: usize, timeout: Duration) -> String {
    let start = std::time::Instant::now();
    loop {
        let content = fs::read_to_string(path).unwrap_or_default();
        let count = content.lines().filter(|line| !line.is_empty()).count();
        if count >= expected {
            return content;
        }
        assert!(
            start.elapsed() < timeout,
            "expected at least {expected} lines within {timeout:?}, \
             got {count} lines. last content:\n{content}"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn log_macro_writes_message_body_and_header() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));

    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);

    zerolog::log!(INFO, "answer={} text={}", 42u32, "ok");
    poll(true).unwrap();

    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("INF"));
    assert!(output.contains("answer=42 text=ok"));
    assert!(output.contains("tests/log_pipeline.rs"));
}

#[test]
fn log_macro_writes_plain_static_body() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern("").unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));

    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);

    zerolog::log!(INFO, "plain message");
    poll(true).unwrap();

    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert_eq!(output, "plain message\n");
}

#[test]
fn log_and_derive_encode_work() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));

    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);

    let point = Point { x: 7, y: 9 };
    let text = String::from("fast");
    zerolog::log!(INFO, "point={}", point);
    zerolog::info!("value={} text={}", 64u32, text);
    poll(true).unwrap();

    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("point=(7, 9)"));
    assert!(output.contains("value=64 text=fast"));
}

#[test]
fn log_fast_and_derive_encode_work() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));

    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);

    zerolog::log_fast!(INFO, "point={}", Point { x: 4, y: 6 });
    poll(true).unwrap();

    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("point=(4, 6)"));
}

#[test]
fn static_header_macro_writes_expected_output() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    let buffer = Arc::new(Mutex::new(Vec::new()));

    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);
    zerolog::set_header_pattern_static!("{thread}|{level}|{location}|{date} {time}.{ns}| ")
        .unwrap();

    zerolog::log!(INFO, "answer={}", 42u32);
    poll(true).unwrap();

    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("|INF|"));
    assert!(output.contains("tests/log_pipeline.rs"));
    assert!(output.contains("answer=42"));
    assert!(output.contains("."));

    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
}

#[test]
fn polling_thread_can_flush_to_file() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    let path = temp_log_path("poller");

    set_log_file(path.to_str().unwrap()).unwrap();
    set_level(INFO);
    set_flush_delay(0);
    start_polling_thread(1_000_000).unwrap();

    zerolog::log!(INFO, "background={}", 123u32);
    let output = wait_until_contains(&path, "background=123", Duration::from_secs(2));

    stop_polling_thread().unwrap();
    assert!(output.contains("INF"));
    assert!(output.contains("background=123"));

    fs::remove_file(path).ok();
}

#[test]
fn format_spec_and_header_pattern_work() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    let buffer = Arc::new(Mutex::new(Vec::new()));

    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);
    set_header_pattern("{level}|{thread}|{location}| ").unwrap();

    zerolog::log!(
        INFO,
        "hex={:x} alt={:#b} float={:.2} pad={:>5} str={:^7}",
        255u32,
        5u32,
        std::f64::consts::PI,
        7u32,
        "hi"
    );
    poll(true).unwrap();

    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("INF|"));
    assert!(output.contains("|tests/log_pipeline.rs:"));
    assert!(output.contains("hex=ff alt=0b101 float=3.14 pad=    7 str=  hi   "));

    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
}

#[test]
fn set_thread_name_updates_header_thread_token() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    let buffer = Arc::new(Mutex::new(Vec::new()));

    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);
    set_header_pattern("{thread}| ").unwrap();

    set_thread_name("api-worker");
    zerolog::log!(INFO, "named={}", 11u32);
    poll(true).unwrap();

    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("api-worker| named=11"));

    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
}

#[test]
fn prewarm_and_runtime_config_constraints_work() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));

    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);

    zerolog::prewarm!(INFO, "warm={}", 7u32);
    poll(true).unwrap();
    assert!(buffer.lock().unwrap().is_empty());

    let path = temp_log_path("config");
    set_log_file(path.to_str().unwrap()).unwrap();
    start_polling_thread(1_000_000).unwrap();

    assert!(set_header_pattern("{level}| ").is_err());
    assert!(set_log_file(path.to_str().unwrap()).is_err());

    stop_polling_thread().unwrap();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    fs::remove_file(path).ok();
}

#[test]
fn flush_policies_work() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    let buffer = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(INFO);

    set_flush_buffer_size(1);
    zerolog::log!(INFO, "buf={}", 1u32);
    poll(false).unwrap();
    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("buf=1"));

    buffer.lock().unwrap().clear();
    set_flush_buffer_size(8 * 1024);
    flush_on(INFO);
    zerolog::log!(INFO, "lvl={}", 2u32);
    poll(false).unwrap();
    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("lvl=2"));

    reset_flush_policy();
}

// ---------------------------------------------------------------------------
// File write tests
// ---------------------------------------------------------------------------

#[test]
fn file_write_many_messages() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();

    let path = temp_log_path("many");
    set_log_file(path.to_str().unwrap()).unwrap();
    set_level(INFO);
    set_flush_delay(0);
    start_polling_thread(1_000_000).unwrap();

    const MESSAGE_COUNT: usize = 1000;
    for i in 0..MESSAGE_COUNT as u32 {
        zerolog::log!(INFO, "seq={}", i);
    }

    let content = wait_for_line_count(&path, MESSAGE_COUNT, Duration::from_secs(5));
    stop_polling_thread().unwrap();

    let line_count = content.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(line_count, MESSAGE_COUNT, "expected {MESSAGE_COUNT} lines, got {line_count}");

    // Verify first and last messages are present
    assert!(content.contains("seq=0"), "missing first message");
    assert!(content.contains(&format!("seq={}", MESSAGE_COUNT - 1)), "missing last message");

    fs::remove_file(path).ok();
}

#[test]
fn file_write_multi_thread() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern("{level}|{thread}| ").unwrap();

    let path = temp_log_path("multi");
    set_log_file(path.to_str().unwrap()).unwrap();
    set_level(INFO);
    set_flush_delay(0);
    start_polling_thread(1_000_000).unwrap();

    const THREADS: usize = 4;
    const PER_THREAD: usize = 250;
    const TOTAL: usize = THREADS * PER_THREAD;

    let handles: Vec<_> = (0..THREADS)
        .map(|thread_index| {
            thread::spawn(move || {
                zerolog::set_thread_name(&format!("writer-{thread_index}"));
                for message_index in 0..PER_THREAD as u32 {
                    zerolog::info!("tid={} seq={}", thread_index as u32, message_index);
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().unwrap();
    }

    let content = wait_for_line_count(&path, TOTAL, Duration::from_secs(5));
    stop_polling_thread().unwrap();

    let line_count = content.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(
        line_count, TOTAL,
        "expected {TOTAL} lines from {THREADS} threads, got {line_count}"
    );

    // Verify each thread contributed the expected number of messages
    for thread_index in 0..THREADS {
        let thread_lines =
            content.lines().filter(|l| l.contains(&format!("tid={thread_index} "))).count();
        assert_eq!(
            thread_lines, PER_THREAD,
            "thread {thread_index}: expected {PER_THREAD} lines, got {thread_lines}"
        );
    }

    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    fs::remove_file(path).ok();
}

#[test]
fn file_flush_on_level() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    reset_flush_policy();
    set_header_pattern("{level}| ").unwrap();

    let path = temp_log_path("flush-level");
    set_log_file(path.to_str().unwrap()).unwrap();
    set_level(INFO);

    // Large flush delay and buffer: nothing flushes automatically
    set_flush_delay(60_000_000_000);
    set_flush_buffer_size(1024 * 1024);
    // Only ERR or above triggers an immediate flush
    flush_on(zerolog::ERR);

    // Write several INFO messages and poll — they should be buffered, NOT flushed
    for i in 0..5u32 {
        zerolog::info!("buffered={}", i);
    }
    poll(false).unwrap();
    let content_before = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        content_before.is_empty(),
        "INFO messages should not be flushed yet, but file contains: {content_before}"
    );

    // Now write an ERR message and poll — this should trigger an immediate flush
    // of ALL pending output (the 5 INFO messages + the ERR message)
    zerolog::error!("trigger-flush");
    poll(false).unwrap();

    let content_after = wait_until_contains(&path, "trigger-flush", Duration::from_secs(2));
    let line_count = content_after.lines().filter(|l| !l.is_empty()).count();
    assert_eq!(line_count, 6, "expected 6 lines (5 INFO + 1 ERR), got {line_count}");
    assert!(content_after.contains("ERR| trigger-flush"));
    assert!(content_after.contains("INF| buffered=0"));
    assert!(content_after.contains("INF| buffered=4"));

    reset_flush_policy();
    set_header_pattern(DEFAULT_HEADER_PATTERN).unwrap();
    fs::remove_file(path).ok();
}

struct RecoveringWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    fail_write: bool,
}

impl Write for RecoveringWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if std::mem::take(&mut self.fail_write) {
            return Err(io::Error::other("injected first-write failure"));
        }
        self.bytes.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn retry_after_first_write_error_emits_each_record_once() {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    set_header_pattern("").unwrap();
    reset_flush_policy();
    set_flush_buffer_size(1);
    set_level(INFO);
    let bytes = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(RecoveringWriter { bytes: Arc::clone(&bytes), fail_write: true });
    zerolog::info!("once-only");
    assert!(poll(true).is_err());
    poll(true).unwrap();
    assert_eq!(&*bytes.lock().unwrap(), b"once-only\n");
    reset_flush_policy();
}

struct PartialWriter {
    bytes: Arc<Mutex<Vec<u8>>>,
    step: u8,
    fail_flush: bool,
}
impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.step += 1;
        match self.step {
            1 => {
                self.bytes.lock().unwrap().extend_from_slice(&bytes[..2]);
                Ok(2)
            }
            2 => Err(io::ErrorKind::Interrupted.into()),
            3 => Err(io::Error::other("partial write failed")),
            _ => {
                self.bytes.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        if std::mem::take(&mut self.fail_flush) {
            Err(io::Error::other("flush failed"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn partial_write_and_flush_errors_never_replay_accepted_bytes() {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let mut output = zerolog::output::OutputBuffer::new();
    output.set_header_pattern("").unwrap();
    output.set_writer(Box::new(PartialWriter {
        bytes: Arc::clone(&bytes),
        step: 0,
        fail_flush: true,
    }));
    output.write_record(0, INFO, b"", "", 0, |b| b.extend_from_slice(b"abcdef")).unwrap();
    assert!(output.flush().is_err());
    assert_eq!(&*bytes.lock().unwrap(), b"ab");
    assert!(output.flush().is_err());
    assert_eq!(&*bytes.lock().unwrap(), b"abcdef\n");
    assert!(output.has_pending(), "failed writer flush must remain retryable");
    output.flush().unwrap();
    output.flush().unwrap();
    assert!(!output.has_pending());
    assert_eq!(&*bytes.lock().unwrap(), b"abcdef\n");
}

#[test]
fn persistent_writer_failure_applies_bounded_backpressure() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }
    let mut output = zerolog::output::OutputBuffer::new();
    output.set_header_pattern("").unwrap();
    output.set_writer(Box::new(Broken));
    let chunk = vec![b'x'; 1024 * 1024 - 1];
    for _ in 0..16 {
        output.write_record(0, INFO, b"", "", 0, |b| b.extend_from_slice(&chunk)).unwrap();
        assert!(output.flush().is_err());
    }
    for _ in 0..10 {
        let error = output.write_record(0, INFO, b"", "", 0, |b| b.push(b'y')).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert_eq!(output.buffered_len(), 16 * 1024 * 1024);
    }
    let bytes = Arc::new(Mutex::new(Vec::new()));
    output.set_writer(Box::new(SharedWriter { buffer: Arc::clone(&bytes) }));
    output.flush().unwrap();
    assert_eq!(bytes.lock().unwrap().len(), 16 * 1024 * 1024);
}

#[test]
fn failed_rotation_preserves_the_open_writer() {
    let directory = temp_log_path("rotation-dir");
    let moved = directory.with_extension("moved");
    fs::create_dir(&directory).unwrap();
    let path = directory.join("out.log");
    let mut output = zerolog::output::OutputBuffer::new();
    output.set_header_pattern("").unwrap();
    output
        .set_log_file_with_rotation(path.to_str().unwrap(), zerolog::RotationConfig::by_size(1, 2))
        .unwrap();
    fs::rename(&directory, &moved).unwrap();
    output.write_record(0, INFO, b"", "", 0, |b| b.extend_from_slice(b"first")).unwrap();
    assert!(output.flush().is_err());
    output.write_record(0, INFO, b"", "", 0, |b| b.extend_from_slice(b"second")).unwrap();
    assert!(output.flush().is_err());
    assert_eq!(fs::read(moved.join("out.log")).unwrap(), b"first\nsecond\n");
    fs::rename(&moved, &directory).unwrap();
    output.flush().unwrap();
    output.write_record(0, INFO, b"", "", 0, |b| b.extend_from_slice(b"third")).unwrap();
    output.flush().unwrap();
    drop(output);
    assert_eq!(fs::read(directory.join("out.log.2")).unwrap(), b"first\nsecond\n");
    assert_eq!(fs::read(directory.join("out.log.1")).unwrap(), b"third\n");
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn stale_guard_does_not_stop_or_flush_a_newer_generation() {
    let _serial = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedWriter { buffer: Arc::clone(&bytes) });
    let old = zerolog::builder().header_pattern("").start().unwrap();
    let current = zerolog::builder().header_pattern("").start().unwrap();
    drop(old);
    assert!(set_header_pattern("").is_err(), "new generation must still run");
    zerolog::info!("new-generation");
    current.shutdown().unwrap();
    assert_eq!(&*bytes.lock().unwrap(), b"new-generation\n");
    set_header_pattern("").unwrap();
}

#[test]
fn failed_builder_validation_preserves_filter_and_running_generation() {
    let _serial = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    set_output_writer(io::sink());
    let active = zerolog::builder().level(zerolog::ERROR).header_pattern("").start().unwrap();
    let absent_parent = temp_log_path("absent-parent").join("out.log");
    assert!(
        zerolog::builder().level(INFO).log_file(absent_parent.to_str().unwrap()).start().is_err()
    );
    assert!(!zerolog::check_level(INFO));
    assert!(set_header_pattern("").is_err());
    assert!(zerolog::builder().level(INFO).header_pattern("{unsupported}").start().is_err());
    assert!(!zerolog::check_level(INFO));
    active.shutdown().unwrap();
    assert!(zerolog::builder().level(INFO).header_pattern("").start().unwrap().shutdown().is_ok());
}

#[test]
fn concurrent_manual_and_background_polls_emit_each_record_once() {
    let _serial = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedWriter { buffer: Arc::clone(&bytes) });
    set_header_pattern("").unwrap();
    set_level(INFO);
    set_flush_buffer_size(1);
    start_polling_thread(10_000).unwrap();
    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..100 {
                    poll(false).unwrap();
                }
            });
        }
        for worker in 0..4u32 {
            scope.spawn(move || {
                for record in 0..100u32 {
                    zerolog::info!("{}:{}", worker, record);
                }
            });
        }
    });
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    let text = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    let lines: std::collections::BTreeSet<_> = text.lines().collect();
    assert_eq!(text.lines().count(), 400);
    assert_eq!(lines.len(), 400);
    reset_flush_policy();
}

#[test]
fn concurrent_lifecycle_calls_join_and_leave_configuration_available() {
    let _serial = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    set_output_writer(io::sink());
    thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..30 {
                    start_polling_thread(10_000).unwrap();
                    stop_polling_thread().unwrap();
                    poll(true).unwrap();
                }
            });
        }
    });
    stop_polling_thread().unwrap();
    set_header_pattern("").unwrap();
}

#[test]
fn a_second_stop_waits_for_the_same_worker_join() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    };
    let state = Arc::new(zerolog::poller::PollerState::default());
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let (entered_tx, entered_rx) = mpsc::channel();
    let worker_release = Arc::clone(&release);
    state
        .start(10_000, move |force| {
            if !force {
                let _ = entered_tx.send(());
                let (lock, changed) = &*worker_release;
                let mut ready = lock.lock().unwrap();
                while !*ready {
                    ready = changed.wait(ready).unwrap();
                }
            }
            Ok(())
        })
        .unwrap();
    entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    let (completed_tx, completed_rx) = mpsc::channel();
    let first_started = Arc::new(AtomicBool::new(false));
    thread::scope(|scope| {
        for _ in 0..2 {
            let state = Arc::clone(&state);
            let tx = completed_tx.clone();
            let started = Arc::clone(&first_started);
            scope.spawn(move || {
                started.store(true, Ordering::Release);
                tx.send(state.stop()).unwrap();
            });
        }
        while !first_started.load(Ordering::Acquire) {
            thread::yield_now();
        }
        let prematurely_done = completed_rx.recv_timeout(Duration::from_millis(50)).is_ok();
        let (lock, changed) = &*release;
        *lock.lock().unwrap() = true;
        changed.notify_all();
        assert!(!prematurely_done, "stop returned before the worker was released/joined");
        for _ in 0..2 {
            completed_rx.recv_timeout(Duration::from_secs(2)).unwrap().unwrap();
        }
    });
}

#[test]
fn panicking_writer_releases_consumer_and_discards_scratch_pointers() {
    struct Panics;
    impl Write for Panics {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("writer panic")
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let _serial = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    set_header_pattern("").unwrap();
    set_level(INFO);
    set_flush_buffer_size(1);
    set_output_writer(Panics);
    thread::spawn(|| {
        zerolog::info!("one");
        zerolog::info!("two");
    })
    .join()
    .unwrap();
    assert!(std::panic::catch_unwind(|| poll(true)).is_err());
    let bytes = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedWriter { buffer: Arc::clone(&bytes) });
    poll(true).unwrap();
    zerolog::info!("three");
    poll(true).unwrap();
    assert_eq!(&*bytes.lock().unwrap(), b"one\ntwo\nthree\n");
    reset_flush_policy();
}

#[test]
fn a_manually_stopped_guard_does_not_flush_a_reconfigured_destination() {
    let _serial = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    set_output_writer(io::sink());
    let guard = zerolog::builder().header_pattern("").start().unwrap();
    stop_polling_thread().unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedWriter { buffer: Arc::clone(&bytes) });
    set_flush_buffer_size(u32::MAX);
    set_flush_delay(i64::MAX);
    zerolog::info!("manual-owner");
    poll(false).unwrap();
    drop(guard);
    assert!(bytes.lock().unwrap().is_empty());
    poll(true).unwrap();
    assert_eq!(&*bytes.lock().unwrap(), b"manual-owner\n");
    reset_flush_policy();
}

#[test]
fn single_oversized_formatted_record_clamps_and_does_not_livelock() {
    #[derive(Clone, Copy, zerolog::Encode)]
    struct Wide;
    impl std::fmt::Display for Wide {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            let chunk = "x".repeat(1024 * 1024);
            for _ in 0..17 {
                f.write_str(&chunk)?;
            }
            Ok(())
        }
    }

    let _serial = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedWriter { buffer: Arc::clone(&bytes) });
    set_header_pattern("").unwrap();
    set_level(INFO);
    reset_flush_policy();

    zerolog::info!("big={}", Wide);
    zerolog::info!("after={}", 7u32);
    poll(true).unwrap();
    poll(true).unwrap();

    let got = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(got.starts_with("big=xxxx"));
    assert!(got.ends_with("\nafter=7\n"));
}

#[test]
fn failed_restart_flush_preserves_running_background_poller() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct ToggleFlushWriter {
        bytes: Arc<Mutex<Vec<u8>>>,
        fail_flush: Arc<AtomicUsize>,
    }
    impl Write for ToggleFlushWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.bytes.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            if self.fail_flush.load(Ordering::SeqCst) == 1 {
                Err(io::Error::other("flush failed"))
            } else {
                Ok(())
            }
        }
    }

    let _serial = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let fail_flush = Arc::new(AtomicUsize::new(0));
    set_output_writer(ToggleFlushWriter {
        bytes: Arc::clone(&bytes),
        fail_flush: Arc::clone(&fail_flush),
    });
    let guard = zerolog::builder()
        .header_pattern("")
        .poller_interval(Duration::from_millis(15))
        .start()
        .unwrap();
    zerolog::info!("before-restart");
    thread::sleep(Duration::from_millis(80));

    fail_flush.store(1, Ordering::SeqCst);
    assert!(zerolog::builder().header_pattern("").start().is_err());
    fail_flush.store(0, Ordering::SeqCst);

    zerolog::info!("after-restart");
    thread::sleep(Duration::from_millis(120));
    let got = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(got.contains("before-restart"), "{got}");
    assert!(got.contains("after-restart"), "{got}");
    guard.shutdown().unwrap();
    reset_flush_policy();
}

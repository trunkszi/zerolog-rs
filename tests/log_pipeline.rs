use std::{
    fs,
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use ferrilog::{
    INFO, OFF, flush_on, poll, set_flush_buffer_size, set_flush_delay, set_header_pattern,
    set_level, set_log_file, set_output_writer, set_thread_name, start_polling_thread,
    stop_polling_thread,
};

#[derive(Clone, Copy, ferrilog::Encode)]
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
    std::env::temp_dir().join(format!("ferrilog-{name}-{}-{unique}.log", std::process::id()))
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

    ferrilog::log!(INFO, "answer={} text={}", 42u32, "ok");
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

    ferrilog::log!(INFO, "plain message");
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
    ferrilog::log!(INFO, "point={}", point);
    ferrilog::info!("value={} text={}", 64u32, text);
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

    ferrilog::log_fast!(INFO, "point={}", Point { x: 4, y: 6 });
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
    ferrilog::set_header_pattern_static!("{thread}|{level}|{location}|{date} {time}.{ns}| ")
        .unwrap();

    ferrilog::log!(INFO, "answer={}", 42u32);
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

    ferrilog::log!(INFO, "background={}", 123u32);
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

    ferrilog::log!(
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
    ferrilog::log!(INFO, "named={}", 11u32);
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

    ferrilog::prewarm!(INFO, "warm={}", 7u32);
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
    ferrilog::log!(INFO, "buf={}", 1u32);
    poll(false).unwrap();
    let output = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
    assert!(output.contains("buf=1"));

    buffer.lock().unwrap().clear();
    set_flush_buffer_size(8 * 1024);
    flush_on(INFO);
    ferrilog::log!(INFO, "lvl={}", 2u32);
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
        ferrilog::log!(INFO, "seq={}", i);
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
                ferrilog::set_thread_name(&format!("writer-{thread_index}"));
                for message_index in 0..PER_THREAD as u32 {
                    ferrilog::info!("tid={} seq={}", thread_index as u32, message_index);
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
    flush_on(ferrilog::ERR);

    // Write several INFO messages and poll — they should be buffered, NOT flushed
    for i in 0..5u32 {
        ferrilog::info!("buffered={}", i);
    }
    poll(false).unwrap();
    let content_before = fs::read_to_string(&path).unwrap_or_default();
    assert!(
        content_before.is_empty(),
        "INFO messages should not be flushed yet, but file contains: {content_before}"
    );

    // Now write an ERR message and poll — this should trigger an immediate flush
    // of ALL pending output (the 5 INFO messages + the ERR message)
    ferrilog::error!("trigger-flush");
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

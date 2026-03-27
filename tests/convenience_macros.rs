use std::{
    io::{self, Write},
    sync::{Arc, Mutex, OnceLock},
};

use ferrilog::{
    Level, OFF, flush_on, poll, set_flush_buffer_size, set_flush_delay, set_level,
    set_output_writer, stop_polling_thread,
};

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

fn capture_output<F: FnOnce()>(level: Level, action: F) -> String {
    let _guard = test_lock().lock().unwrap();
    stop_polling_thread().unwrap();
    set_flush_delay(3_000_000_000);
    set_flush_buffer_size(8 * 1024);
    flush_on(OFF);
    let buffer = Arc::new(Mutex::new(Vec::new()));
    set_output_writer(SharedWriter { buffer: buffer.clone() });
    set_level(level);

    action();
    poll(true).unwrap();

    String::from_utf8(buffer.lock().unwrap().clone()).unwrap()
}

#[test]
fn test_logd() {
    let output = capture_output(Level::DBG, || {
        ferrilog::logd!("debug value={}", 42u32);
    });
    assert!(output.contains("DBG"), "output: {output}");
    assert!(output.contains("debug value=42"), "output: {output}");
}

#[test]
fn test_logi() {
    let output = capture_output(Level::DBG, || {
        ferrilog::logi!("info message={}", "hello");
    });
    assert!(output.contains("INF"), "output: {output}");
    assert!(output.contains("info message=hello"), "output: {output}");
}

#[test]
fn test_logw() {
    let output = capture_output(Level::DBG, || {
        ferrilog::logw!("warning count={}", 99u64);
    });
    assert!(output.contains("WRN"), "output: {output}");
    assert!(output.contains("warning count=99"), "output: {output}");
}

#[test]
fn test_loge() {
    let output = capture_output(Level::DBG, || {
        ferrilog::loge!("error code={}", -1i32);
    });
    assert!(output.contains("ERR"), "output: {output}");
    assert!(output.contains("error code=-1"), "output: {output}");
}

#[test]
fn test_logd_filtered_by_level() {
    let output = capture_output(Level::INF, || {
        ferrilog::logd!("should not appear={}", 0u32);
        ferrilog::logi!("should appear={}", 1u32);
    });
    assert!(!output.contains("should not appear"), "output: {output}");
    assert!(output.contains("should appear=1"), "output: {output}");
}

#[test]
fn test_no_args() {
    let output = capture_output(Level::DBG, || {
        ferrilog::logi!("plain message no args");
    });
    assert!(output.contains("plain message no args"), "output: {output}");
}

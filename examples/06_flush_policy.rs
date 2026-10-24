//! Demonstrates flush policies: delay, buffer size, and level trigger.
//!
//! Uses a SharedWriter (Vec<u8> behind Arc<Mutex>) so we can inspect
//! the output at precise moments without relying on stderr.
//!
//! Run with:
//!   cargo run --example 06_flush_policy

use std::{
    io::Write,
    sync::{Arc, Mutex},
    time::Duration,
};

/// A writer backed by a shared byte buffer.
struct SharedWriter(Arc<Mutex<Vec<u8>>>);

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn main() -> std::io::Result<()> {
    let sink = Arc::new(Mutex::new(Vec::<u8>::new()));

    // --- 1. flush_delay: buffer for 500ms before flushing ---
    println!("--- flush_delay demo ---");
    {
        zerolog::stop_polling_thread().ok();
        zerolog::set_output_writer(SharedWriter(Arc::clone(&sink)));
        let _guard = zerolog::builder()
            .level(zerolog::DBG)
            .flush_delay(Duration::from_millis(500))
            .flush_buffer_size(64 * 1024) // large buffer so size doesn't trigger flush
            .start()?;

        zerolog::set_thread_name("main");
        zerolog::info!("delayed message");

        // After a short sleep the message is still buffered.
        std::thread::sleep(Duration::from_millis(100));
        println!("  after 100ms: {} bytes flushed", sink.lock().unwrap().len());

        // After the delay elapses, the poller flushes automatically.
        std::thread::sleep(Duration::from_millis(600));
        println!("  after 700ms: {} bytes flushed", sink.lock().unwrap().len());
    }
    sink.lock().unwrap().clear();

    // --- 2. flush_buffer_size: flush when buffer reaches N bytes ---
    println!("--- flush_buffer_size demo ---");
    {
        zerolog::stop_polling_thread().ok();
        zerolog::set_output_writer(SharedWriter(Arc::clone(&sink)));
        let _guard = zerolog::builder()
            .level(zerolog::DBG)
            .flush_delay(Duration::from_secs(60)) // very long delay so it doesn't kick in
            .flush_buffer_size(128) // flush at 128 bytes
            .start()?;

        zerolog::set_thread_name("main");

        // Write several short messages until the buffer exceeds 128 bytes.
        for i in 0..10u32 {
            zerolog::info!("buffer-test message number {}", i);
        }
        std::thread::sleep(Duration::from_millis(100));
        let flushed = sink.lock().unwrap().len();
        println!("  flushed {} bytes (threshold = 128)", flushed);
    }
    sink.lock().unwrap().clear();

    // --- 3. flush_on(ERR): error messages trigger immediate flush ---
    println!("--- flush_on(ERR) demo ---");
    {
        zerolog::stop_polling_thread().ok();
        zerolog::set_output_writer(SharedWriter(Arc::clone(&sink)));
        let _guard = zerolog::builder()
            .level(zerolog::DBG)
            .flush_delay(Duration::from_secs(60)) // very long delay
            .flush_buffer_size(64 * 1024) // very large buffer
            .flush_on(zerolog::ERR) // flush immediately on ERR
            .start()?;

        zerolog::set_thread_name("main");
        zerolog::info!("info -- may be buffered");
        zerolog::error!("error -- triggers immediate flush");

        std::thread::sleep(Duration::from_millis(100));
        let flushed = sink.lock().unwrap().len();
        println!("  flushed {} bytes after ERR log", flushed);
    }

    println!("--- all flush policy demos done ---");
    Ok(())
}

//! Comprehensive example demonstrating all core zerolog features.
//!
//! Run with:
//!   cargo run --example basic

use std::{thread, time::Duration};

use zerolog::{
    Level, QueueFullPolicy, flush_on, poll, set_flush_buffer_size, set_flush_delay,
    set_header_pattern, set_level, set_log_file, set_output_writer, set_queue_full_callback,
    set_queue_full_policy, set_thread_name, start_polling_thread, stop_polling_thread,
};

// ---------------------------------------------------------------------------
// Custom type with derived Encode and Display implementation
// ---------------------------------------------------------------------------

/// A simple 2D point used to demonstrate custom-type logging.
#[derive(Clone, Copy, zerolog::Encode)]
struct Point {
    x: i32,
    y: i32,
}

impl std::fmt::Display for Point {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({}, {})", self.x, self.y)
    }
}

/// A trade order used to demonstrate a more realistic custom type.
#[derive(Clone, Copy, zerolog::Encode)]
struct Order {
    id: u64,
    price: f64,
    quantity: u32,
}

impl std::fmt::Display for Order {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Order{{id={}, price={:.2}, qty={}}}", self.id, self.price, self.quantity)
    }
}

// ---------------------------------------------------------------------------
// A dummy writer that discards all output (useful for sink/benchmark setups)
// ---------------------------------------------------------------------------

struct SinkWriter;

impl std::io::Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn main() {
    // -----------------------------------------------------------------------
    // 1. Basic setup: configure header pattern, output, and level
    // -----------------------------------------------------------------------

    // The polling thread must be stopped before changing output or header settings.
    // On a fresh start it is already stopped, but calling stop is safe and idempotent.
    stop_polling_thread().unwrap();

    // Set a custom header pattern.
    // Available tokens: {date}, {time}, {ns}, {level}, {thread}, {location}
    set_header_pattern("[{date} {time}.{ns}] {level} [{thread}] {location} - ").unwrap();

    // Set the minimum log level. Messages below this level are silently discarded.
    set_level(Level::DBG);

    // -----------------------------------------------------------------------
    // 2. Basic log calls at different levels
    // -----------------------------------------------------------------------

    // Name the current thread for the {thread} header token.
    set_thread_name("main");

    // Log at each severity level using the convenience macros.
    zerolog::debug!("application starting, debug mode enabled");
    zerolog::info!("server listening on port {}", 8080u32);
    zerolog::warn!("connection pool utilization at {}%", 85u32);
    zerolog::error!("failed to open config file: {}", "config.toml");

    // Manually poll and flush so we can see the output immediately.
    poll(true).unwrap();

    println!("--- basic log calls done ---\n");

    // -----------------------------------------------------------------------
    // 3. Format specifiers
    // -----------------------------------------------------------------------

    let value: u32 = 255;
    let pi: f64 = std::f64::consts::PI;

    // Hexadecimal (lower and upper)
    zerolog::info!("hex lower: {:x}", value);
    zerolog::info!("hex upper: {:X}", value);
    zerolog::info!("hex with prefix: {:#x}", value);
    zerolog::info!("hex zero-padded: {:#010x}", value);

    // Floating point precision
    zerolog::info!("pi 2 decimals: {:.2}", pi);
    zerolog::info!("pi 6 decimals: {:.6}", pi);

    // Width and alignment
    zerolog::info!("right-aligned: {:>10}", 42u32);
    zerolog::info!("left-aligned:  {:<10}", 42u32);
    zerolog::info!("center-aligned:{:^10}", 42u32);

    // Fill character with alignment
    zerolog::info!("zero-filled: {:0>8}", 42u32);
    zerolog::info!("dash-filled: {:->20}", "hello");

    // Scientific notation
    zerolog::info!("scientific: {:>14.3e}", pi);

    // Binary and octal
    zerolog::info!("binary: {:b}", 42u32);
    zerolog::info!("binary with prefix: {:#b}", 42u32);
    zerolog::info!("octal: {:o}", 42u32);

    poll(true).unwrap();
    println!("--- format specifiers done ---\n");

    // -----------------------------------------------------------------------
    // 4. Custom types with #[derive(Encode)]
    // -----------------------------------------------------------------------

    let point = Point { x: 10, y: -3 };
    let order = Order { id: 100001, price: 49.99, quantity: 50 };

    zerolog::info!("point={}", point);
    zerolog::info!("order={}", order);

    // Custom types also support format specifiers (applied via Display)
    zerolog::info!("padded point: {:^20}", point);
    zerolog::info!("padded order: {:>40}", order);

    // log_fast! variant: only supports bare {} placeholders, but is faster
    zerolog::log_fast!(Level::INF, "fast point={}", point);

    poll(true).unwrap();
    println!("--- custom types done ---\n");

    // -----------------------------------------------------------------------
    // 5. Level filtering with set_level()
    // -----------------------------------------------------------------------

    // Only show warnings and above
    set_level(Level::WRN);
    zerolog::debug!("this debug message is filtered out");
    zerolog::info!("this info message is filtered out");
    zerolog::warn!("this warning IS visible");
    zerolog::error!("this error IS visible");

    poll(true).unwrap();

    // Restore to show all levels
    set_level(Level::DBG);
    println!("--- level filtering done ---\n");

    // -----------------------------------------------------------------------
    // 6. File output with set_log_file()
    // -----------------------------------------------------------------------

    let log_path = std::env::temp_dir().join("zerolog-example.log");
    let log_path_str = log_path.to_str().unwrap();

    set_log_file(log_path_str).unwrap();
    zerolog::info!("this message goes to the log file");
    poll(true).unwrap();

    println!("Log file written to: {}", log_path.display());

    // Switch back to a sink writer for the rest of the example
    stop_polling_thread().unwrap();
    set_output_writer(SinkWriter);
    println!("--- file output done ---\n");

    // -----------------------------------------------------------------------
    // 7. Custom header pattern
    // -----------------------------------------------------------------------

    // Minimal header: just level and a separator
    set_header_pattern("{level} | ").unwrap();
    zerolog::info!("minimal header style");
    poll(true).unwrap();

    // Compact header without date/time
    set_header_pattern("{level} [{thread}] {location} - ").unwrap();
    zerolog::info!("compact header without timestamp");
    poll(true).unwrap();

    // Empty header: body only
    set_header_pattern("").unwrap();
    zerolog::info!("body only, no header at all");
    poll(true).unwrap();

    // Restore default-style header
    set_header_pattern("[{date} {time}.{ns}] {level} [{thread}] {location} - ").unwrap();
    println!("--- header patterns done ---\n");

    // -----------------------------------------------------------------------
    // 8. Polling thread: start and stop
    // -----------------------------------------------------------------------

    // Start the background polling thread with a 1ms poll interval (in nanoseconds).
    start_polling_thread(1_000_000).unwrap();

    zerolog::info!("this is written by the background poller");

    // Give the poller a moment to pick up the message
    thread::sleep(Duration::from_millis(50));

    // Stop the poller (drains remaining messages and joins the thread)
    stop_polling_thread().unwrap();
    println!("--- polling thread done ---\n");

    // -----------------------------------------------------------------------
    // 9. Flush policies: delay, buffer size, and level trigger
    // -----------------------------------------------------------------------

    // set_flush_delay: buffered output is flushed after this duration (ns)
    // with no new messages. Default is 3 seconds.
    set_flush_delay(500_000_000); // 500ms

    // set_flush_buffer_size: flush when internal buffer reaches this many bytes.
    set_flush_buffer_size(4 * 1024); // 4 KB

    // flush_on: immediately flush when a message at this level (or above) is logged.
    flush_on(Level::ERR); // auto-flush on every error

    zerolog::info!("info message, buffered until delay/size threshold");
    zerolog::error!("error message triggers immediate flush");
    poll(true).unwrap();

    // Reset to defaults
    set_flush_delay(3_000_000_000);
    set_flush_buffer_size(8 * 1024);
    flush_on(Level::OFF);
    println!("--- flush policies done ---\n");

    // -----------------------------------------------------------------------
    // 10. Multi-threaded logging with set_thread_name()
    // -----------------------------------------------------------------------

    set_header_pattern("{level} [{thread}] - ").unwrap();
    start_polling_thread(1_000_000).unwrap();

    let mut handles = Vec::new();
    for thread_index in 0..4u32 {
        let handle = thread::spawn(move || {
            // Give each thread a meaningful name visible in the log header
            set_thread_name(&format!("worker-{}", thread_index));

            for message_index in 0..5u32 {
                zerolog::info!(
                    "thread {} message {} value={}",
                    thread_index,
                    message_index,
                    thread_index * 100 + message_index
                );
            }
        });
        handles.push(handle);
    }

    // Wait for all writer threads to finish
    for handle in handles {
        handle.join().unwrap();
    }

    // Give the poller time to drain and then stop it
    thread::sleep(Duration::from_millis(100));
    stop_polling_thread().unwrap();
    set_header_pattern("[{date} {time}.{ns}] {level} [{thread}] {location} - ").unwrap();
    println!("--- multi-threaded logging done ---\n");

    // -----------------------------------------------------------------------
    // 11. Queue full policy configuration
    // -----------------------------------------------------------------------

    // Policy: Drop -- silently discard messages when the queue is full (default).
    set_queue_full_policy(QueueFullPolicy::Drop);

    // Policy: Block -- spin-wait until space becomes available.
    set_queue_full_policy(QueueFullPolicy::Block);

    // Optional callback invoked on each queue-full event.
    // The argument is the cumulative count of queue-full events.
    set_queue_full_callback(Some(|count: u64| {
        eprintln!("queue full event #{}", count);
    }));

    zerolog::info!("queue full policy is now Block with a callback");
    poll(true).unwrap();

    // Clear the callback and restore default policy
    set_queue_full_callback(None);
    set_queue_full_policy(QueueFullPolicy::Drop);
    println!("--- queue full policy done ---\n");

    // -----------------------------------------------------------------------
    // Cleanup
    // -----------------------------------------------------------------------

    // Clean up the temporary log file
    std::fs::remove_file(&log_path).ok();

    println!("All examples completed successfully.");
}

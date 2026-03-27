//! Complete showcase combining ALL ferrilog features.
//!
//! This example demonstrates:
//!   - Builder with all options configured
//!   - Custom types with #[derive(Encode)]
//!   - Multiple log levels with filtering
//!   - Format specifiers (hex, binary, octal, precision, alignment)
//!   - Multi-threaded logging with thread names
//!   - File output with rotation
//!   - Flush policies
//!   - Queue full policy (Drop)
//!   - Guard auto-cleanup
//!
//! Run with:
//!   cargo run --example 07_complete

use std::{thread, time::Duration};

// ---------------------------------------------------------------------------
// Custom types
// ---------------------------------------------------------------------------

/// A 3D vector used to demonstrate custom-type logging.
/// Must be Copy + 'static + Display to work with #[derive(Encode)].
#[derive(Clone, Copy, ferrilog::Encode)]
struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

impl std::fmt::Display for Vec3 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "({:.2}, {:.2}, {:.2})", self.x, self.y, self.z)
    }
}

/// A simplified trade event.
#[derive(Clone, Copy, ferrilog::Encode)]
struct Trade {
    symbol_id: u32,
    price: f64,
    qty: u32,
    buy: bool,
}

impl std::fmt::Display for Trade {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let side = if self.buy { "BUY" } else { "SELL" };
        write!(
            f,
            "Trade{{sym={}, price={:.4}, qty={}, side={}}}",
            self.symbol_id, self.price, self.qty, side
        )
    }
}

fn main() -> std::io::Result<()> {
    // -------------------------------------------------------------------
    // 1. Set up file output with rotation
    // -------------------------------------------------------------------
    let dir = std::env::current_dir()?.join("ferrilog-complete-demo");
    std::fs::create_dir_all(&dir)?;
    let log_path = dir.join("complete.log");
    let log_path_str = log_path.to_str().unwrap();

    // -------------------------------------------------------------------
    // 2. Build the logger with all options
    // -------------------------------------------------------------------
    let _guard = ferrilog::builder()
        .level(ferrilog::DBG)
        .log_file(log_path_str)
        .rotation(ferrilog::RotationConfig::by_size(2048, 3))
        .flush_delay(Duration::from_millis(500))
        .flush_buffer_size(512)
        .flush_on(ferrilog::ERR)
        .queue_full_policy(ferrilog::QueueFullPolicy::Drop)
        .poller_interval(Duration::from_micros(100))
        .header_pattern("[{time}.{ns}] {level} [{thread}] {location} - ")
        .start()?;

    ferrilog::set_thread_name("main");

    // -------------------------------------------------------------------
    // 3. Log levels and filtering
    // -------------------------------------------------------------------
    ferrilog::logd!("debug: application starting");
    ferrilog::logi!("info: configuration loaded");
    ferrilog::logw!("warn: disk usage at {}%", 87u32);
    ferrilog::loge!("error: failed to connect, retrying");

    // -------------------------------------------------------------------
    // 4. Format specifiers
    // -------------------------------------------------------------------
    let flags: u32 = 0xDEAD;
    let pi: f64 = std::f64::consts::PI;

    ferrilog::logi!("hex:        {:#x}", flags);
    ferrilog::logi!("upper hex:  {:#X}", flags);
    ferrilog::logi!("binary:     {:#b}", flags);
    ferrilog::logi!("octal:      {:#o}", flags);
    ferrilog::logi!("precision:  {:.4}", pi);
    ferrilog::logi!("right-pad:  [{:>12}]", 42u32);
    ferrilog::logi!("left-pad:   [{:<12}]", 42u32);
    ferrilog::logi!("center-pad: [{:^12}]", 42u32);
    ferrilog::logi!("zero-pad:   [{:0>8}]", flags);

    // -------------------------------------------------------------------
    // 5. Custom types
    // -------------------------------------------------------------------
    let velocity = Vec3 { x: 1.5, y: -0.3, z: 9.81 };
    let trade = Trade { symbol_id: 42, price: 123.4567, qty: 100, buy: true };

    ferrilog::logi!("velocity = {}", velocity);
    ferrilog::logi!("trade    = {}", trade);
    ferrilog::logi!("padded trade: [{:>50}]", trade);

    // -------------------------------------------------------------------
    // 6. Multi-threaded logging
    // -------------------------------------------------------------------
    let mut handles = Vec::new();
    for id in 0..4u32 {
        let handle = thread::spawn(move || {
            ferrilog::set_thread_name(&format!("worker-{}", id));
            for i in 0..10u32 {
                ferrilog::logi!("worker {} task {} status={}", id, i, id * 10 + i);
            }
        });
        handles.push(handle);
    }

    // The main thread continues logging concurrently.
    for i in 0..5u32 {
        ferrilog::logd!("main heartbeat {}", i);
    }

    for h in handles {
        h.join().unwrap();
    }

    ferrilog::logi!("all workers finished");

    // -------------------------------------------------------------------
    // 7. Guard auto-cleanup: the drop flushes and stops the poller
    // -------------------------------------------------------------------
    drop(_guard);

    // Allow a brief moment for file finalization.
    thread::sleep(Duration::from_millis(50));

    // Show the rotated log files.
    println!("--- log files ---");
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in &entries {
        let meta = entry.metadata()?;
        println!("  {} ({} bytes)", entry.file_name().to_string_lossy(), meta.len());
    }

    // Clean up.
    // std::fs::remove_dir_all(&dir)?;
    println!("--- done ---");

    Ok(())
}

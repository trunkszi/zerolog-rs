//! File output with automatic size-based rotation.
//!
//! Uses a small max_size (1 KB) so that rotation is triggered quickly
//! during the demo. After logging, the rotated files are listed.
//!
//! Run with:
//!   cargo run --example 05_file_rotation

use std::time::Duration;

fn main() -> std::io::Result<()> {
    let dir = std::env::current_dir()?.join("ferrilog-rotation-demo");
    std::fs::create_dir_all(&dir)?;
    let log_path = dir.join("app.log");
    let log_path_str = log_path.to_str().unwrap();

    let _guard = ferrilog::builder()
        .level(ferrilog::DBG)
        .poller_interval(Duration::from_micros(100))
        // Direct output to a file with rotation.
        // Rotate when the file exceeds 1 KB, keeping at most 3 old files.
        .log_file(log_path_str)
        .rotation(ferrilog::RotationConfig::by_size(1024, 3))
        .start()?;

    ferrilog::set_thread_name("main");

    // Write enough messages to exceed 1 KB and trigger rotation.
    for i in 0..100u32 {
        ferrilog::info!(
            "rotation demo message {:0>4} -- padding to increase size xxxxxxxxxxxxxxxxxx",
            i
        );
    }

    // Drop the guard to flush and stop the poller before reading files.
    drop(_guard);

    // Give the OS a moment to finalize writes.
    std::thread::sleep(Duration::from_millis(50));

    // List the rotated files.
    println!("--- rotated files in {} ---", dir.display());
    let mut entries: Vec<_> = std::fs::read_dir(&dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in &entries {
        let meta = entry.metadata()?;
        println!("  {} ({} bytes)", entry.file_name().to_string_lossy(), meta.len());
    }

    // Clean up temporary files.
    std::fs::remove_dir_all(&dir)?;
    println!("--- cleaned up temp directory ---");

    Ok(())
}

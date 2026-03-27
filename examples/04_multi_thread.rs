//! Multi-threaded logging.
//!
//! Each writer thread has its own lock-free SPSC queue. The background
//! poller merges messages from all threads in timestamp order using a
//! min-heap, so the final output is globally time-ordered.
//!
//! Run with:
//!   cargo run --example 04_multi_thread

use std::{thread, time::Duration};

fn main() -> std::io::Result<()> {
    let _guard = ferrilog::builder()
        .level(ferrilog::DBG)
        // Use a short poller interval so messages appear quickly.
        .poller_interval(Duration::from_micros(100))
        .header_pattern("{level} [{thread}] - ")
        .start()?;

    ferrilog::set_thread_name("main");
    ferrilog::logi!("spawning 4 worker threads");

    let mut handles = Vec::new();
    for id in 0..4u32 {
        let handle = thread::spawn(move || {
            // Give each worker a descriptive thread name.
            ferrilog::set_thread_name(&format!("worker-{}", id));

            for i in 0..10u32 {
                ferrilog::logi!("thread {} message {} value={}", id, i, id * 100 + i);
            }
        });
        handles.push(handle);
    }

    // Main thread also logs while workers are running.
    for i in 0..5u32 {
        ferrilog::logi!("main message {}", i);
    }

    // Wait for all workers to finish.
    for h in handles {
        h.join().unwrap();
    }

    // The guard flushes all remaining messages on drop.
    // Output is time-ordered across all threads because the poller's
    // min-heap merges by timestamp.
    ferrilog::logi!("all workers finished");

    Ok(())
}

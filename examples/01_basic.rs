//! Simplest possible zerolog introduction.
//!
//! Shows how to initialize the logger with the builder API, log at different
//! levels, and rely on the guard for automatic cleanup.
//!
//! Run with:
//!   cargo run --example 01_basic

fn main() -> std::io::Result<()> {
    // Initialize the logger: output goes to stderr, minimum level is INFO.
    // The returned guard automatically stops the poller and flushes on drop.
    let _guard = zerolog::builder().level(zerolog::INFO).start()?;

    // Give the current thread a name so it appears in the log header.
    zerolog::set_thread_name("main");

    // INFO and above are emitted.
    zerolog::info!("hello world");

    // DBG is below INFO, so this message is silently discarded.
    zerolog::debug!("debug={}", 42u32);

    // Warnings and errors are always above INFO.
    zerolog::warn!("warning={}", true);
    zerolog::error!("error code={}", -1i32);

    // When `_guard` goes out of scope here, the poller thread is stopped
    // and all buffered output is flushed automatically.
    Ok(())
}

//! Simplest possible ferrilog introduction.
//!
//! Shows how to initialize the logger with the builder API, log at different
//! levels, and rely on the guard for automatic cleanup.
//!
//! Run with:
//!   cargo run --example 01_basic

fn main() -> std::io::Result<()> {
    // Initialize the logger: output goes to stderr, minimum level is INFO.
    // The returned guard automatically stops the poller and flushes on drop.
    let _guard = ferrilog::builder().level(ferrilog::INFO).start()?;

    // Give the current thread a name so it appears in the log header.
    ferrilog::set_thread_name("main");

    // INFO and above are emitted.
    ferrilog::logi!("hello world");

    // DBG is below INFO, so this message is silently discarded.
    ferrilog::logd!("debug={}", 42u32);

    // Warnings and errors are always above INFO.
    ferrilog::logw!("warning={}", true);
    ferrilog::loge!("error code={}", -1i32);

    // When `_guard` goes out of scope here, the poller thread is stopped
    // and all buffered output is flushed automatically.
    Ok(())
}

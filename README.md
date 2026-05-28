# ferrilog

A high-performance, lock-free structured logging library for Rust, targeting sub-30ns hot-path latency per log call.

Core design principle: **zero formatting on the hot path, asynchronous decoding on a background thread**.

## Highlights

- **~17ns per log call** — binary-encode arguments into a per-thread SPSC ring buffer; no string formatting, no locks, no allocation on the hot path.
- **480M msgs/sec** enqueue-only; **1.4 billion msgs/sec** with 4 writer threads.
- **Lock-free everywhere** — no `Mutex` in production code. Atomic operations only where strictly necessary (SPSC sync, level check).
- **Compile-time optimized** — the `log!` macro auto-selects the fastest encoding path; format specifiers (`{:x}`, `{:.2}`) are dispatched at compile time via const generics.
- **Background poller** — a dedicated thread drains all per-thread queues, merges messages by timestamp (min-heap), formats headers, and writes to file.
- **Builder API + RAII guard** — idiomatic Rust initialization; the guard auto-stops the poller and flushes on drop.
- **Log file rotation** — automatic size-based rotation with configurable max file size and retention count.
- **CPU core pinning** — optionally pin the poller thread to a specific core (Linux `sched_setaffinity`).
- **Zero-cost level filtering** — a single `Relaxed` atomic load (one `mov` instruction on x86).
- **`#[derive(Encode)]`** — log any `Copy + Display` type with zero boilerplate.

## Performance

Measured on Apple M3 (aarch64), Rust nightly, `lto = "fat"`, `codegen-units = 1`.

### Hot-path latency (enqueue only, body-only header)

```
latency/no_args         time:   [17.322 ns 17.475 ns 17.634 ns]
latency/single_i32      time:   [17.541 ns 17.733 ns 17.924 ns]
latency/i32_and_str     time:   [18.094 ns 18.193 ns 18.299 ns]
latency/custom_type     time:   [18.180 ns 18.219 ns 18.262 ns]
latency/fast_path       time:   [18.577 ns 18.706 ns 18.839 ns]
```

| Benchmark | Median | Notes |
|-----------|--------|-------|
| `log!` no args | **17.5 ns** | Bare message, no arguments |
| `log!` single i32 | **17.7 ns** | One integer argument |
| `log!` i32 + &str | **18.2 ns** | Mixed fixed + variable-length |
| `log!` custom type | **18.2 ns** | `#[derive(Encode)]` struct |

### Throughput

```
throughput_enqueue_only/single_thread
                        time:   [104.03 µs 104.16 µs 104.29 µs]
                        thrpt:  [479.45 Melem/s 480.04 Melem/s 480.62 Melem/s]

throughput_end_to_end/single_thread
                        time:   [340.34 µs 345.83 µs 350.30 µs]
                        thrpt:  [285.47 Melem/s 289.16 Melem/s 293.83 Melem/s]

throughput_end_to_end/4_threads
                        time:   [70.399 µs 71.275 µs 72.488 µs]
                        thrpt:  [1.3795 Gelem/s 1.4030 Gelem/s 1.4205 Gelem/s]
```

| Benchmark | Median | Notes |
|-----------|--------|-------|
| Enqueue-only (1 thread) | **480 M msgs/sec** | Pure producer speed, no formatting |
| End-to-end (1 writer + poller) | **289 M msgs/sec** | Writer + background poller concurrently |
| End-to-end (4 writers + poller) | **1.40 G msgs/sec** | 4 writer threads + background poller |

### Component breakdown

```
core/queue_thread_buffer/spsc_alloc_push_pop_16b
                        time:   [2.3424 ns 2.4034 ns 2.5024 ns]
core/queue_thread_buffer/thread_buffer_alloc_message_16b
                        time:   [2.3164 ns 2.3250 ns 2.3331 ns]
core/timestamp_poller_logger/timestamp_read_counter
                        time:   [247.48 ps 248.36 ps 249.59 ps]
core/timestamp_poller_logger/timestamp_to_nanoseconds
                        time:   [1.8024 ns 1.8159 ns 1.8311 ns]
core/timestamp_poller_logger/logger_poll_body_only_u32_x64
                        time:   [616.55 ns 619.89 ns 622.70 ns]
```

| Component | Median | Notes |
|-----------|--------|-------|
| TSC read (`rdtsc` / `cntvct_el0`) | **0.25 ns** | Hardware counter instruction |
| Timestamp conversion (seqlock) | **1.8 ns** | `Acquire` fence only (no `mfence` on x86) |
| SPSC alloc + push + pop cycle | **2.4 ns** | Full producer-consumer round trip |
| Thread buffer alloc (TLS) | **2.3 ns** | `#[thread_local]` + `producer_alloc` |
| Poll decode (64 msgs, body-only) | **620 ns** | ~9.7 ns/msg background decode+format |

All latency measurements are **end-to-end hot-path**: from the `log!()` call to the return, including level check, buffer allocation, timestamp read, binary encoding, and atomic publish.

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
ferrilog = "0.1"
```

> **Requires Rust nightly** — ferrilog uses `#![feature(thread_local, min_specialization, core_intrinsics)]`.

### Minimal example

```rust
use std::time::Duration;
use ferrilog::{builder, INFO, ERR};

fn main() -> std::io::Result<()> {
    // Initialize with the builder — guard auto-flushes on drop
    let _guard = builder()
        .level(INFO)
        .poller_interval(Duration::from_millis(1))
        .start()?;

    ferrilog::info!("server started on port {}", 8080u32);
    ferrilog::warn!("connection pool at {}% capacity", 85u32);
    ferrilog::error!("request failed with code {:#x}", 0xDEADu32);

    Ok(())
    // _guard drops here: stops poller, flushes all pending messages
}
```

Output:

```
[2026-03-28 12:00:00.123456789] INF [main] src/main.rs:8 - server started on port 8080
[2026-03-28 12:00:00.123457012] WRN [main] src/main.rs:9 - connection pool at 85% capacity
[2026-03-28 12:00:00.123457234] ERR [main] src/main.rs:10 - request failed with code 0xdead
```

## Log Macros

| Macro | Level | Example |
|-------|-------|---------|
| `debug!(...)` | Debug | `debug!("x={}", x)` |
| `info!(...)` | Info | `info!("started")` |
| `warn!(...)` | Warning | `warn!("slow query: {}ms", elapsed)` |
| `error!(...)` | Error | `error!("failed: {}", err)` |
| `log!(level, ...)` | Any | `log!(Level::WRN, "custom={}", v)` |

The `log!` macro **automatically selects the optimal encoding path** at compile time:
- All `{}` placeholders (no format specifiers) → **fast path** (`EncodeFast` trait)
- Any format specifier (`{:x}`, `{:.2}`, `{:>10}`) → **normal path** (`Encode` trait) with compile-time const-generic dispatch

## Format Specifiers

```rust
ferrilog::info!("decimal:    {}", 255u32);         // 255
ferrilog::info!("hex:        {:x}", 255u32);       // ff
ferrilog::info!("upper hex:  {:#X}", 255u32);      // 0xFF
ferrilog::info!("binary:     {:#b}", 10u32);       // 0b1010
ferrilog::info!("octal:      {:o}", 255u32);       // 377
ferrilog::info!("precision:  {:.3}", 3.14159f64);  // 3.142
ferrilog::info!("right-pad:  {:>10}", 42u32);      //         42
ferrilog::info!("left-pad:   {:<10}", 42u32);      // 42
ferrilog::info!("center:     {:^10}", 42u32);      //     42
ferrilog::info!("zero-pad:   {:08}", 42u32);       // 00000042
```

## Custom Types

Any `Copy + 'static + Display` type can be logged with `#[derive(ferrilog::Encode)]`:

```rust
#[derive(Clone, Copy, ferrilog::Encode)]
struct Order {
    id: u64,
    price: f64,
    qty: u32,
}

impl std::fmt::Display for Order {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Order{{id={}, price={:.2}, qty={}}}", self.id, self.price, self.qty)
    }
}

ferrilog::info!("new order: {}", Order { id: 1001, price: 49.99, qty: 100 });
// → new order: Order{id=1001, price=49.99, qty=100}
```

On the hot path, the struct is `memcpy`-ed into the ring buffer (binary encoding). Formatting via `Display` happens later on the background thread.

## Builder Configuration

```rust
use std::time::Duration;
use ferrilog::{builder, INFO, ERR, QueueFullPolicy, RotationConfig};

let _guard = builder()
    // Minimum log level (messages below this are discarded)
    .level(INFO)
    // Log output file with automatic rotation
    .log_file("/var/log/app.log")
    .rotation(
        RotationConfig::new()
            .max_size_mb(100)   // Rotate at 100 MB
            .max_files(5)       // Keep 5 old files
    )
    // Custom header pattern
    .header_pattern("[{date} {time}.{ns}] {level} [{thread}] {location} - ")
    // Flush policy
    .flush_delay(Duration::from_secs(3))
    .flush_buffer_size(8192)
    .flush_on(ERR)              // ERR and above trigger immediate flush
    // Queue-full behavior
    .queue_full_policy(QueueFullPolicy::Drop)
    // Poller thread
    .poller_interval(Duration::from_millis(1))
    .poller_core_affinity(3)    // Pin to CPU core 3 (Linux only)
    .start()?;
```

### Header Pattern Tokens

| Token | Output | Example |
|-------|--------|---------|
| `{date}` | `YYYY-MM-DD` | `2026-03-28` |
| `{time}` | `HH:MM:SS` | `14:30:05` |
| `{ns}` | nanoseconds (9 digits) | `123456789` |
| `{level}` | `DBG`/`INF`/`WRN`/`ERR` | `INF` |
| `{thread}` | Thread name | `worker-0` |
| `{location}` | `file:line` | `src/main.rs:42` |

## Multi-threaded Logging

Each thread gets its own lock-free SPSC ring buffer. No contention between threads:

```rust
use std::thread;

for i in 0..4 {
    thread::spawn(move || {
        ferrilog::set_thread_name(&format!("worker-{i}"));
        ferrilog::info!("hello from thread {}", i as u32);
    });
}
```

The background poller merges messages from all threads by timestamp using a min-heap, producing globally time-ordered output.

## Buffer Size Configuration

The per-thread ring buffer size is selected at compile time via Cargo features:

```toml
[dependencies]
ferrilog = { git = "...", features = ["buffer-4m"] }
```

| Feature | Size | Memory per thread |
|---------|------|-------------------|
| `buffer-1m` | 1 MiB | 1 MiB |
| **`buffer-2m`** | **2 MiB** | **2 MiB (default)** |
| `buffer-4m` | 4 MiB | 4 MiB |
| `buffer-8m` | 8 MiB | 8 MiB |
| `buffer-16m` | 16 MiB | 16 MiB |
| `buffer-32m` | 32 MiB | 32 MiB |
| `buffer-64m` | 64 MiB | 64 MiB |

If multiple features are enabled (Cargo dependency unification), the **largest** size wins.

## Log File Rotation

Size-based rotation is handled automatically by the background poller:

```rust
use ferrilog::RotationConfig;

let _guard = ferrilog::builder()
    .log_file("/var/log/app.log")
    .rotation(RotationConfig::by_size(100 * 1024 * 1024, 5))
    .start()?;
```

When the file exceeds `max_size`, the poller renames:
```
app.log   → app.log.1
app.log.1 → app.log.2
...
app.log.4 → app.log.5 (oldest deleted if > max_files)
```

## Architecture

```
Writer Thread A ──→ [SPSC Ring Buffer A] ──┐
Writer Thread B ──→ [SPSC Ring Buffer B] ──┤
Writer Thread C ──→ [SPSC Ring Buffer C] ──┤
                                           ▼
                              ┌─── Background Poller ───┐
                              │  1. Calibrate TSC        │
                              │  2. Drain all queues     │
                              │  3. Min-heap merge       │
                              │  4. Decode + format      │
                              │  5. Write to file/stderr │
                              │  6. Rotate if needed     │
                              └──────────────────────────┘
```

**Hot path** (per `log!()` call, ~17ns):
1. Level check — single `Relaxed` atomic load
2. `OnceLock` state check — one atomic load (cached after first call)
3. TLS buffer pointer — one instruction via `#[thread_local]`
4. SPSC `producer_alloc` — pointer arithmetic, no atomics on fast path
5. Write `info_ptr` + timestamp + encoded args — plain memory writes
6. `producer_push` — single `Release` atomic store

**Cold path** (background poller, ~10ns/message for decode):
1. `consumer_front` with `prefetch` for next message
2. Read `StaticInfo` pointer from payload
3. Convert TSC timestamp to nanoseconds (seqlock, `Acquire` fence only)
4. Tokenized header write
5. Invoke monomorphized decoder function
6. Flush to writer

## Building

```bash
# Requires Rust nightly
rustup toolchain install nightly
rustup default nightly

# Run tests
cargo test

# Run clippy
cargo clippy --all-targets

# Run benchmarks
cargo bench --bench latency_throughput -- 'latency/'

# Run examples
cargo run --example 01_basic
cargo run --example 07_complete
```

## Examples

| Example | Description |
|---------|-------------|
| [`01_basic`](examples/01_basic.rs) | Minimal setup with builder API |
| [`02_format_spec`](examples/02_format_spec.rs) | All format specifiers |
| [`03_custom_type`](examples/03_custom_type.rs) | `#[derive(Encode)]` for custom types |
| [`04_multi_thread`](examples/04_multi_thread.rs) | Multi-threaded logging with thread names |
| [`05_file_rotation`](examples/05_file_rotation.rs) | File output with size-based rotation |
| [`06_flush_policy`](examples/06_flush_policy.rs) | Flush delay, buffer size, level triggers |
| [`07_complete`](examples/07_complete.rs) | Complete showcase of all features |

## License

MIT

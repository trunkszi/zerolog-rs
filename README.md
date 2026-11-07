# zerolog-rs

A structured logging library with lock-free producer queues and asynchronous background formatting for low hot-path producer overhead.

Core design principle: **zero formatting on the hot path, asynchronous decoding on a background thread**.

## Why zerolog-rs?

In ultra-low-latency systems, logging on the critical path typically fails because of **three fundamental bottlenecks**:

1. **System Calls & OS Clock Overhead**
   - *The Problem*: Traditional loggers query wall-clock time via OS time functions (`clock_gettime`) and issue kernel I/O syscalls on the caller thread.
   - *How `zerolog-rs` solves it*: `zerolog-rs` bypasses syscalls completely on the hot path by reading the CPU hardware timestamp counter directly (`rdtsc` on `x86_64`, `cntvct_el0` on `aarch64`) without a wall-clock syscall. Hardware ticks are calibrated and converted to wall-clock nanoseconds by the serialized consumer. Counter cost depends on CPU and instruction ordering; an unfenced sample is not a global ordering guarantee.

2. **Hot-Path String Formatting**
   - *The Problem*: Formatting integers, floats, and structs into `String` buffers via `std::fmt` is CPU-intensive and branch-heavy. Running string formatting synchronously on a worker thread steals cycles directly from business logic.
   - *How `zerolog-rs` solves it*: **Zero formatting on the hot path.** The caller thread only performs raw binary data movement—copying argument bytes alongside a compile-time `&'static StaticInfo` descriptor into the buffer—and offloads all string formatting, float/hex rendering, and header assembly to a dedicated background poller thread.

3. **Heap Allocations & Cross-Thread Contention**
   - *The Problem*: Repeated `String` and `Vec` allocations introduce allocator jitter and cache pollution, while shared `Mutex` writers or MPSC channels cause cross-core CAS contention under burst load.
   - *How `zerolog-rs` solves it*: `zerolog-rs` pre-allocates a `#[thread_local]` ring buffer for every thread, forming a **lock-free SPSC (Single-Producer Single-Consumer) queue** per worker. Warmed scalar enqueue paths do not allocate or take the consumer mutex. Release publication and Acquire consumption still transfer cache-line ownership; first use allocates the thread buffer, and Block policy waits for consumer progress.

### Battle-Tested in Production HFT & Familiar Rust Ergonomics

`zerolog-rs` is deployed in production **High-Frequency Trading (HFT)** systems as the primary low-latency observability and audit logging layer across market data feeds, strategy engines, and order-entry gateways.

Despite its binary SPSC architecture under the hood, `zerolog-rs` preserves the idiomatic Rust logging API surface you already know:
- Standard level macros (`trace!`, `debug!`, `info!`, `warn!`, `error!`, `log!`)
- Full Rust format specifiers (`{}`, `{:x}`, `{:#X}`, `{:.3}`, `{:>10}`, `{:08}`) dispatched at compile time via const generics
- Lightweight `span!` and `.instrument()` context tracking without `tracing-subscriber` dynamic dispatch overhead
- `#[derive(Encode)]` to log custom `Send + Copy + 'static + Display` domain structs by raw `memcpy` on the hot path and deferred `Display` formatting on the poller thread

### What You Gain in Low-Latency Scenarios

- **Deferred formatting**: Producers publish encoded arguments; consumer formatting, I/O and rotation run separately. Latency and tail behavior depend on hardware, arguments and queue pressure.
- **Per-thread SPSC queues**: Producers publish records with Release/Acquire synchronization. Consumer progress returns through a shared cache line, so producer/consumer cache-line handoff still occurs. The consumer merges currently published queue fronts by timestamp; this does not establish strict global order.
- **Consumer CPU affinity**: `poller_core_affinity` can place background work on a housekeeping core; it does not isolate shared caches or remove OS scheduling effects.

## Highlights

- **Producer-side binary encoding** — initialized scalar paths enqueue without consumer-side formatting, allocation or mutex acquisition.
- **Explicit measurement boundaries** — enqueue latency and count-verified drained throughput are separate benchmarks.
- **Lock-free producer path** — consumer/configuration access uses a production mutex; lifecycle locking excludes starts from stopped-only configuration. Producers do not take these locks.
- **Compile-time optimized** — the `log!` macro auto-selects the fastest encoding path; format specifiers (`{:x}`, `{:.2}`) are dispatched at compile time via const generics.
- **Background poller** — a dedicated thread drains all per-thread queues, merges messages by timestamp (min-heap), formats headers, and writes to file.
- **Builder API + RAII guard** — idiomatic Rust initialization; the guard auto-stops the poller and flushes on drop.
- **Log file rotation** — automatic size-based rotation with configurable max file size and retention count.
- **CPU core pinning** — optionally pin the poller thread to a specific core (Linux `sched_setaffinity`).
- **Zero-cost level filtering** — a single `Relaxed` atomic load (one `mov` instruction on x86).
- **`#[derive(Encode)]`** — log any `Send + Copy + 'static + Display` type with zero boilerplate.

## Performance measurement

Run the benchmarks on the deployment hardware rather than treating historical
attempt rates as a latency or persistence guarantee:

```sh
cargo bench --bench latency_throughput
cargo bench --bench core_component_bench
```

The latency group times each producer call and drains outside that interval;
timer overhead is part of those samples. Enqueue-only throughput uses a bounded
batch and checks for queue-full events. End-to-end groups use blocking queue
policy, include final joined drain/flush in timing, and verify exact record
completion at a counting sink. The sink does not measure durable disk I/O.
Component benchmarks measure their named operation; a batched timestamp loop is
not evidence that an isolated counter read takes a fraction of a nanosecond.

## Quick Start

Add to your `Cargo.toml`:

```toml
[dependencies]
zerolog = { version = "0.1", package = "zerolog-rs" }
```

> **Requires Rust nightly** — zerolog uses `#![feature(thread_local, min_specialization, core_intrinsics)]`.

### Minimal example

```rust
use std::time::Duration;
use zerolog::{builder, INFO, ERR};

fn main() -> std::io::Result<()> {
    // Initialize with the builder — guard auto-flushes on drop
    let _guard = builder()
        .level(INFO)
        .poller_interval(Duration::from_millis(1))
        .start()?;

    zerolog::info!("server started on port {}", 8080u32);
    zerolog::warn!("connection pool at {}% capacity", 85u32);
    zerolog::error!("request failed with code {:#x}", 0xDEADu32);

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
zerolog::info!("decimal:    {}", 255u32);         // 255
zerolog::info!("hex:        {:x}", 255u32);       // ff
zerolog::info!("upper hex:  {:#X}", 255u32);      // 0xFF
zerolog::info!("binary:     {:#b}", 10u32);       // 0b1010
zerolog::info!("octal:      {:o}", 255u32);       // 377
zerolog::info!("precision:  {:.3}", 3.14159f64);  // 3.142
zerolog::info!("right-pad:  {:>10}", 42u32);      //         42
zerolog::info!("left-pad:   {:<10}", 42u32);      // 42
zerolog::info!("center:     {:^10}", 42u32);      //     42
zerolog::info!("zero-pad:   {:08}", 42u32);       // 00000042
```

## Custom Types

Any `Copy + 'static + Display` type can be logged with `#[derive(zerolog::Encode)]`:

```rust
#[derive(Clone, Copy, zerolog::Encode)]
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

zerolog::info!("new order: {}", Order { id: 1001, price: 49.99, qty: 100 });
// → new order: Order{id=1001, price=49.99, qty=100}
```

On the hot path, the struct is `memcpy`-ed into the ring buffer (binary encoding). Formatting via `Display` happens later on the background thread.

## Builder Configuration

```rust
use std::time::Duration;
use zerolog::{builder, INFO, ERR, QueueFullPolicy, RotationConfig};

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

Each producer has its own SPSC ring buffer; it does not share a multi-producer queue:

```rust
use std::thread;

for i in 0..4 {
    thread::spawn(move || {
        zerolog::set_thread_name(&format!("worker-{i}"));
        zerolog::info!("hello from thread {}", i as u32);
    });
}
```

The background poller merges **currently published ready queue fronts** by timestamp.
On invariant-TSC systems this gives best-effort cross-thread ordering. A producer
can sample an older timestamp, pause before publication, and become visible after
a newer record was emitted; output is therefore not globally monotonic. Manual
and background polls serialize under the same consumer lock. Registry snapshots,
heap use and retired-buffer reclamation all share that exclusion.

The producer's Release publication and consumer's Acquire observation transfer
record visibility; consumer progress returns through the shared read-index cache
line. SPSC avoids multi-producer queue contention, not cache-coherence traffic.

Output retries retain only unwritten bytes and do not append a consumed record a
second time. Pending formatted output is bounded at 16 MiB; exhaustion returns
`WouldBlock` and leaves the unconsumed queue record for retry. A persistently
failed writer therefore applies backpressure to bounded producer queues, whose
configured Drop/Block policy still applies. A rotation failure retains the open
writer and reports the error. `guard.shutdown()` returns shutdown/flush errors;
dropping a stale guard cannot stop a newer generation. Manual stop also revokes
the guard, so later manual configuration is not drained by its Drop.

## Buffer Size Configuration

The per-thread ring buffer size is selected at compile time via Cargo features:

```toml
[dependencies]
zerolog = { version = "0.1", package = "zerolog-rs", features = ["buffer-4m"] }
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
use zerolog::RotationConfig;

let _guard = zerolog::builder()
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

**Producer path** (warmed scalar logging):
1. Level check — single `Relaxed` atomic load
2. Thread-local buffer lookup — first use performs cold allocation/registration
3. TLS buffer pointer — one instruction via `#[thread_local]`
4. SPSC `producer_alloc` — pointer arithmetic, no atomics on fast path
5. Write `info_ptr` + timestamp + encoded args — plain memory writes
6. `producer_push` — single `Release` atomic store

**Consumer path** (serialized manual/background polling):
1. `consumer_front` with `prefetch` for next message
2. Read `StaticInfo` pointer from payload
3. Convert raw ticks using the consumer-owned calibration mapping
4. Tokenized header write
5. Invoke monomorphized decoder function
6. Flush to writer


### Throughput measurement boundaries

`latency_throughput` separates enqueue-only timing from completed end-to-end
throughput. Enqueue batches must fit the queue and assert zero queue-full events.
End-to-end groups use the blocking queue policy, time producer work through the
joined final drain and flush, and assert the exact completed record count at a
counting sink. These are formatting-to-sink measurements, not durable disk I/O.
Enqueue-only attempt rates must not be interpreted as drained end-to-end
throughput or as guarantees for other machines or workloads.

//! Latency and throughput benchmarks for zerolog.
//!
//! - **Latency benchmarks** measure the hot-path cost of a single log!() call
//!   (enqueue only, body-only with empty header pattern).
//! - **Throughput benchmarks** measure sustained write + poll consumption rate
//!   in messages per second.
//!
//! All benchmarks use a SinkWriter (discards output) and an empty header
//! pattern to isolate encode/decode performance from I/O and header formatting.

use std::{
    hint::black_box,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use criterion::{Criterion, criterion_group, criterion_main};
use zerolog::{
    INFO, OFF, flush_on, poll, set_flush_buffer_size, set_flush_delay, set_header_pattern,
    set_level, set_output_writer, stop_polling_thread,
};

// ---------------------------------------------------------------------------
// Custom type for latency_custom_type benchmark
// ---------------------------------------------------------------------------

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
// Sink writer: discards all output to isolate encode/decode from I/O
// ---------------------------------------------------------------------------

struct SinkWriter {
    records: Arc<AtomicU64>,
}

impl std::io::Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.records
            .fetch_add(buf.iter().filter(|&&b| b == b'\n').count() as u64, Ordering::Relaxed);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Runtime configuration helper
// ---------------------------------------------------------------------------

/// Configure the logger for benchmarking: empty header, SinkWriter output,
/// flush buffer size of 1 byte (so poll always flushes), and INFO level.
fn configure_bench_runtime() -> Arc<AtomicU64> {
    stop_polling_thread().unwrap();
    poll(true).unwrap();
    let records = Arc::new(AtomicU64::new(0));
    set_output_writer(SinkWriter { records: Arc::clone(&records) });
    zerolog::set_queue_full_policy(zerolog::QueueFullPolicy::Drop);
    set_level(INFO);
    set_flush_delay(3_000_000_000);
    set_flush_buffer_size(1);
    flush_on(OFF);
    set_header_pattern("").unwrap();
    poll(true).unwrap();
    records
}

// ---------------------------------------------------------------------------
// Emit helpers (marked #[inline(never)] to produce realistic call overhead)
// ---------------------------------------------------------------------------

#[inline(never)]
fn emit_no_args() {
    zerolog::log!(INFO, "ping");
}

#[inline(never)]
fn emit_single_i32(v: i32) {
    zerolog::log!(INFO, "value={}", black_box(v));
}

#[inline(never)]
fn emit_i32_and_str(v: i32, s: &'static str) {
    zerolog::log!(INFO, "value={} tag={}", black_box(v), black_box(s));
}

#[inline(never)]
fn emit_custom_type(order: Order) {
    zerolog::log!(INFO, "order={}", black_box(order));
}

#[inline(never)]
fn emit_fast_i32_and_str(v: i32, s: &'static str) {
    zerolog::log_fast!(INFO, "value={} tag={}", black_box(v), black_box(s));
}

// ---------------------------------------------------------------------------
// Latency benchmarks: measure single-log hot path (enqueue only)
// ---------------------------------------------------------------------------

fn bench_latency(c: &mut Criterion) {
    configure_bench_runtime();

    // Warm up the thread buffer and call sites
    emit_no_args();
    emit_single_i32(1);
    emit_i32_and_str(1, "x");
    emit_custom_type(Order { id: 1, price: 1.0, quantity: 1 });
    emit_fast_i32_and_str(1, "x");
    poll(true).unwrap();

    let mut group = c.benchmark_group("latency");

    // Measure only the enqueue (log!()) call; poll after each to drain the queue
    // so it never fills up.

    group.bench_function("no_args", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                emit_no_args();
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("single_i32", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                emit_single_i32(42);
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("i32_and_str", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                emit_i32_and_str(42, "benchmark");
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("custom_type", |b| {
        let order = Order { id: 100001, price: 49.99, quantity: 200 };
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                emit_custom_type(order);
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("fast_path", |b| {
        b.iter_custom(|iters| {
            let mut total = Duration::ZERO;
            for _ in 0..iters {
                let start = Instant::now();
                emit_fast_i32_and_str(42, "benchmark");
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Throughput benchmarks
//
// Three tiers:
//   1. enqueue_only   — pure producer speed (no poll, no formatting)
//   2. end_to_end     — producer through final formatted sink write/flush
//   3. end_to_end_4t  — same completion boundary with 4 persistent producers
// ---------------------------------------------------------------------------

/// Batch size for enqueue-only throughput.
/// Fits even the smallest supported 1 MiB buffer for this fixed u32 record.
const ENQUEUE_BATCH: usize = 10_000;

/// Batch size for end-to-end throughput (can be larger since poller drains
/// the queue concurrently).
const E2E_BATCH: usize = 100_000;

fn bench_throughput(c: &mut Criterion) {
    let records = configure_bench_runtime();
    set_flush_buffer_size(u32::MAX);

    // Warm up
    emit_single_i32(1);
    poll(true).unwrap();

    // -----------------------------------------------------------------------
    // 1. Enqueue-only: pure producer speed (no poll during measurement)
    // -----------------------------------------------------------------------
    {
        let mut group = c.benchmark_group("throughput_enqueue_only");
        group.throughput(criterion::Throughput::Elements(ENQUEUE_BATCH as u64));

        group.bench_function("single_thread", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    let before = records.load(Ordering::Relaxed);
                    zerolog::reset_queue_full_count();
                    let start = Instant::now();
                    for i in 0..ENQUEUE_BATCH {
                        emit_single_i32(i as i32);
                    }
                    total += start.elapsed();
                    // Drain is untimed only for the explicitly enqueue-only group.
                    poll(true).unwrap();
                    assert_eq!(zerolog::queue_full_count(), 0, "enqueue batch must fit");
                    assert_eq!(records.load(Ordering::Relaxed) - before, ENQUEUE_BATCH as u64);
                }
                total
            });
        });

        group.finish();
    }

    // -----------------------------------------------------------------------
    // 2. End-to-end: accepted producer records through final sink flush.
    // -----------------------------------------------------------------------
    zerolog::set_queue_full_policy(zerolog::QueueFullPolicy::Block);
    {
        let mut group = c.benchmark_group("throughput_end_to_end");
        group.throughput(criterion::Throughput::Elements(E2E_BATCH as u64));

        group.bench_function("single_thread", |b| {
            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    // Start poller in the background
                    zerolog::start_polling_thread(100_000).unwrap(); // 100µs interval
                    let before = records.load(Ordering::Relaxed);
                    zerolog::reset_queue_full_count();
                    let start = Instant::now();
                    for i in 0..E2E_BATCH {
                        emit_single_i32(i as i32);
                    }
                    // Stop poller and drain remaining before stopping the timer.
                    zerolog::stop_polling_thread().unwrap();
                    poll(true).unwrap();
                    total += start.elapsed();
                    assert_eq!(records.load(Ordering::Relaxed) - before, E2E_BATCH as u64);
                }
                total
            });
        });

        group.bench_function("4_threads", |b| {
            use std::sync::{
                Arc, Barrier,
                atomic::{AtomicBool, Ordering as AO},
            };

            let per_thread = E2E_BATCH / 4;

            // Spawn 4 persistent worker threads that wait on a barrier.
            // This avoids exceeding MAX_THREADS across criterion iterations.
            let go = Arc::new(Barrier::new(5)); // 4 workers + 1 main
            let done = Arc::new(Barrier::new(5));
            let stop = Arc::new(AtomicBool::new(false));

            let workers: Vec<_> = (0..4)
                .map(|tid| {
                    let go = Arc::clone(&go);
                    let done = Arc::clone(&done);
                    let stop = Arc::clone(&stop);
                    thread::spawn(move || {
                        zerolog::set_thread_name(&format!("bench-{tid}"));
                        loop {
                            go.wait();
                            if stop.load(AO::Relaxed) {
                                done.wait();
                                return;
                            }
                            for i in 0..per_thread {
                                emit_single_i32((tid * per_thread as u32 + i as u32) as i32);
                            }
                            done.wait();
                        }
                    })
                })
                .collect();

            b.iter_custom(|iters| {
                let mut total = Duration::ZERO;
                for _ in 0..iters {
                    zerolog::start_polling_thread(100_000).unwrap();

                    let before = records.load(Ordering::Relaxed);
                    zerolog::reset_queue_full_count();
                    let start = Instant::now();
                    go.wait(); // release workers
                    done.wait(); // wait for completion
                    zerolog::stop_polling_thread().unwrap();
                    poll(true).unwrap();
                    total += start.elapsed();
                    assert_eq!(records.load(Ordering::Relaxed) - before, E2E_BATCH as u64);
                }
                total
            });

            // Shut down worker threads
            stop.store(true, AO::Relaxed);
            go.wait();
            done.wait();
            for w in workers {
                w.join().unwrap();
            }
        });

        group.finish();
    }
}

// ---------------------------------------------------------------------------
// Criterion harness
// ---------------------------------------------------------------------------

criterion_group! {
    name = latency_throughput;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(2))
        .sample_size(30);
    targets = bench_latency, bench_throughput
}
criterion_main!(latency_throughput);

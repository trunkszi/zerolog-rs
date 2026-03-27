use std::{
    hint::black_box,
    time::{Duration, Instant},
};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ferrilog::{
    Encode, EncodeConstDefaultFormat, FORMAT_ALIGN_CENTER, INFO, OFF, append_display_with_spec,
    flush_on, poll, set_flush_buffer_size, set_flush_delay, set_header_pattern, set_level,
    set_output_writer, stop_polling_thread,
};

const DEFAULT_HEADER_PATTERN: &str = "[{date} {time}.{ns}] {level} [{thread}] {location} - ";
const BODY_ONLY_HEADER_PATTERN: &str = "";
const LEVEL_THREAD_LOCATION_HEADER_PATTERN: &str = "{level} [{thread}] {location} - ";
const STATIC_COMPLEX_HEADER_PATTERN: &str = "{thread}|{level}|{location}|{date} {time}.{ns}| ";
const BATCH_SIZE: usize = 256;

#[derive(Clone, Copy, ferrilog::Encode)]
struct Point {
    x: i32,
    y: i32,
}

impl std::fmt::Display for Point {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "({}, {})", self.x, self.y)
    }
}

struct SinkWriter;

impl std::io::Write for SinkWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn configure_runtime(header_pattern: &str, flush_buffer_size: u32) {
    stop_polling_thread().unwrap();
    let _ = poll(true);
    set_output_writer(SinkWriter);
    set_level(INFO);
    set_flush_delay(3_000_000_000);
    set_flush_buffer_size(flush_buffer_size);
    flush_on(OFF);
    set_header_pattern(header_pattern).unwrap();
    let _ = poll(true);
}

fn configure_runtime_static_complex_header(flush_buffer_size: u32) {
    stop_polling_thread().unwrap();
    let _ = poll(true);
    set_output_writer(SinkWriter);
    set_level(INFO);
    set_flush_delay(3_000_000_000);
    set_flush_buffer_size(flush_buffer_size);
    flush_on(OFF);
    ferrilog::set_header_pattern_static!("{thread}|{level}|{location}|{date} {time}.{ns}| ")
        .unwrap();
    let _ = poll(true);
}

#[inline(never)]
fn emit_no_args() {
    ferrilog::log!(INFO, "plain message");
}

#[inline(never)]
fn emit_u32(value: u32) {
    ferrilog::log!(INFO, "value={}", black_box(value));
}

#[inline(never)]
fn emit_fast_u32(value: u32) {
    ferrilog::log_fast!(INFO, "value={}", black_box(value));
}

#[inline(never)]
fn emit_u32_str(value: u32, text: &'static str) {
    ferrilog::log!(INFO, "value={} text={}", black_box(value), black_box(text));
}

#[inline(never)]
fn emit_fast_u32_str(value: u32, text: &'static str) {
    ferrilog::log_fast!(INFO, "value={} text={}", black_box(value), black_box(text));
}

#[inline(never)]
fn emit_u32_pair(left: u32, right: u32) {
    ferrilog::log!(INFO, "a={} b={}", black_box(left), black_box(right));
}

#[inline(never)]
fn emit_hex(value: u32) {
    ferrilog::log!(INFO, "hex={:x}", black_box(value));
}

#[inline(never)]
fn emit_padded_u32(value: u32) {
    ferrilog::log!(INFO, "pad={:>12}", black_box(value));
}

#[inline(never)]
fn emit_padded_hex(value: u32) {
    ferrilog::log!(INFO, "hex={:#010x}", black_box(value));
}

#[inline(never)]
fn emit_signed_padded_hex(value: i32) {
    ferrilog::log!(INFO, "hex={:#012x}", black_box(value));
}

#[inline(never)]
fn emit_scientific_f64(value: f64) {
    ferrilog::log!(INFO, "float={:>14.3e}", black_box(value));
}

#[inline(never)]
fn emit_point(point: Point) {
    ferrilog::log!(INFO, "point={}", black_box(point));
}

#[inline(never)]
fn emit_padded_point(point: Point) {
    ferrilog::log!(INFO, "point={:^12.5}", black_box(point));
}

fn bench_custom_display_component(criterion: &mut Criterion) {
    let point = Point { x: 7, y: 9 };
    let mut encoded = vec![0u8; point.encoded_size() + 16];
    unsafe {
        let mut offset = 0;
        point.encode(encoded.as_mut_ptr(), &mut offset);
    }

    let mut group = criterion.benchmark_group("component_custom_display");

    group.bench_function("point_decode_empty_runtime_display", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let value = encoded.as_ptr().cast::<Point>().read_unaligned();
                append_display_with_spec(&mut output, value, "");
                black_box(&output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("point_decode_empty_derived_fast", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let mut offset = 0usize;
                Point::decode_and_format(encoded.as_ptr(), &mut offset, &mut output, "");
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("point_decode_padded_runtime_spec", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let mut offset = 0usize;
                Point::decode_and_format(encoded.as_ptr(), &mut offset, &mut output, ":^12.5");
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("point_decode_padded_const_default", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let mut offset = 0usize;
                <Point as EncodeConstDefaultFormat>::decode_append_default_format::<
                    b' ',
                    { FORMAT_ALIGN_CENTER },
                    12,
                    5,
                    false,
                >(encoded.as_ptr(), &mut offset, &mut output);
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_enqueue_only_body_only(criterion: &mut Criterion) {
    configure_runtime(BODY_ONLY_HEADER_PATTERN, 1);

    emit_no_args();
    emit_u32(42);
    emit_fast_u32(42);
    emit_u32_str(42, "fast");
    emit_fast_u32_str(42, "fast");
    emit_u32_pair(42, 7);
    emit_hex(255);
    emit_padded_u32(42);
    emit_padded_hex(42);
    emit_signed_padded_hex(-42);
    emit_scientific_f64(std::f64::consts::PI);
    emit_point(Point { x: 7, y: 9 });
    emit_padded_point(Point { x: 7, y: 9 });
    poll(true).unwrap();

    let mut group = criterion.benchmark_group("enqueue_only_body_only");

    group.bench_function("log_no_args", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                let start = Instant::now();
                emit_no_args();
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("log_u32", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                let start = Instant::now();
                emit_u32(42);
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("log_fast_u32", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                let start = Instant::now();
                emit_fast_u32(42);
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("log_u32_str", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                let start = Instant::now();
                emit_u32_str(42, "fast");
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("log_u32_pair", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                let start = Instant::now();
                emit_u32_pair(42, 7);
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("log_fast_u32_str", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                let start = Instant::now();
                emit_fast_u32_str(42, "fast");
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("log_hex_spec", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                let start = Instant::now();
                emit_hex(255);
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.bench_function("log_custom_display", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                let start = Instant::now();
                emit_point(Point { x: 7, y: 9 });
                total += start.elapsed();
                poll(false).unwrap();
            }
            total
        });
    });

    group.finish();
}

fn bench_consume_only(criterion: &mut Criterion) {
    configure_runtime(BODY_ONLY_HEADER_PATTERN, u32::MAX);

    let mut group = criterion.benchmark_group("consume_only");

    group.bench_function("poll_body_only_u32_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_u32(index as u32);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_u32_pair_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_u32_pair(index as u32, index as u32 + 1);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_hex_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_hex(index as u32);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_no_args_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for _ in 0..BATCH_SIZE {
                    emit_no_args();
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_padded_u32_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_padded_u32(index as u32);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_padded_hex_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_padded_hex(index as u32);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_signed_padded_hex_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_signed_padded_hex(-(index as i32) - 1);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_scientific_f64_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_scientific_f64(index as f64 + 0.125);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_point_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_point(Point { x: index as i32, y: index as i32 + 1 });
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.bench_function("poll_body_only_padded_point_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_padded_point(Point { x: index as i32, y: index as i32 + 1 });
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    configure_runtime(LEVEL_THREAD_LOCATION_HEADER_PATTERN, u32::MAX);
    emit_u32(42);
    poll(true).unwrap();

    group.bench_function("poll_level_thread_location_header_u32_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_u32(index as u32);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    configure_runtime(STATIC_COMPLEX_HEADER_PATTERN, u32::MAX);
    emit_u32(42);
    poll(true).unwrap();

    group.bench_function("poll_dynamic_complex_header_u32_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_u32(index as u32);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    configure_runtime_static_complex_header(u32::MAX);
    emit_u32(42);
    poll(true).unwrap();

    group.bench_function("poll_static_complex_header_u32_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_u32(index as u32);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    configure_runtime(DEFAULT_HEADER_PATTERN, u32::MAX);
    emit_u32(42);
    poll(true).unwrap();

    group.bench_function("poll_default_header_u32_x256", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                for index in 0..BATCH_SIZE {
                    emit_u32(index as u32);
                }
                let start = Instant::now();
                poll(false).unwrap();
                total += start.elapsed();
                poll(true).unwrap();
            }
            total
        });
    });

    group.finish();
}

fn bench_end_to_end(criterion: &mut Criterion) {
    configure_runtime(BODY_ONLY_HEADER_PATTERN, 1);
    emit_u32(42);
    poll(true).unwrap();

    let mut group = criterion.benchmark_group("end_to_end");

    group.bench_function("body_only_u32", |bencher| {
        bencher.iter(|| {
            emit_u32(42);
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_u32_pair", |bencher| {
        bencher.iter(|| {
            emit_u32_pair(42, 7);
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_no_args", |bencher| {
        bencher.iter(|| {
            emit_no_args();
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_hex", |bencher| {
        bencher.iter(|| {
            emit_hex(255);
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_padded_u32", |bencher| {
        bencher.iter(|| {
            emit_padded_u32(42);
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_padded_hex", |bencher| {
        bencher.iter(|| {
            emit_padded_hex(42);
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_signed_padded_hex", |bencher| {
        bencher.iter(|| {
            emit_signed_padded_hex(-42);
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_scientific_f64", |bencher| {
        bencher.iter(|| {
            emit_scientific_f64(std::f64::consts::PI);
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_point", |bencher| {
        bencher.iter(|| {
            emit_point(Point { x: 7, y: 9 });
            poll(false).unwrap();
        });
    });

    group.bench_function("body_only_padded_point", |bencher| {
        bencher.iter(|| {
            emit_padded_point(Point { x: 7, y: 9 });
            poll(false).unwrap();
        });
    });

    configure_runtime(LEVEL_THREAD_LOCATION_HEADER_PATTERN, 1);
    emit_u32(42);
    poll(true).unwrap();

    group.bench_function("level_thread_location_header_u32", |bencher| {
        bencher.iter(|| {
            emit_u32(42);
            poll(false).unwrap();
        });
    });

    configure_runtime(STATIC_COMPLEX_HEADER_PATTERN, 1);
    emit_u32(42);
    poll(true).unwrap();

    group.bench_function("dynamic_complex_header_u32", |bencher| {
        bencher.iter(|| {
            emit_u32(42);
            poll(false).unwrap();
        });
    });

    configure_runtime_static_complex_header(1);
    emit_u32(42);
    poll(true).unwrap();

    group.bench_function("static_complex_header_u32", |bencher| {
        bencher.iter(|| {
            emit_u32(42);
            poll(false).unwrap();
        });
    });

    configure_runtime(DEFAULT_HEADER_PATTERN, 1);
    emit_u32(42);
    poll(true).unwrap();

    group.bench_function("default_header_u32", |bencher| {
        bencher.iter(|| {
            emit_u32(42);
            poll(false).unwrap();
        });
    });

    group.bench_function("default_header_batch_u32_x256", |bencher| {
        bencher.iter(|| {
            for index in 0..BATCH_SIZE {
                emit_u32(index as u32);
            }
            poll(false).unwrap();
        });
    });

    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_millis(600))
        .sample_size(20);
    targets = bench_custom_display_component, bench_enqueue_only_body_only, bench_consume_only, bench_end_to_end
}
criterion_main!(benches);

#[path = "support/allocations.rs"]
mod allocations;
use std::{
    hint::black_box,
    io::{self, Write},
    mem::size_of,
    time::{Duration, Instant},
};

use allocations::assert_zero_alloc;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use zerolog_core::{
    Encode, INFO, OFF, PAYLOAD_ARGS_OFFSET, PAYLOAD_HEADER_SIZE, PAYLOAD_INFO_OFFSET,
    PAYLOAD_TIMESTAMP_OFFSET, StaticInfo, TimestampCounter, append_header_nanoseconds,
    append_header_time, flush_on,
    header::{
        DEFAULT_HEADER_PATTERN, DefaultHeader, HeaderDateCache,
        LEVEL_THREAD_LOCATION_HEADER_PATTERN, LevelThreadLocationHeader, StaticHeaderWriteFn,
        TokenizedHeader,
    },
    number_format,
    output::OutputBuffer,
    poll,
    poller::ReadyMessageHeap,
    set_flush_buffer_size, set_flush_delay, set_header_pattern, set_level, set_output_writer,
    spsc_queue::SpscVarQueue,
    stop_polling_thread,
    thread_buffer::{ThreadBuffer, alloc_message, preallocate},
};

const BODY_ONLY_HEADER_PATTERN: &str = "";
const STATIC_COMPLEX_HEADER_PATTERN: &str = "{thread}|{level}|{location}|{date} {time}.{ns}| ";
const LOGGER_MESSAGE_COUNT: usize = 64;
const HEAP_BUFFER_COUNT: usize = 4;
const HEAP_MESSAGES_PER_BUFFER: usize = 32;
const STATIC_BODY_MESSAGE: &[u8] = b"plain message";

fn write_static_complex_header(
    date_cache: &mut HeaderDateCache,
    output: &mut Vec<u8>,
    timestamp_nanoseconds: i64,
    level: zerolog_core::Level,
    thread_name: &[u8],
    location: &str,
    _span_id: u64,
) {
    output.reserve(37 + thread_name.len() + location.len());
    output.extend_from_slice(thread_name);
    output.push(b'|');
    output.extend_from_slice(level.as_bytes());
    output.push(b'|');
    output.extend_from_slice(location.as_bytes());
    output.push(b'|');
    date_cache.append_date(output, timestamp_nanoseconds);
    output.push(b' ');
    append_header_time(output, timestamp_nanoseconds);
    output.push(b'.');
    append_header_nanoseconds(output, timestamp_nanoseconds);
    output.extend_from_slice(b"| ");
}

unsafe fn decode_static_body(_buffer: *const u8, output: &mut Vec<u8>) {
    output.extend_from_slice(STATIC_BODY_MESSAGE);
}

struct SinkWriter;

impl Write for SinkWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct HeapFixture {
    buffers: Vec<*mut ThreadBuffer>,
}

impl Drop for HeapFixture {
    fn drop(&mut self) {
        for &buffer in &self.buffers {
            unsafe {
                ThreadBuffer::free_on_heap(buffer);
            }
        }
    }
}

fn bench_header_output(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core/header_output");
    let timestamp_nanoseconds = 1_744_000_123_456_789_123i64;

    group.bench_function("parse_default_pattern", |bencher| {
        bencher.iter(|| black_box(TokenizedHeader::parse_pattern(DEFAULT_HEADER_PATTERN).unwrap()));
    });

    let mut preflight_header = DefaultHeader::new();
    let mut preflight_output = Vec::with_capacity(128);
    assert_zero_alloc(|| {
        preflight_output.clear();
        preflight_header.write_header(
            &mut preflight_output,
            timestamp_nanoseconds,
            INFO,
            b"bench-thread",
            "bench",
            0,
        );
        black_box(&preflight_output);
    });
    let mut header = TokenizedHeader::default_pattern();
    group.bench_function("write_default_header", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(128),
            |mut output| {
                header.write_header(
                    &mut output,
                    black_box(timestamp_nanoseconds),
                    INFO,
                    b"bench-thread",
                    "core/benches/core_component_bench.rs:1",
                    0u64,
                );
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    let mut specialized_header = DefaultHeader::new();
    group.bench_function("write_default_header_specialized", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(128),
            |mut output| {
                specialized_header.write_header(
                    &mut output,
                    black_box(timestamp_nanoseconds),
                    INFO,
                    b"bench-thread",
                    "core/benches/core_component_bench.rs:1",
                    0u64,
                );
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("parse_level_thread_location_pattern", |bencher| {
        bencher.iter(|| {
            black_box(TokenizedHeader::parse_pattern(LEVEL_THREAD_LOCATION_HEADER_PATTERN).unwrap())
        });
    });

    let mut concise_tokenized_header =
        TokenizedHeader::parse_pattern(LEVEL_THREAD_LOCATION_HEADER_PATTERN).unwrap();
    group.bench_function("write_level_thread_location_header", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(96),
            |mut output| {
                concise_tokenized_header.write_header(
                    &mut output,
                    black_box(timestamp_nanoseconds),
                    INFO,
                    b"bench-thread",
                    "core/benches/core_component_bench.rs:1",
                    0u64,
                );
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    let mut concise_specialized_header = LevelThreadLocationHeader::new();
    group.bench_function("write_level_thread_location_header_specialized", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(96),
            |mut output| {
                concise_specialized_header.write_header(
                    &mut output,
                    black_box(timestamp_nanoseconds),
                    INFO,
                    b"bench-thread",
                    "core/benches/core_component_bench.rs:1",
                    0u64,
                );
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    let mut dynamic_complex_header =
        TokenizedHeader::parse_pattern(STATIC_COMPLEX_HEADER_PATTERN).unwrap();
    group.bench_function("write_dynamic_complex_header", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(160),
            |mut output| {
                dynamic_complex_header.write_header(
                    &mut output,
                    black_box(timestamp_nanoseconds),
                    INFO,
                    b"bench-thread",
                    "core/benches/core_component_bench.rs:1",
                    0u64,
                );
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    let static_complex_writer: StaticHeaderWriteFn = write_static_complex_header;
    group.bench_function("write_static_complex_header", |bencher| {
        bencher.iter_batched(
            || (HeaderDateCache::new(), Vec::with_capacity(160)),
            |(mut date_cache, mut output)| {
                static_complex_writer(
                    &mut date_cache,
                    &mut output,
                    black_box(timestamp_nanoseconds),
                    INFO,
                    b"bench-thread",
                    "core/benches/core_component_bench.rs:1",
                    0u64,
                );
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("output_write_record_default_header", |bencher| {
        bencher.iter_batched(
            || {
                let mut output = OutputBuffer::new();
                output.set_writer(Box::new(SinkWriter));
                output
            },
            |mut output| {
                output
                    .write_record(
                        timestamp_nanoseconds,
                        INFO,
                        b"bench-thread",
                        "core/benches/core_component_bench.rs:1",
                        0u64,
                        |buffer| number_format::append_u32(buffer, 42),
                    )
                    .unwrap();
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("output_write_record_static_body", |bencher| {
        bencher.iter_batched(
            || {
                let mut output = OutputBuffer::new();
                output.set_writer(Box::new(SinkWriter));
                output.set_header_pattern(BODY_ONLY_HEADER_PATTERN).unwrap();
                output
            },
            |mut output| {
                output
                    .write_record(black_box(0), INFO, b"bench", "bench", 0, |buffer| unsafe {
                        decode_static_body(std::ptr::null(), buffer);
                    })
                    .unwrap();
                black_box(output.buffered_len());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("output_write_record_dynamic_complex_header", |bencher| {
        bencher.iter_batched(
            || {
                let mut output = OutputBuffer::new();
                output.set_writer(Box::new(SinkWriter));
                output.set_header_pattern(STATIC_COMPLEX_HEADER_PATTERN).unwrap();
                output
            },
            |mut output| {
                output
                    .write_record(
                        timestamp_nanoseconds,
                        INFO,
                        b"bench-thread",
                        "core/benches/core_component_bench.rs:1",
                        0u64,
                        |buffer| number_format::append_u32(buffer, 42),
                    )
                    .unwrap();
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("output_write_record_static_complex_header", |bencher| {
        bencher.iter_batched(
            || {
                let mut output = OutputBuffer::new();
                output.set_writer(Box::new(SinkWriter));
                output.set_static_header(write_static_complex_header);
                output
            },
            |mut output| {
                output
                    .write_record(
                        timestamp_nanoseconds,
                        INFO,
                        b"bench-thread",
                        "core/benches/core_component_bench.rs:1",
                        0u64,
                        |buffer| number_format::append_u32(buffer, 42),
                    )
                    .unwrap();
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("output_write_record_level_thread_location_header", |bencher| {
        bencher.iter_batched(
            || {
                let mut output = OutputBuffer::new();
                output.set_writer(Box::new(SinkWriter));
                output.set_header_pattern(LEVEL_THREAD_LOCATION_HEADER_PATTERN).unwrap();
                output
            },
            |mut output| {
                output
                    .write_record(
                        timestamp_nanoseconds,
                        INFO,
                        b"bench-thread",
                        "core/benches/core_component_bench.rs:1",
                        0u64,
                        |buffer| number_format::append_u32(buffer, 42),
                    )
                    .unwrap();
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("output_flush_64_records", |bencher| {
        bencher.iter_batched(
            || {
                let mut output = OutputBuffer::new();
                output.set_writer(Box::new(SinkWriter));
                for index in 0..64u32 {
                    output
                        .write_record(
                            timestamp_nanoseconds + index as i64,
                            INFO,
                            b"bench-thread",
                            "core/benches/core_component_bench.rs:1",
                            0u64,
                            |buffer| number_format::append_u32(buffer, index),
                        )
                        .unwrap();
                }
                output
            },
            |mut output| {
                output.flush().unwrap();
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_timestamp_poller_logger(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core/timestamp_poller_logger");

    let mut counter = TimestampCounter::new();
    counter.init_default();
    let sample_timestamp = TimestampCounter::read_timestamp_counter();

    group.bench_function("timestamp_read_counter", |bencher| {
        bencher.iter(|| black_box(TimestampCounter::read_timestamp_counter()));
    });

    group.bench_function("timestamp_to_nanoseconds", |bencher| {
        bencher.iter(|| black_box(counter.timestamp_to_nanoseconds(sample_timestamp)));
    });

    group.bench_function("timestamp_read_nanoseconds", |bencher| {
        bencher.iter(|| black_box(counter.read_nanoseconds()));
    });

    group.bench_function("poller_heap_build_4x32", |bencher| {
        bencher.iter_batched(
            || create_heap_fixture(HEAP_BUFFER_COUNT, HEAP_MESSAGES_PER_BUFFER),
            |fixture| {
                // SAFETY: fixture owns live buffers and this benchmark is their only consumer.
                let mut heap = unsafe { ReadyMessageHeap::from_buffers(&fixture.buffers) };
                black_box(heap.pop());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("poller_heap_drain_4x32", |bencher| {
        bencher.iter_batched(
            || create_heap_fixture(HEAP_BUFFER_COUNT, HEAP_MESSAGES_PER_BUFFER),
            |fixture| {
                // SAFETY: fixture owns live buffers and this benchmark is their only consumer.
                let mut heap = unsafe { ReadyMessageHeap::from_buffers(&fixture.buffers) };
                while let Some(node) = heap.pop() {
                    unsafe {
                        let queue = &raw mut (*node.thread_buffer()).queue;
                        SpscVarQueue::consumer_pop(queue);
                    }
                    // SAFETY: fixture retains the buffer, with this loop as sole consumer.
                    unsafe { heap.refresh_buffer(node.thread_buffer()) };
                }
            },
            BatchSize::SmallInput,
        );
    });

    configure_logger_for_bench(BODY_ONLY_HEADER_PATTERN, u32::MAX);
    preallocate();
    poll(true).unwrap();

    group.bench_function("logger_poll_body_only_u32_x64", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                fill_logger_queue(&LOGGER_STATIC_INFO, LOGGER_MESSAGE_COUNT);
                let start = Instant::now();
                poll(true).unwrap();
                total += start.elapsed();
            }
            total
        });
    });

    group.bench_function("logger_poll_body_only_static_message_x64", |bencher| {
        bencher.iter_custom(|iterations| {
            let mut total = Duration::ZERO;
            for _ in 0..iterations {
                fill_logger_queue_no_args(&LOGGER_STATIC_BODY_INFO, LOGGER_MESSAGE_COUNT);
                let start = Instant::now();
                poll(true).unwrap();
                total += start.elapsed();
            }
            total
        });
    });

    group.finish();
}

fn configure_logger_for_bench(header_pattern: &str, flush_buffer_size: u32) {
    let _ = stop_polling_thread();
    set_output_writer(SinkWriter);
    set_level(INFO);
    set_flush_delay(3_000_000_000);
    set_flush_buffer_size(flush_buffer_size);
    flush_on(OFF);
    set_header_pattern(header_pattern).unwrap();
}

fn create_heap_fixture(buffer_count: usize, messages_per_buffer: usize) -> HeapFixture {
    let mut buffers = Vec::with_capacity(buffer_count);
    for buffer_index in 0..buffer_count {
        let buffer = ThreadBuffer::new_on_heap();
        unsafe {
            for message_index in 0..messages_per_buffer {
                let timestamp = (message_index * buffer_count + buffer_index) as i64;
                push_heap_timestamp_message(buffer, timestamp);
            }
        }
        buffers.push(buffer);
    }
    HeapFixture { buffers }
}

unsafe fn push_heap_timestamp_message(buffer: *mut ThreadBuffer, timestamp: i64) {
    let queue = unsafe { &raw mut (*buffer).queue };
    let header =
        unsafe { SpscVarQueue::producer_alloc(queue, PAYLOAD_HEADER_SIZE as u32).unwrap() };
    unsafe {
        let payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(header);
        payload.add(PAYLOAD_TIMESTAMP_OFFSET).cast::<i64>().write_unaligned(timestamp);
        (*header).push(PAYLOAD_HEADER_SIZE as u32);
    }
}

static LOGGER_STATIC_INFO: StaticInfo = StaticInfo {
    level: INFO,
    location: "bench",
    format_string: "value={}",
    decode: decode_u32_body,
};

static LOGGER_STATIC_BODY_INFO: StaticInfo = StaticInfo {
    level: INFO,
    location: "bench",
    format_string: "plain message",
    decode: decode_static_body,
};

unsafe fn decode_u32_body(buffer: *const u8, output: &mut Vec<u8>) {
    let mut offset = 0usize;
    unsafe {
        <u32 as Encode>::decode_and_format(buffer, &mut offset, output, "");
    }
}

fn fill_logger_queue(info: &'static StaticInfo, message_count: usize) {
    let base_timestamp = TimestampCounter::read_timestamp_counter();
    for index in 0..message_count as u32 {
        let header = alloc_message((PAYLOAD_HEADER_SIZE + size_of::<u32>()) as u32).unwrap();
        unsafe {
            let payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(header);
            payload
                .add(PAYLOAD_INFO_OFFSET)
                .cast::<*const StaticInfo>()
                .write_unaligned(info as *const StaticInfo);
            payload
                .add(PAYLOAD_TIMESTAMP_OFFSET)
                .cast::<i64>()
                .write_unaligned(base_timestamp + index as i64);
            payload.add(PAYLOAD_ARGS_OFFSET).cast::<u32>().write_unaligned(index);
            (*header).push((PAYLOAD_HEADER_SIZE + size_of::<u32>()) as u32);
        }
    }
}

fn fill_logger_queue_no_args(info: &'static StaticInfo, message_count: usize) {
    let base_timestamp = TimestampCounter::read_timestamp_counter();
    for index in 0..message_count as i64 {
        let header = alloc_message(PAYLOAD_HEADER_SIZE as u32).unwrap();
        unsafe {
            let payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(header);
            payload
                .add(PAYLOAD_INFO_OFFSET)
                .cast::<*const StaticInfo>()
                .write_unaligned(info as *const StaticInfo);
            payload
                .add(PAYLOAD_TIMESTAMP_OFFSET)
                .cast::<i64>()
                .write_unaligned(base_timestamp + index);
            (*header).push(PAYLOAD_HEADER_SIZE as u32);
        }
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(std::time::Duration::from_millis(200))
        .measurement_time(std::time::Duration::from_millis(600)).sample_size(20);
    targets = bench_header_output, bench_timestamp_poller_logger
}
criterion_main!(benches);

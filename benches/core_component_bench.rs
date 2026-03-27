use std::{
    hint::black_box,
    io::{self, Write},
    mem::size_of,
    time::{Duration, Instant},
};

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ferrilog_core::{
    Encode, EncodeConstDefaultFormat, EncodeConstFloatFormat, EncodeConstLowerHexFormat,
    EncodeFast, EncodeLowerHex, FORMAT_ALIGN_CENTER, FORMAT_ALIGN_NONE, FORMAT_ALIGN_RIGHT,
    FORMAT_PRECISION_NONE, INFO, OFF, PAYLOAD_ARGS_OFFSET, PAYLOAD_HEADER_SIZE,
    PAYLOAD_INFO_OFFSET, PAYLOAD_TIMESTAMP_OFFSET, StaticInfo, TimestampCounter,
    append_display_with_spec, append_header_nanoseconds, append_header_time, flush_on,
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
    thread_buffer::{ThreadBuffer, alloc_message, get_thread_buffer, preallocate},
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
    level: ferrilog_core::Level,
    thread_name: &[u8],
    location: &str,
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

#[derive(Clone, Copy)]
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

fn bench_number_format(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core/number_format");

    group.bench_function("count_digits_u64", |bencher| {
        bencher.iter(|| black_box(number_format::count_digits_u64(9_876_543_210_123_456_789)));
    });

    group.bench_function("format_fixed_width_9", |bencher| {
        bencher.iter(|| {
            let mut buffer = [0u8; 9];
            number_format::format_fixed_width::<9>(&mut buffer, black_box(123_456_789));
            black_box(buffer);
        });
    });

    group.bench_function("format_u32", |bencher| {
        bencher.iter(|| {
            let mut buffer = [0u8; 10];
            let written = number_format::format_u32(&mut buffer, black_box(4_294_967_295));
            black_box((&buffer, written));
        });
    });

    group.bench_function("format_i64", |bencher| {
        bencher.iter(|| {
            let mut buffer = [0u8; 21];
            let written =
                number_format::format_i64(&mut buffer, black_box(-9_223_372_036_854_775_808));
            black_box((&buffer, written));
        });
    });

    group.bench_function("append_u64", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| {
                number_format::append_u64(&mut output, black_box(18_446_744_073_709_551_615));
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

fn bench_encode(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core/encode");

    let u32_value = 123_456_789u32;
    let mut encoded_u32 = [0u8; 16];
    let mut encoded_u32_len = 0usize;
    unsafe {
        <u32 as Encode>::encode(&u32_value, encoded_u32.as_mut_ptr(), &mut encoded_u32_len);
    }

    let u32_pair = (123_456_789u32, 987_654_321u32);
    let mut encoded_u32_pair = [0u8; 16];
    let mut encoded_u32_pair_len = 0usize;
    unsafe {
        <u32 as Encode>::encode(
            &u32_pair.0,
            encoded_u32_pair.as_mut_ptr(),
            &mut encoded_u32_pair_len,
        );
        <u32 as Encode>::encode(
            &u32_pair.1,
            encoded_u32_pair.as_mut_ptr(),
            &mut encoded_u32_pair_len,
        );
    }

    let i32_value = -42i32;
    let mut encoded_i32 = [0u8; 16];
    let mut encoded_i32_len = 0usize;
    unsafe {
        <i32 as Encode>::encode(&i32_value, encoded_i32.as_mut_ptr(), &mut encoded_i32_len);
    }

    let str_value = "ferrilog-benchmark";
    let mut encoded_str = [0u8; 64];
    let mut encoded_str_len = 0usize;
    unsafe {
        <&str as Encode>::encode(&str_value, encoded_str.as_mut_ptr(), &mut encoded_str_len);
    }

    let f64_value = 1234.5678_f64;
    let mut encoded_f64 = [0u8; 16];
    let mut encoded_f64_len = 0usize;
    unsafe {
        <f64 as Encode>::encode(&f64_value, encoded_f64.as_mut_ptr(), &mut encoded_f64_len);
    }

    group.bench_function("u32_encode_fast", |bencher| {
        bencher.iter_batched(
            || ([0u8; 16], 0usize),
            |(mut buffer, mut offset)| unsafe {
                <u32 as EncodeFast>::encode_fast(
                    black_box(&u32_value),
                    buffer.as_mut_ptr(),
                    &mut offset,
                );
                black_box((&buffer, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_decode_default", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <u32 as Encode>::decode_and_format(
                    encoded_u32.as_ptr(),
                    &mut offset,
                    &mut output,
                    "",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_pair_decode_fast_sequential", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let mut offset = 0usize;
                output.extend_from_slice(b"a=");
                <u32 as EncodeFast>::decode_and_append_fast(
                    encoded_u32_pair.as_ptr(),
                    &mut offset,
                    &mut output,
                );
                output.extend_from_slice(b" b=");
                <u32 as EncodeFast>::decode_and_append_fast(
                    encoded_u32_pair.as_ptr(),
                    &mut offset,
                    &mut output,
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_pair_decode_fast_fixed_offsets", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                output.extend_from_slice(b"a=");
                <u32 as EncodeFast>::decode_and_append_fast_at_offset(
                    encoded_u32_pair.as_ptr(),
                    0,
                    &mut output,
                );
                output.extend_from_slice(b" b=");
                <u32 as EncodeFast>::decode_and_append_fast_at_offset(
                    encoded_u32_pair.as_ptr(),
                    size_of::<u32>(),
                    &mut output,
                );
                black_box(&output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_pair_decode_default_sequential", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let mut offset = 0usize;
                output.extend_from_slice(b"a=");
                <u32 as EncodeConstDefaultFormat>::decode_append_default_format::<
                    b' ',
                    { FORMAT_ALIGN_NONE },
                    0,
                    { FORMAT_PRECISION_NONE },
                    false,
                >(encoded_u32_pair.as_ptr(), &mut offset, &mut output);
                output.extend_from_slice(b" b=");
                <u32 as EncodeConstDefaultFormat>::decode_append_default_format::<
                    b' ',
                    { FORMAT_ALIGN_NONE },
                    0,
                    { FORMAT_PRECISION_NONE },
                    false,
                >(encoded_u32_pair.as_ptr(), &mut offset, &mut output);
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_pair_decode_default_fixed_offsets", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                output.extend_from_slice(b"a=");
                <u32 as EncodeConstDefaultFormat>::decode_append_default_format_at_offset::<
                    b' ',
                    { FORMAT_ALIGN_NONE },
                    0,
                    { FORMAT_PRECISION_NONE },
                    false,
                >(encoded_u32_pair.as_ptr(), 0, &mut output);
                output.extend_from_slice(b" b=");
                <u32 as EncodeConstDefaultFormat>::decode_append_default_format_at_offset::<
                    b' ',
                    { FORMAT_ALIGN_NONE },
                    0,
                    { FORMAT_PRECISION_NONE },
                    false,
                >(encoded_u32_pair.as_ptr(), size_of::<u32>(), &mut output);
                black_box(&output);
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_decode_hex_spec", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <u32 as Encode>::decode_and_format(
                    encoded_u32.as_ptr(),
                    &mut offset,
                    &mut output,
                    ":#x",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_decode_hex_specialized", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <u32 as EncodeLowerHex>::decode_append_lower_hex::<true>(
                    encoded_u32.as_ptr(),
                    &mut offset,
                    &mut output,
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_decode_padded_hex_spec", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <u32 as Encode>::decode_and_format(
                    encoded_u32.as_ptr(),
                    &mut offset,
                    &mut output,
                    ":#010x",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_decode_padded_hex_const_specialized", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <u32 as EncodeConstLowerHexFormat>::decode_append_lower_hex_format::<
                    b'0',
                    { FORMAT_ALIGN_NONE },
                    10,
                    true,
                    true,
                >(encoded_u32.as_ptr(), &mut offset, &mut output);
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("i32_decode_padded_hex_spec", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <i32 as Encode>::decode_and_format(
                    encoded_i32.as_ptr(),
                    &mut offset,
                    &mut output,
                    ":#012x",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("i32_decode_padded_hex_const_specialized", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <i32 as EncodeConstLowerHexFormat>::decode_append_lower_hex_format::<
                    b'0',
                    { FORMAT_ALIGN_NONE },
                    12,
                    true,
                    true,
                >(encoded_i32.as_ptr(), &mut offset, &mut output);
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_decode_width_spec", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <u32 as Encode>::decode_and_format(
                    encoded_u32.as_ptr(),
                    &mut offset,
                    &mut output,
                    ":>12",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("u32_decode_width_const_default", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(16),
            |mut output| unsafe {
                let mut offset = 0usize;
                <u32 as EncodeConstDefaultFormat>::decode_append_default_format::<
                    b' ',
                    { FORMAT_ALIGN_RIGHT },
                    12,
                    { FORMAT_PRECISION_NONE },
                    false,
                >(encoded_u32.as_ptr(), &mut offset, &mut output);
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("str_encode_fast", |bencher| {
        bencher.iter_batched(
            || ([0u8; 64], 0usize),
            |(mut buffer, mut offset)| unsafe {
                <&str as EncodeFast>::encode_fast(
                    black_box(&str_value),
                    buffer.as_mut_ptr(),
                    &mut offset,
                );
                black_box((&buffer, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("str_decode_default", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(64),
            |mut output| unsafe {
                let mut offset = 0usize;
                <&str as Encode>::decode_and_format(
                    encoded_str.as_ptr(),
                    &mut offset,
                    &mut output,
                    "",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("str_decode_width_precision_spec", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(64),
            |mut output| unsafe {
                let mut offset = 0usize;
                <&str as Encode>::decode_and_format(
                    encoded_str.as_ptr(),
                    &mut offset,
                    &mut output,
                    ":^24.8",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("str_decode_width_precision_const_default", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(64),
            |mut output| unsafe {
                let mut offset = 0usize;
                <&str as EncodeConstDefaultFormat>::decode_append_default_format::<
                    b' ',
                    { FORMAT_ALIGN_CENTER },
                    24,
                    8,
                    false,
                >(encoded_str.as_ptr(), &mut offset, &mut output);
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("f64_decode_default", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let mut offset = 0usize;
                <f64 as Encode>::decode_and_format(
                    encoded_f64.as_ptr(),
                    &mut offset,
                    &mut output,
                    "",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("f64_decode_scientific_spec", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let mut offset = 0usize;
                <f64 as Encode>::decode_and_format(
                    encoded_f64.as_ptr(),
                    &mut offset,
                    &mut output,
                    ":>14.3e",
                );
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("f64_decode_scientific_const_format", |bencher| {
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| unsafe {
                let mut offset = 0usize;
                <f64 as EncodeConstFloatFormat>::decode_append_float_format::<
                    b' ',
                    { FORMAT_ALIGN_RIGHT },
                    14,
                    3,
                    false,
                    b'e',
                >(encoded_f64.as_ptr(), &mut offset, &mut output);
                black_box((&output, offset));
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("display_fallback_with_width", |bencher| {
        let point = Point { x: 7, y: 9 };
        bencher.iter_batched(
            || Vec::with_capacity(32),
            |mut output| {
                append_display_with_spec(&mut output, black_box(point), ":>16");
                black_box(output);
            },
            BatchSize::SmallInput,
        );
    });

    black_box((encoded_u32_len, encoded_str_len, encoded_f64_len));
    group.finish();
}

fn bench_header_output(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core/header_output");
    let timestamp_nanoseconds = 1_744_000_123_456_789_123i64;

    group.bench_function("parse_default_pattern", |bencher| {
        bencher.iter(|| black_box(TokenizedHeader::parse_pattern(DEFAULT_HEADER_PATTERN).unwrap()));
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
                    .write_record(black_box(0), INFO, b"bench", "bench", |buffer| unsafe {
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

fn bench_queue_thread_buffer(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core/queue_thread_buffer");

    let queue_buffer = ThreadBuffer::new_on_heap();
    let queue = unsafe { &raw mut (*queue_buffer).queue };
    let mut message_seed = 0u64;

    group.bench_function("spsc_alloc_push_pop_16b", |bencher| {
        bencher.iter(|| unsafe {
            let header = SpscVarQueue::producer_alloc(queue, 16).unwrap();
            let payload = (*header).payload_pointer();
            payload.cast::<u64>().write_unaligned(message_seed);
            payload.add(8).cast::<u64>().write_unaligned(message_seed ^ 0xDEADBEEF);
            (*header).push(16);

            let front = SpscVarQueue::consumer_front(queue).unwrap();
            let front_payload = (*front).payload_pointer();
            black_box(front_payload.cast::<u64>().read_unaligned());
            SpscVarQueue::consumer_pop(queue);
            message_seed = message_seed.wrapping_add(1);
        });
    });

    unsafe {
        push_queue_pair_message(queue_buffer, 99);
    }
    group.bench_function("spsc_front_pop_16b", |bencher| {
        bencher.iter(|| unsafe {
            let front = SpscVarQueue::consumer_front(queue).unwrap();
            black_box((*front).payload_pointer());
            SpscVarQueue::consumer_pop(queue);
            push_queue_pair_message(queue_buffer, 99);
        });
    });

    preallocate();
    let thread_buffer = get_thread_buffer();
    let thread_queue = unsafe { &raw mut (*thread_buffer).queue };
    while unsafe { SpscVarQueue::consumer_front(thread_queue).is_some() } {
        unsafe {
            SpscVarQueue::consumer_pop(thread_queue);
        }
    }

    group.bench_function("thread_buffer_alloc_message_16b", |bencher| {
        bencher.iter(|| unsafe {
            let header = alloc_message(16).unwrap();
            let payload = (*header).payload_pointer();
            payload.cast::<u64>().write_unaligned(0xABCD);
            payload.add(8).cast::<u64>().write_unaligned(0x1234);
            (*header).push(16);

            let front = SpscVarQueue::consumer_front(thread_queue).unwrap();
            let front_payload = (*front).payload_pointer();
            black_box(front_payload.cast::<u64>().read_unaligned());
            SpscVarQueue::consumer_pop(thread_queue);
        });
    });

    group.finish();

    unsafe {
        ThreadBuffer::free_on_heap(queue_buffer);
    }
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
                let mut heap = ReadyMessageHeap::from_buffers(&fixture.buffers);
                black_box(heap.pop());
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("poller_heap_drain_4x32", |bencher| {
        bencher.iter_batched(
            || create_heap_fixture(HEAP_BUFFER_COUNT, HEAP_MESSAGES_PER_BUFFER),
            |fixture| {
                let mut heap = ReadyMessageHeap::from_buffers(&fixture.buffers);
                while let Some(node) = heap.pop() {
                    unsafe {
                        let queue = &raw mut (*node.thread_buffer()).queue;
                        SpscVarQueue::consumer_pop(queue);
                    }
                    heap.refresh_buffer(node.thread_buffer());
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

unsafe fn push_queue_pair_message(buffer: *mut ThreadBuffer, seed: u64) {
    let queue = unsafe { &raw mut (*buffer).queue };
    let header = unsafe { SpscVarQueue::producer_alloc(queue, 16).unwrap() };
    unsafe {
        let payload = (*header).payload_pointer();
        payload.cast::<u64>().write_unaligned(seed);
        payload.add(8).cast::<u64>().write_unaligned(seed ^ 0xDEADBEEF);
        (*header).push(16);
    }
}

unsafe fn push_heap_timestamp_message(buffer: *mut ThreadBuffer, timestamp: i64) {
    let queue = unsafe { &raw mut (*buffer).queue };
    let header =
        unsafe { SpscVarQueue::producer_alloc(queue, PAYLOAD_HEADER_SIZE as u32).unwrap() };
    unsafe {
        let payload = (*header).payload_pointer();
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
            let payload = (*header).payload_pointer();
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
            let payload = (*header).payload_pointer();
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
    config = Criterion::default()
        .warm_up_time(std::time::Duration::from_millis(200))
        .measurement_time(std::time::Duration::from_millis(600))
        .sample_size(20);
    targets =
        bench_number_format,
        bench_encode,
        bench_header_output,
        bench_queue_thread_buffer,
        bench_timestamp_poller_logger
}
criterion_main!(benches);

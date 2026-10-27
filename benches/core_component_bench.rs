#[path = "support/allocations.rs"]
mod allocations;
use std::{hint::black_box, mem::size_of};

use allocations::assert_zero_alloc;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use zerolog_core::{
    Encode, EncodeConstDefaultFormat, EncodeConstFloatFormat, EncodeConstLowerHexFormat,
    EncodeFast, EncodeLowerHex, FORMAT_ALIGN_CENTER, FORMAT_ALIGN_NONE, FORMAT_ALIGN_RIGHT,
    FORMAT_PRECISION_NONE, append_display_with_spec, number_format,
    spsc_queue::SpscVarQueue,
    thread_buffer::{ThreadBuffer, alloc_message, get_thread_buffer, preallocate},
};
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

fn bench_number_format(criterion: &mut Criterion) {
    assert_zero_alloc(|| {
        let mut buffer = [0_u8; 21];
        black_box(number_format::format_u64(&mut buffer, black_box(u64::MAX)));
        let mut fixed = [0_u8; 9];
        number_format::format_fixed_width(&mut fixed, black_box(123_456_789));
        black_box(fixed);
    });
    let mut group = criterion.benchmark_group("core/number_format");

    group.bench_function("count_digits_u64", |bencher| {
        bencher.iter(|| {
            black_box(number_format::count_digits_u64(black_box(9_876_543_210_123_456_789)))
        });
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
    let mut bytes = [0_u8; 128];
    let mut output = Vec::with_capacity(128);
    let text = "allocation-free";
    assert_zero_alloc(|| {
        let mut offset = 0;
        // SAFETY: the stack buffer holds the complete integer and length-prefixed string.
        unsafe {
            <u32 as Encode>::encode(&42, bytes.as_mut_ptr(), &mut offset);
            <&str as Encode>::encode(&text, bytes.as_mut_ptr(), &mut offset);
            assert_eq!(offset, 8 + text.len());
            output.clear();
            offset = 0;
            <u32 as EncodeFast>::decode_and_append_fast(bytes.as_ptr(), &mut offset, &mut output);
            <&str as EncodeFast>::decode_and_append_fast(bytes.as_ptr(), &mut offset, &mut output);
        }
        black_box(&output);
    });
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

    let str_value = "zerolog-benchmark";
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

fn bench_queue_thread_buffer(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("core/queue_thread_buffer");

    let queue_buffer = ThreadBuffer::new_on_heap();
    let queue = unsafe { &raw mut (*queue_buffer).queue };
    let mut message_seed = 0u64;
    assert_zero_alloc(|| {
        // SAFETY: this benchmark owns both queue ends and publishes initialized bytes.
        unsafe {
            let header = SpscVarQueue::producer_alloc(queue, 16).unwrap();
            let payload = zerolog_core::spsc_queue::MessageHeader::payload_pointer_raw(header);
            payload.write_bytes(0, 16);
            (*header).push(16);
            assert!(SpscVarQueue::consumer_front(queue).is_some());
            SpscVarQueue::consumer_pop(queue);
        }
    });

    group.bench_function("spsc_alloc_push_pop_16b", |bencher| {
        bencher.iter(|| unsafe {
            let header = SpscVarQueue::producer_alloc(queue, 16).unwrap();
            let payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(header);
            payload.cast::<u64>().write_unaligned(message_seed);
            payload.add(8).cast::<u64>().write_unaligned(message_seed ^ 0xDEADBEEF);
            (*header).push(16);

            let front = SpscVarQueue::consumer_front(queue).unwrap();
            let front_payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(front);
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
            black_box(zerolog::spsc_queue::MessageHeader::payload_pointer_raw(front));
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

    assert_zero_alloc(|| {
        // SAFETY: this thread owns the initialized TLS producer and the sole consumer.
        unsafe {
            let header = alloc_message(16).unwrap();
            zerolog_core::spsc_queue::MessageHeader::payload_pointer_raw(header).write_bytes(0, 16);
            (*header).push(16);
            assert!(SpscVarQueue::consumer_front(thread_queue).is_some());
            SpscVarQueue::consumer_pop(thread_queue);
        }
    });
    group.bench_function("thread_buffer_alloc_message_16b", |bencher| {
        bencher.iter(|| unsafe {
            let header = alloc_message(16).unwrap();
            let payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(header);
            payload.cast::<u64>().write_unaligned(0xABCD);
            payload.add(8).cast::<u64>().write_unaligned(0x1234);
            (*header).push(16);

            let front = SpscVarQueue::consumer_front(thread_queue).unwrap();
            let front_payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(front);
            black_box(front_payload.cast::<u64>().read_unaligned());
            SpscVarQueue::consumer_pop(thread_queue);
        });
    });

    group.finish();

    unsafe {
        ThreadBuffer::free_on_heap(queue_buffer);
    }
}

unsafe fn push_queue_pair_message(buffer: *mut ThreadBuffer, seed: u64) {
    let queue = unsafe { &raw mut (*buffer).queue };
    let header = unsafe { SpscVarQueue::producer_alloc(queue, 16).unwrap() };
    unsafe {
        let payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(header);
        payload.cast::<u64>().write_unaligned(seed);
        payload.add(8).cast::<u64>().write_unaligned(seed ^ 0xDEADBEEF);
        (*header).push(16);
    }
}

criterion_group! {
    name = benches;
    config = Criterion::default().warm_up_time(std::time::Duration::from_millis(200))
        .measurement_time(std::time::Duration::from_millis(600)).sample_size(20);
    targets = bench_number_format, bench_encode, bench_queue_thread_buffer
}
criterion_main!(benches);

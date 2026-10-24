use std::fmt::Write as _;

use super::{
    Encode, EncodeFast, FORMAT_ALIGN_NONE, FORMAT_ALIGN_RIGHT, FORMAT_PRECISION_NONE,
    format::{
        VecWriter, append_binary_u64_padded_const, append_const_float_body,
        append_display_with_spec, append_float_with_spec, append_i32_decimal_with_spec,
        append_i64_decimal_with_spec, append_integer_with_spec_cast,
        append_lower_hex_u64_padded_const, append_octal_u64_padded_const, append_padded_const,
        append_ryu_float, append_u32_decimal_with_spec, append_u64_decimal_with_spec,
        append_upper_hex_u64_padded_const,
    },
    traits::{
        EncodeBinary, EncodeConstBinaryFormat, EncodeConstDefaultFormat, EncodeConstFloatFormat,
        EncodeConstLowerHexFormat, EncodeConstOctalFormat, EncodeConstUpperHexFormat,
        EncodeLowerHex, EncodeOctal, EncodeUpperHex,
    },
};
use crate::number_format;

// --- Layer 1: Fast Primitive Path ---

macro_rules! impl_integer_encode {
    ($type:ty, $append_function:path, $cast:expr, $append_decimal:expr) => {
        impl Encode for $type {
            const ENCODED_SIZE_HINT: usize = std::mem::size_of::<$type>();

            #[inline(always)]
            fn encoded_size(&self) -> usize {
                std::mem::size_of::<$type>()
            }

            #[inline(always)]
            unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
                unsafe {
                    buffer.add(*offset).cast::<$type>().write_unaligned(*self);
                }
                *offset += std::mem::size_of::<$type>();
            }

            #[inline(always)]
            unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
                unsafe {
                    buffer.add(offset).cast::<$type>().write_unaligned(*self);
                }
            }

            #[inline]
            unsafe fn decode_and_format(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
                format_spec: &str,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                if format_spec.is_empty() {
                    $append_function(output, value);
                } else {
                    append_integer_with_spec_cast(
                        output,
                        value,
                        ($cast)(value),
                        format_spec,
                        |output, spec| ($append_decimal)(output, value, spec),
                    );
                }
            }

            #[inline]
            unsafe fn decode_and_format_at_offset(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
                format_spec: &str,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                if format_spec.is_empty() {
                    $append_function(output, value);
                } else {
                    append_integer_with_spec_cast(
                        output,
                        value,
                        ($cast)(value),
                        format_spec,
                        |output, spec| ($append_decimal)(output, value, spec),
                    );
                }
            }
        }

        unsafe impl EncodeFast for $type {
            #[inline(always)]
            unsafe fn decode_and_append_fast_at_offset(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                $append_function(output, value);
            }
        }
    };
}

// Integer formatting uses the dedicated number_format path
impl_integer_encode!(
    i32,
    number_format::append_i32,
    |value: i32| (value as u32) as u64,
    append_i32_decimal_with_spec
);
impl_integer_encode!(
    u32,
    number_format::append_u32,
    |value: u32| value as u64,
    append_u32_decimal_with_spec
);
impl_integer_encode!(
    i64,
    number_format::append_i64,
    |value: i64| value as u64,
    append_i64_decimal_with_spec
);
impl_integer_encode!(
    u64,
    number_format::append_u64,
    |value: u64| value,
    append_u64_decimal_with_spec
);
impl_integer_encode!(
    usize,
    number_format::append_u64_from_usize,
    |value: usize| value as u64,
    |output, value, spec| append_u64_decimal_with_spec(output, value as u64, spec)
);
impl_integer_encode!(
    isize,
    number_format::append_i64_from_isize,
    |value: isize| { (value as usize) as u64 },
    |output, value, spec| append_i64_decimal_with_spec(output, value as i64, spec)
);
macro_rules! impl_unsigned_integer_format_traits {
    ($type:ty, $read:expr) => {
        impl EncodeLowerHex for $type {
            #[inline]
            unsafe fn decode_append_lower_hex<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = ($read)(buffer, offset);
                append_lower_hex_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    value as u64,
                );
            }

            #[inline]
            unsafe fn decode_append_lower_hex_at_offset<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_lower_hex_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    value as u64,
                );
            }
        }

        impl EncodeUpperHex for $type {
            #[inline]
            unsafe fn decode_append_upper_hex<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = ($read)(buffer, offset);
                append_upper_hex_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    value as u64,
                );
            }

            #[inline]
            unsafe fn decode_append_upper_hex_at_offset<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_upper_hex_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    value as u64,
                );
            }
        }

        impl EncodeBinary for $type {
            #[inline]
            unsafe fn decode_append_binary<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = ($read)(buffer, offset);
                append_binary_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    value as u64,
                );
            }

            #[inline]
            unsafe fn decode_append_binary_at_offset<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_binary_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    value as u64,
                );
            }
        }

        impl EncodeOctal for $type {
            #[inline]
            unsafe fn decode_append_octal<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = ($read)(buffer, offset);
                append_octal_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    value as u64,
                );
            }

            #[inline]
            unsafe fn decode_append_octal_at_offset<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_octal_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    value as u64,
                );
            }
        }
    };
}

macro_rules! impl_signed_integer_format_traits {
    ($type:ty, $read:expr, $cast:expr) => {
        impl EncodeLowerHex for $type {
            #[inline]
            unsafe fn decode_append_lower_hex<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = ($read)(buffer, offset);
                append_lower_hex_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_lower_hex_at_offset<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_lower_hex_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeUpperHex for $type {
            #[inline]
            unsafe fn decode_append_upper_hex<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = ($read)(buffer, offset);
                append_upper_hex_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_upper_hex_at_offset<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_upper_hex_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeBinary for $type {
            #[inline]
            unsafe fn decode_append_binary<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = ($read)(buffer, offset);
                append_binary_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_binary_at_offset<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_binary_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeOctal for $type {
            #[inline]
            unsafe fn decode_append_octal<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = ($read)(buffer, offset);
                append_octal_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_octal_at_offset<const ALTERNATE: bool>(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_octal_u64_padded_const::<b' ', { FORMAT_ALIGN_NONE }, 0, false, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }
    };
}

impl_signed_integer_format_traits!(
    i32,
    |buffer: *const u8, offset: &mut usize| {
        let value = unsafe { buffer.add(*offset).cast::<i32>().read_unaligned() };
        *offset += std::mem::size_of::<i32>();
        value
    },
    |value: i32| (value as u32) as u64
);
impl_unsigned_integer_format_traits!(u32, |buffer: *const u8, offset: &mut usize| {
    let value = unsafe { buffer.add(*offset).cast::<u32>().read_unaligned() };
    *offset += std::mem::size_of::<u32>();
    value
});
impl_signed_integer_format_traits!(
    i64,
    |buffer: *const u8, offset: &mut usize| {
        let value = unsafe { buffer.add(*offset).cast::<i64>().read_unaligned() };
        *offset += std::mem::size_of::<i64>();
        value
    },
    |value: i64| value as u64
);
impl_unsigned_integer_format_traits!(u64, |buffer: *const u8, offset: &mut usize| {
    let value = unsafe { buffer.add(*offset).cast::<u64>().read_unaligned() };
    *offset += std::mem::size_of::<u64>();
    value
});
impl_unsigned_integer_format_traits!(usize, |buffer: *const u8, offset: &mut usize| {
    let value = unsafe { buffer.add(*offset).cast::<usize>().read_unaligned() };
    *offset += std::mem::size_of::<usize>();
    value
});
impl_signed_integer_format_traits!(
    isize,
    |buffer: *const u8, offset: &mut usize| {
        let value = unsafe { buffer.add(*offset).cast::<isize>().read_unaligned() };
        *offset += std::mem::size_of::<isize>();
        value
    },
    |value: isize| (value as usize) as u64
);

// Small integers are promoted to i64/u64
impl Encode for i8 {
    const ENCODED_SIZE_HINT: usize = 1;

    #[inline(always)]
    fn encoded_size(&self) -> usize {
        1
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            *buffer.add(*offset) = *self as u8;
        }
        *offset += 1;
    }

    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            *buffer.add(offset) = *self as u8;
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let value = unsafe { *buffer.add(*offset) } as i8;
        *offset += 1;
        if format_spec.is_empty() {
            number_format::append_i32(output, value as i32);
        } else {
            append_integer_with_spec_cast(
                output,
                value,
                (value as u8) as u64,
                format_spec,
                |output, spec| append_i32_decimal_with_spec(output, value as i32, spec),
            );
        }
    }
}
unsafe impl EncodeFast for i8 {}

impl_signed_integer_format_traits!(
    i8,
    |buffer: *const u8, offset: &mut usize| {
        let value = unsafe { *buffer.add(*offset) } as i8;
        *offset += 1;
        value
    },
    |value: i8| (value as u8) as u64
);

impl Encode for u8 {
    const ENCODED_SIZE_HINT: usize = 1;

    #[inline(always)]
    fn encoded_size(&self) -> usize {
        1
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            *buffer.add(*offset) = *self;
        }
        *offset += 1;
    }

    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            *buffer.add(offset) = *self;
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let value = unsafe { *buffer.add(*offset) };
        *offset += 1;
        if format_spec.is_empty() {
            number_format::append_u32(output, value as u32);
        } else {
            append_integer_with_spec_cast(
                output,
                value,
                value as u64,
                format_spec,
                |output, spec| append_u32_decimal_with_spec(output, value as u32, spec),
            );
        }
    }
}
unsafe impl EncodeFast for u8 {}

impl_unsigned_integer_format_traits!(u8, |buffer: *const u8, offset: &mut usize| {
    let value = unsafe { *buffer.add(*offset) };
    *offset += 1;
    value
});

impl Encode for i16 {
    const ENCODED_SIZE_HINT: usize = 2;

    #[inline(always)]
    fn encoded_size(&self) -> usize {
        2
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            buffer.add(*offset).cast::<i16>().write_unaligned(*self);
        }
        *offset += 2;
    }

    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            buffer.add(offset).cast::<i16>().write_unaligned(*self);
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let value = unsafe { buffer.add(*offset).cast::<i16>().read_unaligned() };
        *offset += 2;
        if format_spec.is_empty() {
            number_format::append_i32(output, value as i32);
        } else {
            append_integer_with_spec_cast(
                output,
                value,
                (value as u16) as u64,
                format_spec,
                |output, spec| append_i32_decimal_with_spec(output, value as i32, spec),
            );
        }
    }
}
unsafe impl EncodeFast for i16 {}

impl_signed_integer_format_traits!(
    i16,
    |buffer: *const u8, offset: &mut usize| {
        let value = unsafe { buffer.add(*offset).cast::<i16>().read_unaligned() };
        *offset += 2;
        value
    },
    |value: i16| (value as u16) as u64
);

impl Encode for u16 {
    const ENCODED_SIZE_HINT: usize = 2;

    #[inline(always)]
    fn encoded_size(&self) -> usize {
        2
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            buffer.add(*offset).cast::<u16>().write_unaligned(*self);
        }
        *offset += 2;
    }

    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            buffer.add(offset).cast::<u16>().write_unaligned(*self);
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let value = unsafe { buffer.add(*offset).cast::<u16>().read_unaligned() };
        *offset += 2;
        if format_spec.is_empty() {
            number_format::append_u32(output, value as u32);
        } else {
            append_integer_with_spec_cast(
                output,
                value,
                value as u64,
                format_spec,
                |output, spec| append_u32_decimal_with_spec(output, value as u32, spec),
            );
        }
    }
}
unsafe impl EncodeFast for u16 {}

impl_unsigned_integer_format_traits!(u16, |buffer: *const u8, offset: &mut usize| {
    let value = unsafe { buffer.add(*offset).cast::<u16>().read_unaligned() };
    *offset += 2;
    value
});

macro_rules! impl_const_default_integer_format {
    ($type:ty, $cast:expr, $append:ident) => {
        impl EncodeConstDefaultFormat for $type {
            #[inline]
            unsafe fn decode_append_default_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const PRECISION: usize,
                const ZERO_PAD: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                let _ = PRECISION;
                number_format::$append::<FILL, ALIGN, WIDTH, ZERO_PAD>(output, ($cast)(value));
            }

            #[inline]
            unsafe fn decode_append_default_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const PRECISION: usize,
                const ZERO_PAD: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                let _ = PRECISION;
                number_format::$append::<FILL, ALIGN, WIDTH, ZERO_PAD>(output, ($cast)(value));
            }
        }
    };
}

impl_const_default_integer_format!(i32, |value: i32| value, append_i32_padded_const);
impl_const_default_integer_format!(u32, |value: u32| value, append_u32_padded_const);
impl_const_default_integer_format!(i64, |value: i64| value, append_i64_padded_const);
impl_const_default_integer_format!(u64, |value: u64| value, append_u64_padded_const);
impl_const_default_integer_format!(usize, |value: usize| value as u64, append_u64_padded_const);
impl_const_default_integer_format!(isize, |value: isize| value as i64, append_i64_padded_const);
impl_const_default_integer_format!(i8, |value: i8| value as i32, append_i32_padded_const);
impl_const_default_integer_format!(u8, |value: u8| value as u32, append_u32_padded_const);
impl_const_default_integer_format!(i16, |value: i16| value as i32, append_i32_padded_const);
impl_const_default_integer_format!(u16, |value: u16| value as u32, append_u32_padded_const);

macro_rules! impl_const_typed_unsigned_integer_format_traits {
    ($type:ty, $cast:expr) => {
        impl EncodeConstLowerHexFormat for $type {
            #[inline]
            unsafe fn decode_append_lower_hex_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                append_lower_hex_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_lower_hex_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_lower_hex_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeConstUpperHexFormat for $type {
            #[inline]
            unsafe fn decode_append_upper_hex_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                append_upper_hex_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_upper_hex_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_upper_hex_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeConstBinaryFormat for $type {
            #[inline]
            unsafe fn decode_append_binary_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                append_binary_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_binary_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_binary_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeConstOctalFormat for $type {
            #[inline]
            unsafe fn decode_append_octal_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                append_octal_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_octal_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_octal_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }
    };
}

macro_rules! impl_const_typed_signed_integer_format_traits {
    ($type:ty, $cast:expr) => {
        impl EncodeConstLowerHexFormat for $type {
            #[inline]
            unsafe fn decode_append_lower_hex_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                append_lower_hex_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_lower_hex_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_lower_hex_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeConstUpperHexFormat for $type {
            #[inline]
            unsafe fn decode_append_upper_hex_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                append_upper_hex_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_upper_hex_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_upper_hex_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeConstBinaryFormat for $type {
            #[inline]
            unsafe fn decode_append_binary_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                append_binary_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_binary_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_binary_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }

        impl EncodeConstOctalFormat for $type {
            #[inline]
            unsafe fn decode_append_octal_format<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: &mut usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(*offset).cast::<$type>().read_unaligned() };
                *offset += std::mem::size_of::<$type>();
                append_octal_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }

            #[inline]
            unsafe fn decode_append_octal_format_at_offset<
                const FILL: u8,
                const ALIGN: u8,
                const WIDTH: usize,
                const ZERO_PAD: bool,
                const ALTERNATE: bool,
            >(
                buffer: *const u8,
                offset: usize,
                output: &mut Vec<u8>,
            ) {
                let value = unsafe { buffer.add(offset).cast::<$type>().read_unaligned() };
                append_octal_u64_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, ALTERNATE>(
                    output,
                    ($cast)(value),
                );
            }
        }
    };
}

impl_const_typed_signed_integer_format_traits!(i32, |value: i32| (value as u32) as u64);
impl_const_typed_unsigned_integer_format_traits!(u32, |value: u32| value as u64);
impl_const_typed_signed_integer_format_traits!(i64, |value: i64| value as u64);
impl_const_typed_unsigned_integer_format_traits!(u64, |value: u64| value);
impl_const_typed_unsigned_integer_format_traits!(usize, |value: usize| value as u64);
impl_const_typed_signed_integer_format_traits!(isize, |value: isize| (value as usize) as u64);
impl_const_typed_signed_integer_format_traits!(i8, |value: i8| (value as u8) as u64);
impl_const_typed_unsigned_integer_format_traits!(u8, |value: u8| value as u64);
impl_const_typed_signed_integer_format_traits!(i16, |value: i16| (value as u16) as u64);
impl_const_typed_unsigned_integer_format_traits!(u16, |value: u16| value as u64);

impl Encode for bool {
    const ENCODED_SIZE_HINT: usize = 1;

    #[inline(always)]
    fn encoded_size(&self) -> usize {
        1
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            *buffer.add(*offset) = *self as u8;
        }
        *offset += 1;
    }

    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            *buffer.add(offset) = *self as u8;
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let value = unsafe { *buffer.add(*offset) } != 0;
        *offset += 1;
        if format_spec.is_empty() {
            number_format::append_bool(output, value);
        } else {
            append_display_with_spec(output, value, format_spec);
        }
    }
}
unsafe impl EncodeFast for bool {}

impl Encode for f64 {
    const ENCODED_SIZE_HINT: usize = 8;

    #[inline(always)]
    fn encoded_size(&self) -> usize {
        8
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            buffer.add(*offset).cast::<f64>().write_unaligned(*self);
        }
        *offset += 8;
    }

    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            buffer.add(offset).cast::<f64>().write_unaligned(*self);
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let value = unsafe { buffer.add(*offset).cast::<f64>().read_unaligned() };
        *offset += 8;
        if format_spec.is_empty() {
            append_ryu_float(output, value);
        } else {
            append_float_with_spec(output, value, format_spec);
        }
    }
}
unsafe impl EncodeFast for f64 {}

impl EncodeConstDefaultFormat for f64 {
    #[inline]
    unsafe fn decode_append_default_format<
        const FILL: u8,
        const ALIGN: u8,
        const WIDTH: usize,
        const PRECISION: usize,
        const ZERO_PAD: bool,
    >(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    ) {
        let value = unsafe { buffer.add(*offset).cast::<f64>().read_unaligned() };
        *offset += 8;
        let mut body = Vec::new();
        if PRECISION == FORMAT_PRECISION_NONE {
            append_ryu_float(&mut body, value);
        } else {
            let mut writer = VecWriter(&mut body);
            write!(writer, "{:.*}", PRECISION, value).unwrap();
        }
        append_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, { FORMAT_ALIGN_RIGHT }>(output, &body);
    }
}

impl EncodeConstFloatFormat for f64 {
    #[inline]
    unsafe fn decode_append_float_format<
        const FILL: u8,
        const ALIGN: u8,
        const WIDTH: usize,
        const PRECISION: usize,
        const ZERO_PAD: bool,
        const TY: u8,
    >(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    ) {
        let value = unsafe { buffer.add(*offset).cast::<f64>().read_unaligned() };
        *offset += 8;
        let mut body = Vec::new();
        append_const_float_body::<f64, PRECISION, TY>(&mut body, value);
        append_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, { FORMAT_ALIGN_RIGHT }>(output, &body);
    }
}

impl Encode for f32 {
    const ENCODED_SIZE_HINT: usize = 4;

    #[inline(always)]
    fn encoded_size(&self) -> usize {
        4
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            buffer.add(*offset).cast::<f32>().write_unaligned(*self);
        }
        *offset += 4;
    }

    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            buffer.add(offset).cast::<f32>().write_unaligned(*self);
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let value = unsafe { buffer.add(*offset).cast::<f32>().read_unaligned() };
        *offset += 4;
        if format_spec.is_empty() {
            append_ryu_float(output, value);
        } else {
            append_float_with_spec(output, value, format_spec);
        }
    }
}
unsafe impl EncodeFast for f32 {}

impl EncodeConstDefaultFormat for f32 {
    #[inline]
    unsafe fn decode_append_default_format<
        const FILL: u8,
        const ALIGN: u8,
        const WIDTH: usize,
        const PRECISION: usize,
        const ZERO_PAD: bool,
    >(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    ) {
        let value = unsafe { buffer.add(*offset).cast::<f32>().read_unaligned() };
        *offset += 4;
        let mut body = Vec::new();
        if PRECISION == FORMAT_PRECISION_NONE {
            append_ryu_float(&mut body, value);
        } else {
            let mut writer = VecWriter(&mut body);
            write!(writer, "{:.*}", PRECISION, value).unwrap();
        }
        append_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, { FORMAT_ALIGN_RIGHT }>(output, &body);
    }
}

impl EncodeConstFloatFormat for f32 {
    #[inline]
    unsafe fn decode_append_float_format<
        const FILL: u8,
        const ALIGN: u8,
        const WIDTH: usize,
        const PRECISION: usize,
        const ZERO_PAD: bool,
        const TY: u8,
    >(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    ) {
        let value = unsafe { buffer.add(*offset).cast::<f32>().read_unaligned() };
        *offset += 4;
        let mut body = Vec::new();
        append_const_float_body::<f32, PRECISION, TY>(&mut body, value);
        append_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, { FORMAT_ALIGN_RIGHT }>(output, &body);
    }
}

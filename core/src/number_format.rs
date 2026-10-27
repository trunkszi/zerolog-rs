use crate::encode::{
    FORMAT_ALIGN_CENTER, FORMAT_ALIGN_LEFT, FORMAT_ALIGN_NONE, FORMAT_ALIGN_RIGHT,
};

/// ASCII byte pairs for 00-99, totaling 200 bytes.
/// `DIGIT_PAIRS[number * 2 .. number * 2 + 2]` gives the two-digit ASCII representation of `number`.
pub static DIGIT_PAIRS: [u8; 200] = *b"00010203040506070809\
10111213141516171819\
20212223242526272829\
30313233343536373839\
40414243444546474849\
50515253545556575859\
60616263646566676869\
70717273747576777879\
80818283848586878889\
90919293949596979899";

// --- Fixed-width formatting (header time fields) ---

/// Fixed-width integer formatting. The compiler fully unrolls the loop for const `WIDTH`.
/// Combines digit-pair lookups into four-digit groups.
#[inline(always)]
pub fn format_fixed_width<const WIDTH: usize>(buffer: &mut [u8; WIDTH], mut number: u64) {
    if WIDTH & 1 == 1 {
        buffer[WIDTH - 1] = b'0' + (number % 10) as u8;
        number /= 10;
    }
    let mut position = WIDTH & !1;
    while position >= 4 {
        position -= 4;
        write_four_digits(&mut buffer[position..position + 4], (number % 10_000) as u32);
        number /= 10_000;
    }
    if position == 2 {
        write_two_digits(&mut buffer[..2], (number % 100) as u32);
    }
}

#[inline(always)]
fn write_two_digits(output: &mut [u8], number: u32) {
    let index = (number as usize) * 2;
    output[..2].copy_from_slice(&DIGIT_PAIRS[index..index + 2]);
}

#[inline(always)]
fn write_four_digits(output: &mut [u8], number: u32) {
    write_two_digits(&mut output[..2], number / 100);
    write_two_digits(&mut output[2..4], number % 100);
}

// --- Variable-length formatting (user arguments) ---

/// Counts the number of decimal digits in a `u64`.
/// Approximates via `leading_zeros`, then corrects using a power-of-ten table.
#[inline(always)]
pub fn count_digits_u64(number: u64) -> usize {
    static POWERS: [u64; 20] = [
        1,
        10,
        100,
        1_000,
        10_000,
        100_000,
        1_000_000,
        10_000_000,
        100_000_000,
        1_000_000_000,
        10_000_000_000,
        100_000_000_000,
        1_000_000_000_000,
        10_000_000_000_000,
        100_000_000_000_000,
        1_000_000_000_000_000,
        10_000_000_000_000_000,
        100_000_000_000_000_000,
        1_000_000_000_000_000_000,
        10_000_000_000_000_000_000,
    ];
    if number == 0 {
        return 1;
    }
    let bits = 64 - number.leading_zeros() as usize;
    let guess = (bits * 77) >> 8; // log10(2) is approximately 77/256.
    guess + (number >= POWERS[guess]) as usize
}

/// Counts the number of decimal digits in a `u32`.
#[inline(always)]
pub fn count_digits_u32(number: u32) -> usize {
    count_digits_u64(number as u64)
}

/// Variable-length u64 to ASCII, written directly into the destination buffer.
/// Returns the number of bytes written. Zero temporary buffers, zero copies.
#[inline]
pub fn format_u64(output: &mut [u8], mut number: u64) -> usize {
    let length = count_digits_u64(number);
    debug_assert!(output.len() >= length);

    let mut position = length;
    while number >= 10_000 {
        position -= 4;
        write_four_digits(&mut output[position..position + 4], (number % 10_000) as u32);
        number /= 10_000;
    }
    if number >= 100 {
        position -= 2;
        write_two_digits(&mut output[position..position + 2], (number % 100) as u32);
        number /= 100;
    }
    if number >= 10 {
        write_two_digits(&mut output[..2], number as u32);
    } else {
        output[0] = b'0' + number as u8;
    }

    length
}

/// Variable-length i64 to ASCII (handles the negative sign).
#[inline]
pub fn format_i64(output: &mut [u8], number: i64) -> usize {
    if number >= 0 {
        format_u64(output, number as u64)
    } else {
        output[0] = b'-';
        1 + format_u64(&mut output[1..], (!(number as u64)).wrapping_add(1))
    }
}

/// Variable-length u32 to ASCII. 32-bit division is faster than 64-bit.
#[inline]
pub fn format_u32(output: &mut [u8], mut number: u32) -> usize {
    let length = count_digits_u32(number);
    debug_assert!(output.len() >= length);

    let mut position = length;
    while number >= 10_000 {
        position -= 4;
        write_four_digits(&mut output[position..position + 4], number % 10_000);
        number /= 10_000;
    }
    if number >= 100 {
        position -= 2;
        write_two_digits(&mut output[position..position + 2], number % 100);
        number /= 100;
    }
    if number >= 10 {
        write_two_digits(&mut output[..2], number);
    } else {
        output[0] = b'0' + number as u8;
    }

    length
}

/// Variable-length i32 to ASCII.
#[inline]
pub fn format_i32(output: &mut [u8], number: i32) -> usize {
    if number >= 0 {
        format_u32(output, number as u32)
    } else {
        output[0] = b'-';
        1 + format_u32(&mut output[1..], (!(number as u32)).wrapping_add(1))
    }
}

// --- Append directly to Vec<u8> ---

/// Appends the decimal representation of a `u64` to the output buffer.
#[inline]
pub fn append_u64(output: &mut Vec<u8>, number: u64) {
    output.reserve(20);
    unsafe {
        let buffer = core::slice::from_raw_parts_mut(output.as_mut_ptr().add(output.len()), 20);
        let written = format_u64(buffer, number);
        output.set_len(output.len() + written);
    }
}

/// Appends the decimal representation of an `i64` to the output buffer.
#[inline]
pub fn append_i64(output: &mut Vec<u8>, number: i64) {
    output.reserve(21);
    unsafe {
        let buffer = core::slice::from_raw_parts_mut(output.as_mut_ptr().add(output.len()), 21);
        let written = format_i64(buffer, number);
        output.set_len(output.len() + written);
    }
}

/// Appends the decimal representation of a `u32` to the output buffer.
#[inline]
pub fn append_u32(output: &mut Vec<u8>, number: u32) {
    output.reserve(10);
    unsafe {
        let buffer = core::slice::from_raw_parts_mut(output.as_mut_ptr().add(output.len()), 10);
        let written = format_u32(buffer, number);
        output.set_len(output.len() + written);
    }
}

/// Appends the decimal representation of an `i32` to the output buffer.
#[inline]
pub fn append_i32(output: &mut Vec<u8>, number: i32) {
    output.reserve(11);
    unsafe {
        let buffer = core::slice::from_raw_parts_mut(output.as_mut_ptr().add(output.len()), 11);
        let written = format_i32(buffer, number);
        output.set_len(output.len() + written);
    }
}

#[inline(always)]
fn const_alignment<const ALIGN: u8>() -> u8 {
    if ALIGN == FORMAT_ALIGN_NONE { FORMAT_ALIGN_RIGHT } else { ALIGN }
}

#[inline(always)]
fn const_fill<const FILL: u8, const ALIGN: u8, const ZERO_PAD: bool>() -> u8 {
    if ZERO_PAD && ALIGN == FORMAT_ALIGN_NONE { b'0' } else { FILL }
}

#[inline(always)]
pub(crate) unsafe fn reserve_uninitialized(output: &mut Vec<u8>, additional: usize) -> &mut [u8] {
    let start = output.len();
    output.reserve(additional);
    let pointer = unsafe { output.as_mut_ptr().add(start) };
    unsafe {
        output.set_len(start + additional);
        core::slice::from_raw_parts_mut(pointer, additional)
    }
}

#[inline]
pub(crate) fn append_u32_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
>(
    output: &mut Vec<u8>,
    number: u32,
) {
    let digits = count_digits_u32(number);
    if WIDTH == 0 || WIDTH <= digits {
        append_u32(output, number);
        return;
    }

    let alignment = const_alignment::<ALIGN>();
    let fill = const_fill::<FILL, ALIGN, ZERO_PAD>();
    let padding = WIDTH - digits;
    unsafe {
        let buffer = reserve_uninitialized(output, WIDTH);
        match alignment {
            FORMAT_ALIGN_LEFT => {
                format_u32(&mut buffer[..digits], number);
                buffer[digits..].fill(fill);
            }
            FORMAT_ALIGN_CENTER => {
                let left = padding / 2;
                let right_start = left + digits;
                buffer[..left].fill(fill);
                format_u32(&mut buffer[left..right_start], number);
                buffer[right_start..].fill(fill);
            }
            _ => {
                buffer[..padding].fill(fill);
                format_u32(&mut buffer[padding..], number);
            }
        }
    }
}

#[inline]
pub(crate) fn append_u64_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
>(
    output: &mut Vec<u8>,
    number: u64,
) {
    let digits = count_digits_u64(number);
    if WIDTH == 0 || WIDTH <= digits {
        append_u64(output, number);
        return;
    }

    let alignment = const_alignment::<ALIGN>();
    let fill = const_fill::<FILL, ALIGN, ZERO_PAD>();
    let padding = WIDTH - digits;
    unsafe {
        let buffer = reserve_uninitialized(output, WIDTH);
        match alignment {
            FORMAT_ALIGN_LEFT => {
                format_u64(&mut buffer[..digits], number);
                buffer[digits..].fill(fill);
            }
            FORMAT_ALIGN_CENTER => {
                let left = padding / 2;
                let right_start = left + digits;
                buffer[..left].fill(fill);
                format_u64(&mut buffer[left..right_start], number);
                buffer[right_start..].fill(fill);
            }
            _ => {
                buffer[..padding].fill(fill);
                format_u64(&mut buffer[padding..], number);
            }
        }
    }
}

#[inline]
pub(crate) fn append_i32_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
>(
    output: &mut Vec<u8>,
    number: i32,
) {
    let is_negative = number < 0;
    let magnitude = if is_negative { (!(number as u32)).wrapping_add(1) } else { number as u32 };
    let magnitude_digits = count_digits_u32(magnitude);
    let body_len = magnitude_digits + is_negative as usize;
    if WIDTH == 0 || WIDTH <= body_len {
        append_i32(output, number);
        return;
    }

    let alignment = const_alignment::<ALIGN>();
    let fill = const_fill::<FILL, ALIGN, ZERO_PAD>();
    let padding = WIDTH - body_len;
    unsafe {
        let buffer = reserve_uninitialized(output, WIDTH);
        match alignment {
            FORMAT_ALIGN_LEFT => {
                format_i32(&mut buffer[..body_len], number);
                buffer[body_len..].fill(fill);
            }
            FORMAT_ALIGN_CENTER => {
                let left = padding / 2;
                let right_start = left + body_len;
                buffer[..left].fill(fill);
                format_i32(&mut buffer[left..right_start], number);
                buffer[right_start..].fill(fill);
            }
            _ if fill == b'0' && is_negative => {
                buffer[0] = b'-';
                let digits_start = WIDTH - magnitude_digits;
                buffer[1..digits_start].fill(b'0');
                format_u32(&mut buffer[digits_start..], magnitude);
            }
            _ if fill == b'0' => {
                buffer[..padding].fill(b'0');
                format_u32(&mut buffer[padding..], magnitude);
            }
            _ => {
                buffer[..padding].fill(fill);
                format_i32(&mut buffer[padding..], number);
            }
        }
    }
}

#[inline]
pub(crate) fn append_i64_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
>(
    output: &mut Vec<u8>,
    number: i64,
) {
    let is_negative = number < 0;
    let magnitude = if is_negative { (!(number as u64)).wrapping_add(1) } else { number as u64 };
    let magnitude_digits = count_digits_u64(magnitude);
    let body_len = magnitude_digits + is_negative as usize;
    if WIDTH == 0 || WIDTH <= body_len {
        append_i64(output, number);
        return;
    }

    let alignment = const_alignment::<ALIGN>();
    let fill = const_fill::<FILL, ALIGN, ZERO_PAD>();
    let padding = WIDTH - body_len;
    unsafe {
        let buffer = reserve_uninitialized(output, WIDTH);
        match alignment {
            FORMAT_ALIGN_LEFT => {
                format_i64(&mut buffer[..body_len], number);
                buffer[body_len..].fill(fill);
            }
            FORMAT_ALIGN_CENTER => {
                let left = padding / 2;
                let right_start = left + body_len;
                buffer[..left].fill(fill);
                format_i64(&mut buffer[left..right_start], number);
                buffer[right_start..].fill(fill);
            }
            _ if fill == b'0' && is_negative => {
                buffer[0] = b'-';
                let digits_start = WIDTH - magnitude_digits;
                buffer[1..digits_start].fill(b'0');
                format_u64(&mut buffer[digits_start..], magnitude);
            }
            _ if fill == b'0' => {
                buffer[..padding].fill(b'0');
                format_u64(&mut buffer[padding..], magnitude);
            }
            _ => {
                buffer[..padding].fill(fill);
                format_i64(&mut buffer[padding..], number);
            }
        }
    }
}

/// Appends the decimal representation of a `usize` (as `u64`) to the output buffer.
#[inline]
pub fn append_u64_from_usize(output: &mut Vec<u8>, number: usize) {
    append_u64(output, number as u64);
}

/// Appends the decimal representation of an `isize` (as `i64`) to the output buffer.
#[inline]
pub fn append_i64_from_isize(output: &mut Vec<u8>, number: isize) {
    append_i64(output, number as i64);
}

/// Appends `"true"` or `"false"` to the output buffer.
#[inline]
pub fn append_bool(output: &mut Vec<u8>, value: bool) {
    if value {
        output.extend_from_slice(b"true");
    } else {
        output.extend_from_slice(b"false");
    }
}

#[cfg(test)]
mod decimal_scenarios {
    use super::*;
    #[test]
    fn decimal_groups_match_standard_formatting() {
        let mut output = [0_u8; 21];
        for value in 0..=u16::MAX {
            let expected = value.to_string();
            let len = format_u32(&mut output, u32::from(value));
            assert_eq!(&output[..len], expected.as_bytes());
            let len = format_u64(&mut output, u64::from(value));
            assert_eq!(&output[..len], expected.as_bytes());
        }
        for power in 0..20 {
            let base = 10_u64.pow(power);
            for value in [base - 1, base, base.saturating_add(1), u64::MAX] {
                let len = format_u64(&mut output, value);
                assert_eq!(&output[..len], value.to_string().as_bytes());
            }
        }
        let mut fixed = [0; 9];
        format_fixed_width(&mut fixed, 12_340_000_009);
        assert_eq!(&fixed, b"340000009");
        format_fixed_width(&mut [0; 0], u64::MAX);
        let len = format_i64(&mut output, i64::MIN);
        assert_eq!(&output[..len], b"-9223372036854775808");
    }
}

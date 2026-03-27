use crate::encode::{
    FORMAT_ALIGN_CENTER, FORMAT_ALIGN_LEFT, FORMAT_ALIGN_NONE, FORMAT_ALIGN_RIGHT,
};

/// ASCII byte pairs for 00-99, totaling 200 bytes.
/// `DIGIT_PAIRS[number * 2 .. number * 2 + 2]` gives the two-digit ASCII representation of `number`.
#[rustfmt::skip]
pub static DIGIT_PAIRS: [u8; 200] = [
    b'0',b'0', b'0',b'1', b'0',b'2', b'0',b'3', b'0',b'4',
    b'0',b'5', b'0',b'6', b'0',b'7', b'0',b'8', b'0',b'9',
    b'1',b'0', b'1',b'1', b'1',b'2', b'1',b'3', b'1',b'4',
    b'1',b'5', b'1',b'6', b'1',b'7', b'1',b'8', b'1',b'9',
    b'2',b'0', b'2',b'1', b'2',b'2', b'2',b'3', b'2',b'4',
    b'2',b'5', b'2',b'6', b'2',b'7', b'2',b'8', b'2',b'9',
    b'3',b'0', b'3',b'1', b'3',b'2', b'3',b'3', b'3',b'4',
    b'3',b'5', b'3',b'6', b'3',b'7', b'3',b'8', b'3',b'9',
    b'4',b'0', b'4',b'1', b'4',b'2', b'4',b'3', b'4',b'4',
    b'4',b'5', b'4',b'6', b'4',b'7', b'4',b'8', b'4',b'9',
    b'5',b'0', b'5',b'1', b'5',b'2', b'5',b'3', b'5',b'4',
    b'5',b'5', b'5',b'6', b'5',b'7', b'5',b'8', b'5',b'9',
    b'6',b'0', b'6',b'1', b'6',b'2', b'6',b'3', b'6',b'4',
    b'6',b'5', b'6',b'6', b'6',b'7', b'6',b'8', b'6',b'9',
    b'7',b'0', b'7',b'1', b'7',b'2', b'7',b'3', b'7',b'4',
    b'7',b'5', b'7',b'6', b'7',b'7', b'7',b'8', b'7',b'9',
    b'8',b'0', b'8',b'1', b'8',b'2', b'8',b'3', b'8',b'4',
    b'8',b'5', b'8',b'6', b'8',b'7', b'8',b'8', b'8',b'9',
    b'9',b'0', b'9',b'1', b'9',b'2', b'9',b'3', b'9',b'4',
    b'9',b'5', b'9',b'6', b'9',b'7', b'9',b'8', b'9',b'9',
];

// --- Fixed-width formatting (header time fields) ---

/// Fixed-width integer formatting. The compiler fully unrolls the loop for const `WIDTH`.
/// Uses a digit-pairs lookup table to process two digits per iteration.
#[inline(always)]
pub fn format_fixed_width<const WIDTH: usize>(buffer: &mut [u8; WIDTH], mut number: u64) {
    if WIDTH & 1 == 1 {
        buffer[WIDTH - 1] = b'0' + (number % 10) as u8;
        number /= 10;
    }
    let mut position: isize = (WIDTH & !1) as isize - 2;
    while position >= 0 {
        let index = ((number % 100) as usize) << 1;
        buffer[position as usize] = DIGIT_PAIRS[index];
        buffer[position as usize + 1] = DIGIT_PAIRS[index + 1];
        number /= 100;
        position -= 2;
    }
}

// --- Variable-length formatting (user arguments) ---

/// Counts the number of decimal digits in a `u64`.
/// Algorithm: approximate via `leading_zeros`, then correct with a table lookup. ~5 branchless instructions.
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
    let guess = (bits * 77) >> 8; // log10(2) ≈ 77/256
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

    // Process 2 digits at a time, starting from the least significant
    let mut position = length;
    while number >= 100 {
        position -= 2;
        let index = ((number % 100) as usize) << 1;
        output[position] = DIGIT_PAIRS[index];
        output[position + 1] = DIGIT_PAIRS[index + 1];
        number /= 100;
    }

    // Remaining 1 or 2 digits
    if number >= 10 {
        let index = (number as usize) << 1;
        output[0] = DIGIT_PAIRS[index];
        output[1] = DIGIT_PAIRS[index + 1];
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
    while number >= 100 {
        position -= 2;
        let index = ((number % 100) as usize) << 1;
        output[position] = DIGIT_PAIRS[index];
        output[position + 1] = DIGIT_PAIRS[index + 1];
        number /= 100;
    }

    if number >= 10 {
        let index = (number as usize) << 1;
        output[0] = DIGIT_PAIRS[index];
        output[1] = DIGIT_PAIRS[index + 1];
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

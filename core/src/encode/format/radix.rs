use super::*;

#[inline(always)]
fn count_hex_digits_u64(number: u64) -> usize {
    if number == 0 {
        1
    } else {
        let bits = 64 - number.leading_zeros() as usize;
        bits.div_ceil(4)
    }
}

#[inline(always)]
fn count_binary_digits_u64(number: u64) -> usize {
    if number == 0 { 1 } else { 64 - number.leading_zeros() as usize }
}

#[inline(always)]
fn count_octal_digits_u64(number: u64) -> usize {
    if number == 0 {
        1
    } else {
        let bits = 64 - number.leading_zeros() as usize;
        bits.div_ceil(3)
    }
}

#[inline(always)]
fn write_hex_digits_u64<const UPPER: bool>(output: &mut [u8], mut number: u64) {
    if number == 0 {
        output[0] = b'0';
        return;
    }

    let digits = if UPPER { &UPPER_HEX_DIGITS } else { &LOWER_HEX_DIGITS };
    let mut position = output.len();
    while number != 0 {
        position -= 1;
        output[position] = digits[(number & 0x0f) as usize];
        number >>= 4;
    }
}

#[inline(always)]
fn write_binary_digits_u64(output: &mut [u8], mut number: u64) {
    if number == 0 {
        output[0] = b'0';
        return;
    }

    let mut position = output.len();
    while number != 0 {
        position -= 1;
        output[position] = b'0' + (number & 1) as u8;
        number >>= 1;
    }
}

#[inline(always)]
fn write_octal_digits_u64(output: &mut [u8], mut number: u64) {
    if number == 0 {
        output[0] = b'0';
        return;
    }

    let mut position = output.len();
    while number != 0 {
        position -= 1;
        output[position] = b'0' + (number & 0x07) as u8;
        number >>= 3;
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

#[inline]
fn append_prefixed_integer_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
    F,
>(
    output: &mut Vec<u8>,
    prefix: &[u8],
    body_len: usize,
    write_body: F,
) where
    F: FnOnce(&mut [u8]),
{
    let total_len = prefix.len() + body_len;
    if WIDTH == 0 || WIDTH <= total_len {
        unsafe {
            let buffer = number_format::reserve_uninitialized(output, total_len);
            let (prefix_buffer, body_buffer) = buffer.split_at_mut(prefix.len());
            prefix_buffer.copy_from_slice(prefix);
            write_body(body_buffer);
        }
        return;
    }

    let alignment = const_alignment::<ALIGN>();
    let fill = const_fill::<FILL, ALIGN, ZERO_PAD>();
    let padding = WIDTH - total_len;
    unsafe {
        let buffer = number_format::reserve_uninitialized(output, WIDTH);
        match alignment {
            FORMAT_ALIGN_LEFT => {
                let (value_buffer, padding_buffer) = buffer.split_at_mut(total_len);
                let (prefix_buffer, body_buffer) = value_buffer.split_at_mut(prefix.len());
                prefix_buffer.copy_from_slice(prefix);
                write_body(body_buffer);
                padding_buffer.fill(fill);
            }
            FORMAT_ALIGN_CENTER => {
                let left = padding / 2;
                let value_start = left;
                let value_end = value_start + total_len;
                buffer[..value_start].fill(fill);
                buffer[value_end..].fill(fill);
                let (prefix_buffer, body_buffer) =
                    buffer[value_start..value_end].split_at_mut(prefix.len());
                prefix_buffer.copy_from_slice(prefix);
                write_body(body_buffer);
            }
            _ if fill == b'0' && !prefix.is_empty() => {
                let digits_start = prefix.len() + padding;
                buffer[..prefix.len()].copy_from_slice(prefix);
                buffer[prefix.len()..digits_start].fill(b'0');
                write_body(&mut buffer[digits_start..]);
            }
            _ => {
                buffer[..padding].fill(fill);
                let (prefix_buffer, body_buffer) = buffer[padding..].split_at_mut(prefix.len());
                prefix_buffer.copy_from_slice(prefix);
                write_body(body_buffer);
            }
        }
    }
}

#[inline]
pub(super) fn append_prefixed_integer_padded<F>(
    output: &mut Vec<u8>,
    prefix: &[u8],
    body_len: usize,
    spec: &ParsedFormatSpec,
    write_body: F,
) where
    F: FnOnce(&mut [u8]),
{
    let total_len = prefix.len() + body_len;
    let Some(width) = spec.width else {
        unsafe {
            let buffer = number_format::reserve_uninitialized(output, total_len);
            let (prefix_buffer, body_buffer) = buffer.split_at_mut(prefix.len());
            prefix_buffer.copy_from_slice(prefix);
            write_body(body_buffer);
        }
        return;
    };

    if total_len >= width {
        unsafe {
            let buffer = number_format::reserve_uninitialized(output, total_len);
            let (prefix_buffer, body_buffer) = buffer.split_at_mut(prefix.len());
            prefix_buffer.copy_from_slice(prefix);
            write_body(body_buffer);
        }
        return;
    }

    let alignment = spec.alignment.unwrap_or(Alignment::Right);
    let fill = if spec.zero_pad && spec.alignment.is_none() { b'0' } else { spec.fill };
    let padding = width - total_len;
    unsafe {
        let buffer = number_format::reserve_uninitialized(output, width);
        match alignment {
            Alignment::Left => {
                let (value_buffer, padding_buffer) = buffer.split_at_mut(total_len);
                let (prefix_buffer, body_buffer) = value_buffer.split_at_mut(prefix.len());
                prefix_buffer.copy_from_slice(prefix);
                write_body(body_buffer);
                padding_buffer.fill(fill);
            }
            Alignment::Center => {
                let left = padding / 2;
                let value_start = left;
                let value_end = value_start + total_len;
                buffer[..value_start].fill(fill);
                buffer[value_end..].fill(fill);
                let (prefix_buffer, body_buffer) =
                    buffer[value_start..value_end].split_at_mut(prefix.len());
                prefix_buffer.copy_from_slice(prefix);
                write_body(body_buffer);
            }
            Alignment::Right if fill == b'0' && !prefix.is_empty() => {
                let digits_start = prefix.len() + padding;
                buffer[..prefix.len()].copy_from_slice(prefix);
                buffer[prefix.len()..digits_start].fill(b'0');
                write_body(&mut buffer[digits_start..]);
            }
            Alignment::Right => {
                buffer[..padding].fill(fill);
                let (prefix_buffer, body_buffer) = buffer[padding..].split_at_mut(prefix.len());
                prefix_buffer.copy_from_slice(prefix);
                write_body(body_buffer);
            }
        }
    }
}

#[inline]
pub(crate) fn append_lower_hex_u64_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
    const ALTERNATE: bool,
>(
    output: &mut Vec<u8>,
    value: u64,
) {
    let body_len = count_hex_digits_u64(value);
    let prefix = if ALTERNATE { b"0x".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, _>(
        output,
        prefix,
        body_len,
        |body| write_hex_digits_u64::<false>(body, value),
    );
}

#[inline]
pub(crate) fn append_upper_hex_u64_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
    const ALTERNATE: bool,
>(
    output: &mut Vec<u8>,
    value: u64,
) {
    let body_len = count_hex_digits_u64(value);
    let prefix = if ALTERNATE { b"0x".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, _>(
        output,
        prefix,
        body_len,
        |body| write_hex_digits_u64::<true>(body, value),
    );
}

#[inline]
pub(crate) fn append_binary_u64_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
    const ALTERNATE: bool,
>(
    output: &mut Vec<u8>,
    value: u64,
) {
    let body_len = count_binary_digits_u64(value);
    let prefix = if ALTERNATE { b"0b".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, _>(
        output,
        prefix,
        body_len,
        |body| write_binary_digits_u64(body, value),
    );
}

#[inline]
pub(crate) fn append_octal_u64_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
    const ALTERNATE: bool,
>(
    output: &mut Vec<u8>,
    value: u64,
) {
    let body_len = count_octal_digits_u64(value);
    let prefix = if ALTERNATE { b"0o".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, _>(
        output,
        prefix,
        body_len,
        |body| write_octal_digits_u64(body, value),
    );
}

#[inline]
pub(crate) fn append_lower_hex_u64_with_spec(
    output: &mut Vec<u8>,
    value: u64,
    spec: &ParsedFormatSpec,
) {
    let body_len = count_hex_digits_u64(value);
    let prefix = if spec.alternate { b"0x".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded(output, prefix, body_len, spec, |body| {
        write_hex_digits_u64::<false>(body, value)
    });
}

#[inline]
pub(crate) fn append_upper_hex_u64_with_spec(
    output: &mut Vec<u8>,
    value: u64,
    spec: &ParsedFormatSpec,
) {
    let body_len = count_hex_digits_u64(value);
    let prefix = if spec.alternate { b"0x".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded(output, prefix, body_len, spec, |body| {
        write_hex_digits_u64::<true>(body, value)
    });
}

#[inline]
pub(crate) fn append_binary_u64_with_spec(
    output: &mut Vec<u8>,
    value: u64,
    spec: &ParsedFormatSpec,
) {
    let body_len = count_binary_digits_u64(value);
    let prefix = if spec.alternate { b"0b".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded(output, prefix, body_len, spec, |body| {
        write_binary_digits_u64(body, value)
    });
}

#[inline]
pub(crate) fn append_octal_u64_with_spec(
    output: &mut Vec<u8>,
    value: u64,
    spec: &ParsedFormatSpec,
) {
    let body_len = count_octal_digits_u64(value);
    let prefix = if spec.alternate { b"0o".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded(output, prefix, body_len, spec, |body| {
        write_octal_digits_u64(body, value)
    });
}

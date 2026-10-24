use std::fmt::{Display, Write as _};

use super::{
    FORMAT_ALIGN_CENTER, FORMAT_ALIGN_LEFT, FORMAT_ALIGN_NONE, FORMAT_ALIGN_RIGHT,
    FORMAT_PRECISION_NONE,
};
use crate::number_format;

/// An adapter that implements [`std::fmt::Write`] by appending UTF-8 bytes to a `Vec<u8>`.
pub struct VecWriter<'a>(pub &'a mut Vec<u8>);

impl std::fmt::Write for VecWriter<'_> {
    fn write_str(&mut self, string: &str) -> std::fmt::Result {
        let bytes = string.as_bytes();
        self.0.extend_from_slice(bytes);
        Ok(())
    }
}

/// Text alignment direction used in formatted output.
#[derive(Clone, Copy)]
pub(crate) enum Alignment {
    Left,
    Right,
    Center,
}

/// A parsed representation of a Rust-style format specifier string.
pub(crate) struct ParsedFormatSpec {
    pub(crate) fill: u8,
    pub(crate) alignment: Option<Alignment>,
    pub(crate) width: Option<usize>,
    pub(crate) precision: Option<usize>,
    pub(crate) alternate: bool,
    pub(crate) zero_pad: bool,
    pub(crate) ty: Option<u8>,
}

impl ParsedFormatSpec {
    /// Parses a Rust-style format specifier string (e.g., `":>10.2"`) into its components.
    pub(crate) fn parse(format_spec: &str) -> Self {
        let spec = format_spec.strip_prefix(':').unwrap_or(format_spec);
        let bytes = spec.as_bytes();
        let mut index = 0usize;
        let mut parsed = Self {
            fill: b' ',
            alignment: None,
            width: None,
            precision: None,
            alternate: false,
            zero_pad: false,
            ty: None,
        };

        if bytes.len() >= 2
            && let Some(alignment) = parse_alignment(bytes[1])
        {
            parsed.fill = bytes[0];
            parsed.alignment = Some(alignment);
            index = 2;
        }

        if parsed.alignment.is_none()
            && let Some(alignment) = bytes.get(index).copied().and_then(parse_alignment)
        {
            parsed.alignment = Some(alignment);
            index += 1;
        }

        while let Some(ch) = bytes.get(index).copied() {
            match ch {
                b'#' => {
                    parsed.alternate = true;
                    index += 1;
                }
                b'0' if parsed.width.is_none() => {
                    parsed.zero_pad = true;
                    parsed.fill = b'0';
                    index += 1;
                }
                _ => break,
            }
        }

        let width_start = index;
        while matches!(bytes.get(index), Some(value) if value.is_ascii_digit()) {
            index += 1;
        }
        if index > width_start {
            parsed.width = Some(parse_usize_ascii(&bytes[width_start..index]));
        }

        if matches!(bytes.get(index), Some(b'.')) {
            index += 1;
            let precision_start = index;
            while matches!(bytes.get(index), Some(value) if value.is_ascii_digit()) {
                index += 1;
            }
            if index > precision_start {
                parsed.precision = Some(parse_usize_ascii(&bytes[precision_start..index]));
            } else {
                parsed.precision = Some(0);
            }
        }

        parsed.ty = bytes.get(index).copied();

        parsed
    }
}

/// Parses an alignment character (`<`, `>`, `^`) into an [`Alignment`] variant.
pub(crate) fn parse_alignment(value: u8) -> Option<Alignment> {
    match value {
        b'<' => Some(Alignment::Left),
        b'>' => Some(Alignment::Right),
        b'^' => Some(Alignment::Center),
        _ => None,
    }
}

#[inline]
fn parse_usize_ascii(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .try_fold(0usize, |value, digit| {
            value.checked_mul(10)?.checked_add((digit - b'0') as usize)
        })
        .unwrap_or(0)
}

pub(crate) fn write_const_format_spec<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const PRECISION: usize,
    const ZERO_PAD: bool,
    const ALTERNATE: bool,
    const TY: u8,
>(
    output: &mut [u8],
) -> usize {
    let mut length = 0usize;
    output[length] = b':';
    length += 1;

    if ALIGN != FORMAT_ALIGN_NONE {
        output[length] = FILL;
        output[length + 1] = alignment_to_byte(ALIGN);
        length += 2;
    }

    if ALTERNATE {
        output[length] = b'#';
        length += 1;
    }

    if ZERO_PAD {
        output[length] = b'0';
        length += 1;
    }

    if WIDTH != 0 {
        let mut width_buffer = [0u8; 20];
        let width = number_format::format_u64(&mut width_buffer, WIDTH as u64);
        output[length..length + width].copy_from_slice(&width_buffer[..width]);
        length += width;
    }

    if PRECISION != FORMAT_PRECISION_NONE {
        output[length] = b'.';
        length += 1;
        let mut precision_buffer = [0u8; 20];
        let precision = number_format::format_u64(&mut precision_buffer, PRECISION as u64);
        output[length..length + precision].copy_from_slice(&precision_buffer[..precision]);
        length += precision;
    }

    if TY != 0 {
        output[length] = TY;
        length += 1;
    }

    length
}

#[inline]
fn alignment_to_byte(alignment: u8) -> u8 {
    match alignment {
        FORMAT_ALIGN_LEFT => b'<',
        FORMAT_ALIGN_RIGHT => b'>',
        FORMAT_ALIGN_CENTER => b'^',
        _ => unreachable!(),
    }
}

const LOWER_HEX_DIGITS: [u8; 16] = *b"0123456789abcdef";
const UPPER_HEX_DIGITS: [u8; 16] = *b"0123456789ABCDEF";

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
fn append_prefixed_integer_padded<F>(
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

#[inline]
pub(crate) fn append_u32_decimal_with_spec(
    output: &mut Vec<u8>,
    value: u32,
    spec: &ParsedFormatSpec,
) {
    let digits = number_format::count_digits_u32(value);
    append_prefixed_integer_padded(output, b"", digits, spec, |body| {
        number_format::format_u32(body, value);
    });
}

#[inline]
pub(crate) fn append_u64_decimal_with_spec(
    output: &mut Vec<u8>,
    value: u64,
    spec: &ParsedFormatSpec,
) {
    let digits = number_format::count_digits_u64(value);
    append_prefixed_integer_padded(output, b"", digits, spec, |body| {
        number_format::format_u64(body, value);
    });
}

#[inline]
pub(crate) fn append_i32_decimal_with_spec(
    output: &mut Vec<u8>,
    value: i32,
    spec: &ParsedFormatSpec,
) {
    let is_negative = value < 0;
    let magnitude = if is_negative { (!(value as u32)).wrapping_add(1) } else { value as u32 };
    let digits = number_format::count_digits_u32(magnitude);
    let prefix = if is_negative { b"-".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded(output, prefix, digits, spec, |body| {
        number_format::format_u32(body, magnitude);
    });
}

#[inline]
pub(crate) fn append_i64_decimal_with_spec(
    output: &mut Vec<u8>,
    value: i64,
    spec: &ParsedFormatSpec,
) {
    let is_negative = value < 0;
    let magnitude = if is_negative { (!(value as u64)).wrapping_add(1) } else { value as u64 };
    let digits = number_format::count_digits_u64(magnitude);
    let prefix = if is_negative { b"-".as_slice() } else { b"".as_slice() };
    append_prefixed_integer_padded(output, prefix, digits, spec, |body| {
        number_format::format_u64(body, magnitude);
    });
}

#[inline]
fn try_append_integer_u64_with_spec(
    output: &mut Vec<u8>,
    value: u64,
    spec: &ParsedFormatSpec,
) -> bool {
    match spec.ty {
        Some(b'x') => {
            append_lower_hex_u64_with_spec(output, value, spec);
            true
        }
        Some(b'X') => {
            append_upper_hex_u64_with_spec(output, value, spec);
            true
        }
        Some(b'b') => {
            append_binary_u64_with_spec(output, value, spec);
            true
        }
        Some(b'o') => {
            append_octal_u64_with_spec(output, value, spec);
            true
        }
        _ => false,
    }
}

#[inline]
pub(crate) fn append_const_float_body<T, const PRECISION: usize, const TY: u8>(
    output: &mut Vec<u8>,
    value: T,
) where
    T: Display + std::fmt::LowerExp + std::fmt::UpperExp + ryu::Float,
{
    match (TY, PRECISION == FORMAT_PRECISION_NONE) {
        (b'e', true) => {
            let mut writer = VecWriter(output);
            write!(writer, "{:e}", value).unwrap();
        }
        (b'e', false) => {
            let mut writer = VecWriter(output);
            write!(writer, "{:.*e}", PRECISION, value).unwrap();
        }
        (b'E', true) => {
            let mut writer = VecWriter(output);
            write!(writer, "{:E}", value).unwrap();
        }
        (b'E', false) => {
            let mut writer = VecWriter(output);
            write!(writer, "{:.*E}", PRECISION, value).unwrap();
        }
        (_, true) => append_ryu_float(output, value),
        _ => {
            let mut writer = VecWriter(output);
            write!(writer, "{:.*}", PRECISION, value).unwrap();
        }
    }
}

/// Formats an integer value according to `format_spec` and appends the result to `output`.
fn append_integer_with_parsed_spec<T>(output: &mut Vec<u8>, value: T, spec: &ParsedFormatSpec)
where
    T: Display + std::fmt::LowerHex + std::fmt::UpperHex + std::fmt::Binary + std::fmt::Octal,
{
    let mut body = Vec::new();
    {
        let mut writer = VecWriter(&mut body);
        match spec.ty {
            Some(b'x') if spec.alternate => write!(writer, "{:#x}", value).unwrap(),
            Some(b'x') => write!(writer, "{:x}", value).unwrap(),
            Some(b'X') if spec.alternate => write!(writer, "{:#X}", value).unwrap(),
            Some(b'X') => write!(writer, "{:X}", value).unwrap(),
            Some(b'b') if spec.alternate => write!(writer, "{:#b}", value).unwrap(),
            Some(b'b') => write!(writer, "{:b}", value).unwrap(),
            Some(b'o') if spec.alternate => write!(writer, "{:#o}", value).unwrap(),
            Some(b'o') => write!(writer, "{:o}", value).unwrap(),
            _ => write!(writer, "{}", value).unwrap(),
        }
    }
    append_padded(output, &body, spec, Alignment::Right);
}

/// Formats a primitive integer value using a runtime-known `format_spec` and a precomputed u64 view.
pub(crate) fn append_integer_with_spec_cast<T>(
    output: &mut Vec<u8>,
    value: T,
    integer_value: u64,
    format_spec: &str,
    append_default: impl FnOnce(&mut Vec<u8>, &ParsedFormatSpec),
) where
    T: Display + std::fmt::LowerHex + std::fmt::UpperHex + std::fmt::Binary + std::fmt::Octal,
{
    let spec = ParsedFormatSpec::parse(format_spec);
    if try_append_integer_u64_with_spec(output, integer_value, &spec) {
        return;
    }
    if spec.ty.is_none() {
        append_default(output, &spec);
        return;
    }
    append_integer_with_parsed_spec(output, value, &spec);
}

/// Formats a floating-point value according to `format_spec` and appends the result to `output`.
pub(crate) fn append_float_with_spec<T>(output: &mut Vec<u8>, value: T, format_spec: &str)
where
    T: Display + std::fmt::LowerExp + std::fmt::UpperExp,
{
    let spec = ParsedFormatSpec::parse(format_spec);
    let mut body = Vec::new();
    {
        let mut writer = VecWriter(&mut body);
        match (spec.ty, spec.precision) {
            (Some(b'e'), Some(precision)) => write!(writer, "{:.*e}", precision, value).unwrap(),
            (Some(b'e'), None) => write!(writer, "{:e}", value).unwrap(),
            (Some(b'E'), Some(precision)) => write!(writer, "{:.*E}", precision, value).unwrap(),
            (Some(b'E'), None) => write!(writer, "{:E}", value).unwrap(),
            (_, Some(precision)) => write!(writer, "{:.*}", precision, value).unwrap(),
            _ => write!(writer, "{}", value).unwrap(),
        }
    }
    append_padded(output, &body, &spec, Alignment::Right);
}

/// Formats a string value according to `format_spec` and appends the result to `output`.
pub(crate) fn append_str_with_spec(output: &mut Vec<u8>, value: &str, format_spec: &str) {
    let spec = ParsedFormatSpec::parse(format_spec);
    let value = if let Some(precision) = spec.precision {
        truncate_to_boundary(value, precision)
    } else {
        value
    };
    append_padded(output, value.as_bytes(), &spec, Alignment::Left);
}

/// Formats a [`Display`] value according to `format_spec` and appends the result to `output`.
pub fn append_display_with_spec<T>(output: &mut Vec<u8>, value: T, format_spec: &str)
where
    T: Display,
{
    let spec = ParsedFormatSpec::parse(format_spec);
    let mut body = Vec::new();
    {
        let mut writer = VecWriter(&mut body);
        write!(writer, "{}", value).unwrap();
    }
    if let Some(precision) = spec.precision
        && body.len() > precision
    {
        truncate_display(&mut body, precision);
    }
    append_padded(output, &body, &spec, Alignment::Left);
}

/// Appends a [`Display`] value using a compile-time-known default-format configuration.
#[inline]
pub fn append_display_const_default<
    T,
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const PRECISION: usize,
    const ZERO_PAD: bool,
>(
    output: &mut Vec<u8>,
    value: T,
) where
    T: Display,
{
    if WIDTH == 0 && PRECISION == FORMAT_PRECISION_NONE {
        let mut writer = VecWriter(output);
        write!(writer, "{}", value).unwrap();
        return;
    }

    let mut body = Vec::new();
    {
        let mut writer = VecWriter(&mut body);
        write!(writer, "{}", value).unwrap();
    }
    if PRECISION != FORMAT_PRECISION_NONE && body.len() > PRECISION {
        truncate_display(&mut body, PRECISION);
    }
    append_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, { FORMAT_ALIGN_LEFT }>(output, &body);
}

pub(crate) fn append_padded_const<
    const FILL: u8,
    const ALIGN: u8,
    const WIDTH: usize,
    const ZERO_PAD: bool,
    const DEFAULT_ALIGN: u8,
>(
    output: &mut Vec<u8>,
    body: &[u8],
) {
    if WIDTH == 0 || body.len() >= WIDTH {
        output.extend_from_slice(body);
        return;
    }

    let alignment = if ALIGN == FORMAT_ALIGN_NONE { DEFAULT_ALIGN } else { ALIGN };
    let fill = if ZERO_PAD && ALIGN == FORMAT_ALIGN_NONE { b'0' } else { FILL };
    let padding = WIDTH - body.len();

    match alignment {
        FORMAT_ALIGN_LEFT => {
            output.extend_from_slice(body);
            append_fill(output, fill, padding);
        }
        FORMAT_ALIGN_CENTER => {
            let left = padding / 2;
            let right = padding - left;
            append_fill(output, fill, left);
            output.extend_from_slice(body);
            append_fill(output, fill, right);
        }
        FORMAT_ALIGN_RIGHT if fill == b'0' => {
            let prefix_len = numeric_prefix_length(body);
            output.extend_from_slice(&body[..prefix_len]);
            append_fill(output, fill, padding);
            output.extend_from_slice(&body[prefix_len..]);
        }
        _ => {
            append_fill(output, fill, padding);
            output.extend_from_slice(body);
        }
    }
}

/// Appends `body` to `output` with padding applied according to the parsed format spec.
pub(crate) fn append_padded(
    output: &mut Vec<u8>,
    body: &[u8],
    spec: &ParsedFormatSpec,
    default_alignment: Alignment,
) {
    let Some(width) = spec.width else {
        output.extend_from_slice(body);
        return;
    };

    if body.len() >= width {
        output.extend_from_slice(body);
        return;
    }

    let alignment = spec.alignment.unwrap_or(default_alignment);
    let fill = if spec.zero_pad && spec.alignment.is_none() { b'0' } else { spec.fill };
    let padding = width - body.len();

    match alignment {
        Alignment::Left => {
            output.extend_from_slice(body);
            append_fill(output, fill, padding);
        }
        Alignment::Center => {
            let left = padding / 2;
            let right = padding - left;
            append_fill(output, fill, left);
            output.extend_from_slice(body);
            append_fill(output, fill, right);
        }
        Alignment::Right if fill == b'0' => {
            let prefix_len = numeric_prefix_length(body);
            output.extend_from_slice(&body[..prefix_len]);
            append_fill(output, fill, padding);
            output.extend_from_slice(&body[prefix_len..]);
        }
        Alignment::Right => {
            append_fill(output, fill, padding);
            output.extend_from_slice(body);
        }
    }
}

/// Appends `count` copies of the `fill` byte to `output`.
pub(crate) fn append_fill(output: &mut Vec<u8>, fill: u8, count: usize) {
    output.resize(output.len() + count, fill);
}

/// Returns the length of a numeric prefix (e.g., `"-"`, `"0x"`, `"0b"`) in `body`.
pub(crate) fn numeric_prefix_length(body: &[u8]) -> usize {
    if body.starts_with(b"-") || body.starts_with(b"+") {
        1
    } else if body.starts_with(b"0x")
        || body.starts_with(b"0X")
        || body.starts_with(b"0b")
        || body.starts_with(b"0o")
    {
        2
    } else {
        0
    }
}

/// Truncates valid text to at most `max_chars` Unicode scalar values.
pub(crate) fn truncate_to_boundary(value: &str, max_chars: usize) -> &str {
    let end = value.char_indices().nth(max_chars).map_or(value.len(), |(at, _)| at);
    &value[..end]
}

/// Restricts formatted text without splitting a Unicode scalar value.
pub(crate) fn truncate_display(body: &mut Vec<u8>, precision: usize) {
    let value = std::str::from_utf8(body).expect("Display writes valid UTF-8");
    let end = truncate_to_boundary(value, precision).len();
    body.truncate(end);
}

/// Formats a float using the `ryu` crate for maximum speed and appends the result to `output`.
///
/// Trailing `.0` is stripped so that integer-valued floats appear without a decimal point.
pub(crate) fn append_ryu_float<T>(output: &mut Vec<u8>, value: T)
where
    T: ryu::Float,
{
    let mut buffer = ryu::Buffer::new();
    let formatted = buffer.format(value);
    if let Some(trimmed) = formatted.strip_suffix(".0") {
        output.extend_from_slice(trimmed.as_bytes());
    } else {
        output.extend_from_slice(formatted.as_bytes());
    }
}

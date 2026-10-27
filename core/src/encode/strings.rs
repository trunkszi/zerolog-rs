use super::{
    Encode, EncodeFast, FORMAT_ALIGN_LEFT, FORMAT_PRECISION_NONE,
    format::{append_padded_const, append_str_with_spec, truncate_to_boundary},
    traits::EncodeConstDefaultFormat,
};

// --- Layer 2: Fast String Path (length-prefixed) ---

// The complete length-prefixed representation must fit in a u32 payload.
const MAX_STR_LEN: usize = (u32::MAX as usize) - 4;

#[inline(always)]
fn bounded_len(value: &str) -> usize {
    if value.len() <= MAX_STR_LEN { value.len() } else { bounded_len_slow(value) }
}

#[cold]
fn bounded_len_slow(value: &str) -> usize {
    value.floor_char_boundary(MAX_STR_LEN)
}

impl Encode for str {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        4 + bounded_len(self)
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        let len = bounded_len(self);
        let start = *offset;
        // SAFETY: the caller reserves the complete prefix and UTF-8 byte range.
        unsafe {
            buffer.add(start).cast::<u32>().write_unaligned(len as u32);
            core::ptr::copy_nonoverlapping(self.as_ptr(), buffer.add(start + 4), len);
        }
        *offset = start + 4 + len;
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        unsafe {
            let length = buffer.add(*offset).cast::<u32>().read_unaligned() as usize;
            *offset += 4;
            let value = core::str::from_utf8_unchecked(core::slice::from_raw_parts(
                buffer.add(*offset),
                length,
            ));
            if format_spec.is_empty() {
                output.extend_from_slice(value.as_bytes());
            } else {
                append_str_with_spec(output, value, format_spec);
            }
            *offset += length;
        }
    }
}
unsafe impl EncodeFast for str {}

impl EncodeConstDefaultFormat for &str {
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
        let length = unsafe { buffer.add(*offset).cast::<u32>().read_unaligned() as usize };
        *offset += 4;
        let value = unsafe {
            core::str::from_utf8_unchecked(core::slice::from_raw_parts(buffer.add(*offset), length))
        };
        let body = if PRECISION == FORMAT_PRECISION_NONE {
            value
        } else {
            truncate_to_boundary(value, PRECISION)
        };
        append_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, { FORMAT_ALIGN_LEFT }>(
            output,
            body.as_bytes(),
        );
        *offset += length;
    }
}

impl Encode for String {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        self.as_str().encoded_size()
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            self.as_str().encode(buffer, offset);
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        unsafe {
            <&str as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }
}
unsafe impl EncodeFast for String {}

impl EncodeConstDefaultFormat for String {
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
        unsafe {
            <&str as EncodeConstDefaultFormat>::decode_append_default_format::<
                FILL,
                ALIGN,
                WIDTH,
                PRECISION,
                ZERO_PAD,
            >(buffer, offset, output);
        }
    }
}

// References copy their referent's wire representation, never the borrowed address.
impl<T: Encode + ?Sized> Encode for &T {
    const ENCODED_SIZE_HINT: usize = T::ENCODED_SIZE_HINT;
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        T::encoded_size(*self)
    }
    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            T::encode(*self, buffer, offset);
        }
    }
    #[inline(always)]
    unsafe fn encode_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            T::encode_at_offset(*self, buffer, offset);
        }
    }
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        spec: &str,
    ) {
        unsafe {
            T::decode_and_format(buffer, offset, output, spec);
        }
    }
}
// SAFETY: Both fast encoding and decoding use the referent's identical layout.
unsafe impl<T: EncodeFast + ?Sized> EncodeFast for &T {
    #[inline(always)]
    fn encoded_size_fast(&self) -> usize {
        T::encoded_size_fast(*self)
    }
    #[inline(always)]
    unsafe fn encode_fast(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            T::encode_fast(*self, buffer, offset);
        }
    }
    #[inline(always)]
    unsafe fn encode_fast_at_offset(&self, buffer: *mut u8, offset: usize) {
        unsafe {
            T::encode_fast_at_offset(*self, buffer, offset);
        }
    }
    unsafe fn decode_and_append_fast(buffer: *const u8, offset: &mut usize, output: &mut Vec<u8>) {
        unsafe {
            T::decode_and_append_fast(buffer, offset, output);
        }
    }
}

impl Encode for std::borrow::Cow<'_, str> {
    fn encoded_size(&self) -> usize {
        self.as_ref().encoded_size()
    }
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            self.as_ref().encode(buffer, offset);
        }
    }
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        spec: &str,
    ) {
        unsafe {
            <str as Encode>::decode_and_format(buffer, offset, output, spec);
        }
    }
}
// SAFETY: Cow values serialize owned bytes using the string encoding in both paths.
unsafe impl EncodeFast for std::borrow::Cow<'_, str> {}

fn format_bytes(bytes: &[u8], output: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push(b'[');
    for (idx, &byte) in bytes.iter().enumerate() {
        if idx > 0 {
            output.extend_from_slice(b", ");
        }
        output.extend_from_slice(b"0x");
        output.push(HEX[(byte >> 4) as usize]);
        output.push(HEX[(byte & 0x0f) as usize]);
    }
    output.push(b']');
}

impl Encode for [u8] {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        4 + self.len().min(MAX_STR_LEN)
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        let len = self.len().min(MAX_STR_LEN);
        unsafe {
            buffer.add(*offset).cast::<u32>().write_unaligned(len as u32);
            *offset += 4;
            core::ptr::copy_nonoverlapping(self.as_ptr(), buffer.add(*offset), len);
        }
        *offset += len;
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let length = unsafe { buffer.add(*offset).cast::<u32>().read_unaligned() as usize };
        *offset += 4;
        let bytes = unsafe { core::slice::from_raw_parts(buffer.add(*offset), length) };
        if format_spec.is_empty() {
            format_bytes(bytes, output);
        } else {
            let mut staged = Vec::new();
            format_bytes(bytes, &mut staged);
            let text = unsafe { core::str::from_utf8_unchecked(&staged) };
            append_str_with_spec(output, text, format_spec);
        }
        *offset += length;
    }
}
// SAFETY: [u8] length-prefixed layout is identical across fast and general paths.
unsafe impl EncodeFast for [u8] {}

impl Encode for Vec<u8> {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        self.as_slice().encoded_size()
    }
    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe { self.as_slice().encode(buffer, offset) }
    }
    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        spec: &str,
    ) {
        unsafe { <[u8] as Encode>::decode_and_format(buffer, offset, output, spec) }
    }
}
// SAFETY: Vec<u8> delegates to [u8]'s length-prefixed wire layout.
unsafe impl EncodeFast for Vec<u8> {}

impl<const N: usize> Encode for [u8; N] {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        self.as_slice().encoded_size()
    }
    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe { self.as_slice().encode(buffer, offset) }
    }
    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        spec: &str,
    ) {
        unsafe { <[u8] as Encode>::decode_and_format(buffer, offset, output, spec) }
    }
}
// SAFETY: [u8; N] delegates to [u8]'s length-prefixed wire layout.
unsafe impl<const N: usize> EncodeFast for [u8; N] {}

impl<T: Encode> Encode for Option<T> {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        match self {
            Some(v) => 1usize.saturating_add(v.encoded_size()),
            None => 1,
        }
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        match self {
            Some(v) => unsafe {
                buffer.add(*offset).write(1u8);
                *offset += 1;
                v.encode(buffer, offset);
            },
            None => {
                unsafe {
                    buffer.add(*offset).write(0u8);
                }
                *offset += 1;
            }
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let tag = unsafe { buffer.add(*offset).read() };
        *offset += 1;
        if tag == 0 {
            if format_spec.is_empty() {
                output.extend_from_slice(b"None");
            } else {
                append_str_with_spec(output, "None", format_spec);
            }
        } else {
            output.extend_from_slice(b"Some(");
            unsafe { T::decode_and_format(buffer, offset, output, format_spec) };
            output.push(b')');
        }
    }
}
// SAFETY: Option<T> uses the same 1-byte tag + T payload layout on both paths.
unsafe impl<T: Encode> EncodeFast for Option<T> {}

impl<T: Encode, E: Encode> Encode for Result<T, E> {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        match self {
            Ok(v) => 1usize.saturating_add(v.encoded_size()),
            Err(e) => 1usize.saturating_add(e.encoded_size()),
        }
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        match self {
            Ok(v) => unsafe {
                buffer.add(*offset).write(0u8);
                *offset += 1;
                v.encode(buffer, offset);
            },
            Err(e) => unsafe {
                buffer.add(*offset).write(1u8);
                *offset += 1;
                e.encode(buffer, offset);
            },
        }
    }

    #[inline]
    unsafe fn decode_and_format(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
        format_spec: &str,
    ) {
        let tag = unsafe { buffer.add(*offset).read() };
        *offset += 1;
        if tag == 0 {
            output.extend_from_slice(b"Ok(");
            unsafe { T::decode_and_format(buffer, offset, output, format_spec) };
            output.push(b')');
        } else {
            output.extend_from_slice(b"Err(");
            unsafe { E::decode_and_format(buffer, offset, output, format_spec) };
            output.push(b')');
        }
    }
}
// SAFETY: Result<T, E> uses the same 1-byte tag + variant payload layout on both paths.
unsafe impl<T: Encode, E: Encode> EncodeFast for Result<T, E> {}

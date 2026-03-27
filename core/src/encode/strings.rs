use super::{
    Encode, EncodeFast, FORMAT_ALIGN_LEFT, FORMAT_PRECISION_NONE,
    format::{append_padded_const, append_str_with_spec, truncate_to_boundary},
    traits::EncodeConstDefaultFormat,
};

// --- Layer 2: Fast String Path (length-prefixed) ---

impl Encode for &str {
    #[inline(always)]
    fn encoded_size(&self) -> usize {
        4 + self.len() // u32 length prefix + content bytes
    }

    #[inline(always)]
    unsafe fn encode(&self, buffer: *mut u8, offset: &mut usize) {
        unsafe {
            // Write u32 length prefix
            buffer.add(*offset).cast::<u32>().write_unaligned(self.len() as u32);
            *offset += 4;
            // Write content bytes
            core::ptr::copy_nonoverlapping(self.as_ptr(), buffer.add(*offset), self.len());
        }
        *offset += self.len();
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
unsafe impl EncodeFast for &str {}

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

use super::{
    Encode, FORMAT_ALIGN_LEFT, FORMAT_PRECISION_NONE,
    format::{append_padded_const, write_const_format_spec},
};

pub trait EncodeLowerHex: Encode {
    /// Decodes from `buffer + *offset` and appends the lowercase hex representation.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
    unsafe fn decode_append_lower_hex<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    );

    /// Decodes from `buffer + offset` and appends lowercase hex.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
    unsafe fn decode_append_lower_hex_at_offset<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <Self as EncodeLowerHex>::decode_append_lower_hex::<ALTERNATE>(
                buffer,
                &mut offset,
                output,
            );
        }
    }
}

impl<T: Encode> EncodeLowerHex for T {
    default unsafe fn decode_append_lower_hex<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    ) {
        let format_spec = if ALTERNATE { ":#x" } else { ":x" };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_lower_hex_at_offset<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <T as EncodeLowerHex>::decode_append_lower_hex::<ALTERNATE>(
                buffer,
                &mut offset,
                output,
            );
        }
    }
}

pub trait EncodeUpperHex: Encode {
    /// Decodes from `buffer + *offset` and appends the uppercase hex representation.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
    unsafe fn decode_append_upper_hex<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    );

    /// Decodes from `buffer + offset` and appends uppercase hex.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
    unsafe fn decode_append_upper_hex_at_offset<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <Self as EncodeUpperHex>::decode_append_upper_hex::<ALTERNATE>(
                buffer,
                &mut offset,
                output,
            );
        }
    }
}

impl<T: Encode> EncodeUpperHex for T {
    default unsafe fn decode_append_upper_hex<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    ) {
        let format_spec = if ALTERNATE { ":#X" } else { ":X" };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_upper_hex_at_offset<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <T as EncodeUpperHex>::decode_append_upper_hex::<ALTERNATE>(
                buffer,
                &mut offset,
                output,
            );
        }
    }
}

pub trait EncodeBinary: Encode {
    /// Decodes from `buffer + *offset` and appends the binary representation.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
    unsafe fn decode_append_binary<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    );

    /// Decodes from `buffer + offset` and appends binary.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
    unsafe fn decode_append_binary_at_offset<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <Self as EncodeBinary>::decode_append_binary::<ALTERNATE>(buffer, &mut offset, output);
        }
    }
}

impl<T: Encode> EncodeBinary for T {
    default unsafe fn decode_append_binary<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    ) {
        let format_spec = if ALTERNATE { ":#b" } else { ":b" };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_binary_at_offset<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <T as EncodeBinary>::decode_append_binary::<ALTERNATE>(buffer, &mut offset, output);
        }
    }
}

pub trait EncodeOctal: Encode {
    /// Decodes from `buffer + *offset` and appends the octal representation.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
    unsafe fn decode_append_octal<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    );

    /// Decodes from `buffer + offset` and appends octal.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
    unsafe fn decode_append_octal_at_offset<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <Self as EncodeOctal>::decode_append_octal::<ALTERNATE>(buffer, &mut offset, output);
        }
    }
}

impl<T: Encode> EncodeOctal for T {
    default unsafe fn decode_append_octal<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: &mut usize,
        output: &mut Vec<u8>,
    ) {
        let format_spec = if ALTERNATE { ":#o" } else { ":o" };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_octal_at_offset<const ALTERNATE: bool>(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <T as EncodeOctal>::decode_append_octal::<ALTERNATE>(buffer, &mut offset, output);
        }
    }
}

pub trait EncodeConstDefaultFormat: Encode {
    /// Decodes from `buffer + *offset` and appends the value with compile-time formatting.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
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
    );

    /// Decodes from `buffer + offset` and appends the value with compile-time formatting.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
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
        let mut offset = offset;
        unsafe {
            <Self as EncodeConstDefaultFormat>::decode_append_default_format::<
                FILL,
                ALIGN,
                WIDTH,
                PRECISION,
                ZERO_PAD,
            >(buffer, &mut offset, output);
        }
    }
}

impl<T: Encode> EncodeConstDefaultFormat for T {
    default unsafe fn decode_append_default_format<
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
        let mut body = Vec::new();
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, &mut body, "");
        }
        if PRECISION != FORMAT_PRECISION_NONE && body.len() > PRECISION {
            super::format::truncate_display(&mut body, PRECISION);
        }
        append_padded_const::<FILL, ALIGN, WIDTH, ZERO_PAD, { FORMAT_ALIGN_LEFT }>(output, &body);
    }

    #[inline(always)]
    default unsafe fn decode_append_default_format_at_offset<
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
        let mut offset = offset;
        unsafe {
            <T as EncodeConstDefaultFormat>::decode_append_default_format::<
                FILL,
                ALIGN,
                WIDTH,
                PRECISION,
                ZERO_PAD,
            >(buffer, &mut offset, output);
        }
    }
}

mod extended;
pub use extended::{
    EncodeConstBinaryFormat, EncodeConstFloatFormat, EncodeConstLowerHexFormat,
    EncodeConstOctalFormat, EncodeConstUpperHexFormat,
};

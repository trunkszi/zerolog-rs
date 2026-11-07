use super::*;

pub trait EncodeConstLowerHexFormat: Encode {
    /// Decodes from `buffer + *offset` and appends formatted lowercase hex.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
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
    );

    /// Decodes from `buffer + offset` and appends formatted lowercase hex.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
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
        let mut offset = offset;
        unsafe {
            <Self as EncodeConstLowerHexFormat>::decode_append_lower_hex_format::<
                FILL,
                ALIGN,
                WIDTH,
                ZERO_PAD,
                ALTERNATE,
            >(buffer, &mut offset, output);
        }
    }
}

impl<T: Encode> EncodeConstLowerHexFormat for T {
    default unsafe fn decode_append_lower_hex_format<
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
        let mut format_spec = [0u8; 32];
        let length = write_const_format_spec::<
            FILL,
            ALIGN,
            WIDTH,
            FORMAT_PRECISION_NONE,
            ZERO_PAD,
            ALTERNATE,
            b'x',
        >(&mut format_spec);
        let format_spec = unsafe { std::str::from_utf8_unchecked(&format_spec[..length]) };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_lower_hex_format_at_offset<
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
        let mut offset = offset;
        unsafe {
            <T as EncodeConstLowerHexFormat>::decode_append_lower_hex_format::<
                FILL,
                ALIGN,
                WIDTH,
                ZERO_PAD,
                ALTERNATE,
            >(buffer, &mut offset, output);
        }
    }
}

pub trait EncodeConstUpperHexFormat: Encode {
    /// Decodes from `buffer + *offset` and appends formatted uppercase hex.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
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
    );

    /// Decodes from `buffer + offset` and appends formatted uppercase hex.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
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
        let mut offset = offset;
        unsafe {
            <Self as EncodeConstUpperHexFormat>::decode_append_upper_hex_format::<
                FILL,
                ALIGN,
                WIDTH,
                ZERO_PAD,
                ALTERNATE,
            >(buffer, &mut offset, output);
        }
    }
}

impl<T: Encode> EncodeConstUpperHexFormat for T {
    default unsafe fn decode_append_upper_hex_format<
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
        let mut format_spec = [0u8; 32];
        let length = write_const_format_spec::<
            FILL,
            ALIGN,
            WIDTH,
            FORMAT_PRECISION_NONE,
            ZERO_PAD,
            ALTERNATE,
            b'X',
        >(&mut format_spec);
        let format_spec = unsafe { std::str::from_utf8_unchecked(&format_spec[..length]) };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_upper_hex_format_at_offset<
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
        let mut offset = offset;
        unsafe {
            <T as EncodeConstUpperHexFormat>::decode_append_upper_hex_format::<
                FILL,
                ALIGN,
                WIDTH,
                ZERO_PAD,
                ALTERNATE,
            >(buffer, &mut offset, output);
        }
    }
}

pub trait EncodeConstBinaryFormat: Encode {
    /// Decodes from `buffer + *offset` and appends formatted binary.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
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
    );

    /// Decodes from `buffer + offset` and appends formatted binary.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
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
        let mut offset = offset;
        unsafe {
            <Self as EncodeConstBinaryFormat>::decode_append_binary_format::<
                FILL,
                ALIGN,
                WIDTH,
                ZERO_PAD,
                ALTERNATE,
            >(buffer, &mut offset, output);
        }
    }
}

impl<T: Encode> EncodeConstBinaryFormat for T {
    default unsafe fn decode_append_binary_format<
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
        let mut format_spec = [0u8; 32];
        let length = write_const_format_spec::<
            FILL,
            ALIGN,
            WIDTH,
            FORMAT_PRECISION_NONE,
            ZERO_PAD,
            ALTERNATE,
            b'b',
        >(&mut format_spec);
        let format_spec = unsafe { std::str::from_utf8_unchecked(&format_spec[..length]) };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_binary_format_at_offset<
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
        let mut offset = offset;
        unsafe {
            <T as EncodeConstBinaryFormat>::decode_append_binary_format::<
                FILL,
                ALIGN,
                WIDTH,
                ZERO_PAD,
                ALTERNATE,
            >(buffer, &mut offset, output);
        }
    }
}

pub trait EncodeConstOctalFormat: Encode {
    /// Decodes from `buffer + *offset` and appends formatted octal.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
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
    );

    /// Decodes from `buffer + offset` and appends formatted octal.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
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
        let mut offset = offset;
        unsafe {
            <Self as EncodeConstOctalFormat>::decode_append_octal_format::<
                FILL,
                ALIGN,
                WIDTH,
                ZERO_PAD,
                ALTERNATE,
            >(buffer, &mut offset, output);
        }
    }
}

impl<T: Encode> EncodeConstOctalFormat for T {
    default unsafe fn decode_append_octal_format<
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
        let mut format_spec = [0u8; 32];
        let length = write_const_format_spec::<
            FILL,
            ALIGN,
            WIDTH,
            FORMAT_PRECISION_NONE,
            ZERO_PAD,
            ALTERNATE,
            b'o',
        >(&mut format_spec);
        let format_spec = unsafe { std::str::from_utf8_unchecked(&format_spec[..length]) };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_octal_format_at_offset<
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
        let mut offset = offset;
        unsafe {
            <T as EncodeConstOctalFormat>::decode_append_octal_format::<
                FILL,
                ALIGN,
                WIDTH,
                ZERO_PAD,
                ALTERNATE,
            >(buffer, &mut offset, output);
        }
    }
}

pub trait EncodeConstFloatFormat: Encode {
    /// Decodes from `buffer + *offset` and appends formatted float.
    ///
    /// # Safety
    /// `buffer + *offset` must point to valid encoded data written by [`Encode::encode`].
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
    );

    /// Decodes from `buffer + offset` and appends formatted float.
    ///
    /// # Safety
    /// `buffer + offset` must point to valid encoded data.
    #[inline(always)]
    unsafe fn decode_append_float_format_at_offset<
        const FILL: u8,
        const ALIGN: u8,
        const WIDTH: usize,
        const PRECISION: usize,
        const ZERO_PAD: bool,
        const TY: u8,
    >(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <Self as EncodeConstFloatFormat>::decode_append_float_format::<
                FILL,
                ALIGN,
                WIDTH,
                PRECISION,
                ZERO_PAD,
                TY,
            >(buffer, &mut offset, output);
        }
    }
}

impl<T: Encode> EncodeConstFloatFormat for T {
    default unsafe fn decode_append_float_format<
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
        let mut format_spec = [0u8; 32];
        let length = write_const_format_spec::<FILL, ALIGN, WIDTH, PRECISION, ZERO_PAD, false, TY>(
            &mut format_spec,
        );
        let format_spec = unsafe { std::str::from_utf8_unchecked(&format_spec[..length]) };
        unsafe {
            <T as Encode>::decode_and_format(buffer, offset, output, format_spec);
        }
    }

    #[inline(always)]
    default unsafe fn decode_append_float_format_at_offset<
        const FILL: u8,
        const ALIGN: u8,
        const WIDTH: usize,
        const PRECISION: usize,
        const ZERO_PAD: bool,
        const TY: u8,
    >(
        buffer: *const u8,
        offset: usize,
        output: &mut Vec<u8>,
    ) {
        let mut offset = offset;
        unsafe {
            <T as EncodeConstFloatFormat>::decode_append_float_format::<
                FILL,
                ALIGN,
                WIDTH,
                PRECISION,
                ZERO_PAD,
                TY,
            >(buffer, &mut offset, output);
        }
    }
}

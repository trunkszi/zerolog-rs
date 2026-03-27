use ferrilog::{
    EncodeConstBinaryFormat, EncodeConstDefaultFormat, EncodeConstFloatFormat,
    EncodeConstLowerHexFormat, EncodeConstOctalFormat, EncodeConstUpperHexFormat, EncodeLowerHex,
    FORMAT_ALIGN_CENTER, FORMAT_ALIGN_NONE, FORMAT_ALIGN_RIGHT, FORMAT_PRECISION_NONE,
    encode::Encode,
};

#[derive(Clone, Copy, ferrilog::Encode)]
struct Point {
    x: i32,
    y: i32,
}

impl std::fmt::Display for Point {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "({}, {})", self.x, self.y)
    }
}

fn roundtrip_test<T: Encode + Copy>(value: T, expected_output: &str) {
    let size = value.encoded_size();
    let mut buffer = vec![0u8; size + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);
        assert_eq!(encode_offset, size);

        let mut decode_offset = 0;
        let mut output = Vec::new();
        T::decode_and_format(buffer.as_ptr(), &mut decode_offset, &mut output, "");
        assert_eq!(decode_offset, size);
        assert_eq!(std::str::from_utf8(&output).unwrap(), expected_output);
    }
}

#[test]
fn test_integers() {
    roundtrip_test(0i32, "0");
    roundtrip_test(42i32, "42");
    roundtrip_test(-1i32, "-1");
    roundtrip_test(i32::MIN, "-2147483648");
    roundtrip_test(u32::MAX, "4294967295");
    roundtrip_test(0u64, "0");
    roundtrip_test(123456789u64, "123456789");
}

#[test]
fn test_small_integers() {
    roundtrip_test(0i8, "0");
    roundtrip_test(-128i8, "-128");
    roundtrip_test(255u8, "255");
    roundtrip_test(0i16, "0");
    roundtrip_test(-32768i16, "-32768");
    roundtrip_test(65535u16, "65535");
}

#[test]
fn test_bool() {
    roundtrip_test(true, "true");
    roundtrip_test(false, "false");
}

#[test]
fn test_float() {
    roundtrip_test(2.72f64, "2.72");
    roundtrip_test(0.0f32, "0");
}

#[test]
fn test_str() {
    let string = "hello world";
    let size = string.encoded_size();
    assert_eq!(size, 4 + 11); // u32 prefix + content

    let mut buffer = vec![0u8; size + 16];
    unsafe {
        let mut encode_offset = 0;
        string.encode(buffer.as_mut_ptr(), &mut encode_offset);
        assert_eq!(encode_offset, size);

        let mut decode_offset = 0;
        let mut output = Vec::new();
        <&str>::decode_and_format(buffer.as_ptr(), &mut decode_offset, &mut output, "");
        assert_eq!(decode_offset, size);
        assert_eq!(std::str::from_utf8(&output).unwrap(), "hello world");
    }
}

#[test]
fn test_const_default_format_u32_matches_runtime_spec() {
    let value = 42u32;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        u32::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":>12");

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <u32 as EncodeConstDefaultFormat>::decode_append_default_format::<
            b' ',
            { FORMAT_ALIGN_RIGHT },
            12,
            { FORMAT_PRECISION_NONE },
            false,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_const_default_format_i32_zero_pad_matches_runtime_spec() {
    let value = -42i32;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        i32::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":08");

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <i32 as EncodeConstDefaultFormat>::decode_append_default_format::<
            b'0',
            { FORMAT_ALIGN_NONE },
            8,
            { FORMAT_PRECISION_NONE },
            true,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_const_default_format_str_matches_runtime_spec() {
    let value = "hello world";
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        <&str>::decode_and_format(
            buffer.as_ptr(),
            &mut runtime_offset,
            &mut runtime_output,
            ":^16.5",
        );

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <&str as EncodeConstDefaultFormat>::decode_append_default_format::<
            b' ',
            { FORMAT_ALIGN_CENTER },
            16,
            5,
            false,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_const_default_format_f64_matches_runtime_spec() {
    let value = std::f64::consts::PI;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        f64::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":>10.2");

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <f64 as EncodeConstDefaultFormat>::decode_append_default_format::<
            b' ',
            { FORMAT_ALIGN_RIGHT },
            10,
            2,
            false,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_const_padded_hex_matches_runtime_spec() {
    let value = 0x2Au32;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        u32::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":#010x");

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <u32 as EncodeConstLowerHexFormat>::decode_append_lower_hex_format::<
            b'0',
            { FORMAT_ALIGN_NONE },
            10,
            true,
            true,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_specialized_hex_matches_runtime_spec() {
    let value = 0x2Au32;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        u32::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":x");

        let mut specialized_offset = 0;
        let mut specialized_output = Vec::new();
        <u32 as EncodeLowerHex>::decode_append_lower_hex::<false>(
            buffer.as_ptr(),
            &mut specialized_offset,
            &mut specialized_output,
        );

        assert_eq!(runtime_offset, specialized_offset);
        assert_eq!(runtime_output, specialized_output);
    }
}

#[test]
fn test_specialized_signed_hex_matches_runtime_spec() {
    let value = -42i8;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        i8::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":#x");

        let mut specialized_offset = 0;
        let mut specialized_output = Vec::new();
        <i8 as EncodeLowerHex>::decode_append_lower_hex::<true>(
            buffer.as_ptr(),
            &mut specialized_offset,
            &mut specialized_output,
        );

        assert_eq!(runtime_offset, specialized_offset);
        assert_eq!(runtime_output, specialized_output);
    }
}

#[test]
fn test_const_signed_padded_hex_matches_runtime_spec() {
    let value = -42i8;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        i8::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":#06x");

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <i8 as EncodeConstLowerHexFormat>::decode_append_lower_hex_format::<
            b'0',
            { FORMAT_ALIGN_NONE },
            6,
            true,
            true,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_const_padded_upper_hex_matches_runtime_spec() {
    let value = 0x2Au32;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        u32::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":#010X");

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <u32 as EncodeConstUpperHexFormat>::decode_append_upper_hex_format::<
            b'0',
            { FORMAT_ALIGN_NONE },
            10,
            true,
            true,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_const_padded_binary_matches_runtime_spec() {
    let value = 0x2Au32;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        u32::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":#014b");

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <u32 as EncodeConstBinaryFormat>::decode_append_binary_format::<
            b'0',
            { FORMAT_ALIGN_NONE },
            14,
            true,
            true,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_const_padded_octal_matches_runtime_spec() {
    let value = 0x2Au32;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        u32::decode_and_format(buffer.as_ptr(), &mut runtime_offset, &mut runtime_output, ":#08o");

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <u32 as EncodeConstOctalFormat>::decode_append_octal_format::<
            b'0',
            { FORMAT_ALIGN_NONE },
            8,
            true,
            true,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_const_scientific_float_matches_runtime_spec() {
    let value = std::f64::consts::PI;
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        f64::decode_and_format(
            buffer.as_ptr(),
            &mut runtime_offset,
            &mut runtime_output,
            ":>14.3e",
        );

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <f64 as EncodeConstFloatFormat>::decode_append_float_format::<
            b' ',
            { FORMAT_ALIGN_RIGHT },
            14,
            3,
            false,
            b'e',
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

#[test]
fn test_derived_const_default_format_matches_runtime_spec() {
    let value = Point { x: 12, y: 34 };
    let mut buffer = vec![0u8; value.encoded_size() + 16];

    unsafe {
        let mut encode_offset = 0;
        value.encode(buffer.as_mut_ptr(), &mut encode_offset);

        let mut runtime_offset = 0;
        let mut runtime_output = Vec::new();
        Point::decode_and_format(
            buffer.as_ptr(),
            &mut runtime_offset,
            &mut runtime_output,
            ":^12.5",
        );

        let mut const_offset = 0;
        let mut const_output = Vec::new();
        <Point as EncodeConstDefaultFormat>::decode_append_default_format::<
            b' ',
            { FORMAT_ALIGN_CENTER },
            12,
            5,
            false,
        >(buffer.as_ptr(), &mut const_offset, &mut const_output);

        assert_eq!(runtime_offset, const_offset);
        assert_eq!(runtime_output, const_output);
    }
}

use zerolog::number_format::*;

#[test]
fn test_format_fixed_width_2() {
    let mut buffer = [0u8; 2];
    format_fixed_width(&mut buffer, 5);
    assert_eq!(&buffer, b"05");
    format_fixed_width(&mut buffer, 42);
    assert_eq!(&buffer, b"42");
    format_fixed_width(&mut buffer, 0);
    assert_eq!(&buffer, b"00");
}

#[test]
fn test_format_fixed_width_4() {
    let mut buffer = [0u8; 4];
    format_fixed_width(&mut buffer, 2026);
    assert_eq!(&buffer, b"2026");
    format_fixed_width(&mut buffer, 7);
    assert_eq!(&buffer, b"0007");
}

#[test]
fn test_format_fixed_width_9() {
    let mut buffer = [0u8; 9];
    format_fixed_width(&mut buffer, 123456789);
    assert_eq!(&buffer, b"123456789");
    format_fixed_width(&mut buffer, 123);
    assert_eq!(&buffer, b"000000123");
}

#[test]
fn test_count_digits() {
    assert_eq!(count_digits_u64(0), 1);
    assert_eq!(count_digits_u64(1), 1);
    assert_eq!(count_digits_u64(9), 1);
    assert_eq!(count_digits_u64(10), 2);
    assert_eq!(count_digits_u64(99), 2);
    assert_eq!(count_digits_u64(100), 3);
    assert_eq!(count_digits_u64(999), 3);
    assert_eq!(count_digits_u64(1_000_000_000), 10);
    assert_eq!(count_digits_u64(u64::MAX), 20);
}

#[test]
fn test_format_u64() {
    let mut buffer = [0u8; 20];
    assert_eq!(format_u64(&mut buffer, 0), 1);
    assert_eq!(&buffer[..1], b"0");

    assert_eq!(format_u64(&mut buffer, 42), 2);
    assert_eq!(&buffer[..2], b"42");

    assert_eq!(format_u64(&mut buffer, 123456789), 9);
    assert_eq!(&buffer[..9], b"123456789");

    assert_eq!(format_u64(&mut buffer, 10), 2);
    assert_eq!(&buffer[..2], b"10");

    let length = format_u64(&mut buffer, u64::MAX);
    assert_eq!(&buffer[..length], b"18446744073709551615");
}

#[test]
fn test_format_i64() {
    let mut buffer = [0u8; 21];
    assert_eq!(format_i64(&mut buffer, 0), 1);
    assert_eq!(&buffer[..1], b"0");

    assert_eq!(format_i64(&mut buffer, -1), 2);
    assert_eq!(&buffer[..2], b"-1");

    assert_eq!(format_i64(&mut buffer, -42), 3);
    assert_eq!(&buffer[..3], b"-42");

    let length = format_i64(&mut buffer, i64::MIN);
    assert_eq!(&buffer[..length], b"-9223372036854775808");
}

#[test]
fn test_format_u32() {
    let mut buffer = [0u8; 10];
    assert_eq!(format_u32(&mut buffer, 0), 1);
    assert_eq!(&buffer[..1], b"0");

    assert_eq!(format_u32(&mut buffer, 100), 3);
    assert_eq!(&buffer[..3], b"100");

    let length = format_u32(&mut buffer, u32::MAX);
    assert_eq!(&buffer[..length], b"4294967295");
}

#[test]
fn test_format_i32() {
    let mut buffer = [0u8; 11];
    let length = format_i32(&mut buffer, i32::MIN);
    assert_eq!(&buffer[..length], b"-2147483648");
}

#[test]
fn test_append_functions() {
    let mut output = Vec::new();
    append_u64(&mut output, 12345);
    assert_eq!(&output, b"12345");

    output.clear();
    append_i64(&mut output, -99);
    assert_eq!(&output, b"-99");

    output.clear();
    append_u32(&mut output, 0);
    assert_eq!(&output, b"0");

    output.clear();
    append_i32(&mut output, -2147483648);
    assert_eq!(&output, b"-2147483648");

    output.clear();
    append_bool(&mut output, true);
    assert_eq!(&output, b"true");

    output.clear();
    append_bool(&mut output, false);
    assert_eq!(&output, b"false");
}

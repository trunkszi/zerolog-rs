use ferrilog::timestamp_counter::TimestampCounter;

#[test]
fn test_read_system_nanoseconds() {
    let nanoseconds = TimestampCounter::read_system_nanoseconds();
    assert!(nanoseconds > 0);
}

#[test]
fn test_read_timestamp_counter() {
    let timestamp = TimestampCounter::read_timestamp_counter();
    assert!(timestamp > 0);
}

#[test]
fn test_init_and_convert() {
    let mut counter = TimestampCounter::new();
    counter.init(5_000_000); // 5ms calibration

    let before = TimestampCounter::read_system_nanoseconds();
    let timestamp = TimestampCounter::read_timestamp_counter();
    let converted = counter.timestamp_to_nanoseconds(timestamp);
    let after = TimestampCounter::read_system_nanoseconds();

    // Converted value should be within the before..after range (1ms tolerance)
    assert!(
        converted >= before - 1_000_000 && converted <= after + 1_000_000,
        "converted={converted}, before={before}, after={after}"
    );
}

#[test]
fn test_calibrate() {
    let mut counter = TimestampCounter::new();
    counter.init(5_000_000);
    counter.calibrate();
}

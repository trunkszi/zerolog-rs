use zerolog::timestamp_counter::TimestampCounter;

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

#[test]
fn ready_front_merge_does_not_wait_for_unpublished_earlier_records() {
    use zerolog::{
        poller::ReadyMessageHeap,
        spsc_queue::{PAYLOAD_HEADER_SIZE, PAYLOAD_TIMESTAMP_OFFSET, SpscVarQueue},
        thread_buffer::ThreadBuffer,
    };
    // SAFETY: this test owns both allocations and is their sole producer/consumer.
    // Both buffers outlive the heap; initialized payloads contain no borrowed data.
    unsafe {
        let first = ThreadBuffer::new_on_heap();
        let second = ThreadBuffer::new_on_heap();
        let reserve = |buffer: *mut ThreadBuffer, timestamp: i64| {
            let header =
                SpscVarQueue::producer_alloc(&raw mut (*buffer).queue, PAYLOAD_HEADER_SIZE as u32)
                    .unwrap();
            let payload = zerolog::spsc_queue::MessageHeader::payload_pointer_raw(header);
            payload.write_bytes(0, PAYLOAD_HEADER_SIZE);
            payload.add(PAYLOAD_TIMESTAMP_OFFSET).cast::<i64>().write_unaligned(timestamp);
            header
        };
        let older = reserve(first, 10);
        let newer = reserve(second, 20);
        (*newer).push(PAYLOAD_HEADER_SIZE as u32);
        let mut heap = ReadyMessageHeap::from_buffers(&[first, second]);
        let first_emitted = heap.pop().unwrap().raw_timestamp();
        SpscVarQueue::consumer_pop(&raw mut (*second).queue);
        assert!(heap.pop().is_none());
        (*older).push(PAYLOAD_HEADER_SIZE as u32);
        heap.refresh_buffer(first);
        let second_emitted = heap.pop().unwrap().raw_timestamp();
        SpscVarQueue::consumer_pop(&raw mut (*first).queue);
        assert_eq!([first_emitted, second_emitted], [20, 10]);
        drop(heap);
        ThreadBuffer::free_on_heap(first);
        ThreadBuffer::free_on_heap(second);
    }
}

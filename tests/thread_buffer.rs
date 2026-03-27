use ferrilog::thread_buffer::*;

#[test]
fn test_preallocate_and_get() {
    preallocate();
    let pointer = get_thread_buffer();
    assert!(!pointer.is_null());

    // Calling again should return the same pointer
    let pointer2 = get_thread_buffer();
    assert_eq!(pointer, pointer2);
}

#[test]
fn test_alloc_message() {
    let header = alloc_message(16);
    assert!(header.is_some());
}

#[test]
fn test_thread_buffer_registered() {
    preallocate();
    assert!(THREAD_BUFFER_REGISTRY.count() > 0);
}

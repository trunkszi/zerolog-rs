use zerolog::thread_buffer::*;

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

#[test]
fn sequential_logging_threads_reuse_reclaimed_registry_slots() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct CountLines(Arc<AtomicUsize>);
    impl std::io::Write for CountLines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.fetch_add(bytes.iter().filter(|&&b| b == b'\n').count(), Ordering::Relaxed);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    zerolog::stop_polling_thread().unwrap();
    let decoded = Arc::new(AtomicUsize::new(0));
    zerolog::set_output_writer(CountLines(decoded.clone()));
    zerolog::set_header_pattern("").unwrap();
    for value in 0..350u32 {
        std::thread::spawn(move || zerolog::info!("churn={}", value)).join().unwrap();
        zerolog::poll(true).unwrap();
        assert!(THREAD_BUFFER_REGISTRY.count() <= MAX_THREADS);
    }
    assert_eq!(decoded.load(Ordering::Relaxed), 350);
}

#[test]
fn late_tls_destructor_logging_does_not_abort() {
    const CHILD: &str = "ZEROLOG_SHARD1_LATE_TLS_CHILD";
    if std::env::var_os(CHILD).is_some() {
        struct Late;
        impl Drop for Late {
            fn drop(&mut self) {
                zerolog::info!("late TLS log is safely discarded");
                set_thread_name("too late");
                assert!(get_thread_buffer().is_null());
                assert!(alloc_message(8).is_none());
            }
        }
        thread_local! { static LATE: Late = const { Late }; }
        std::thread::spawn(|| {
            LATE.with(|_| {});
            zerolog::info!("before TLS exit");
        })
        .join()
        .unwrap();
        return;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "late_tls_destructor_logging_does_not_abort", "--nocapture"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "child failed: {:?}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

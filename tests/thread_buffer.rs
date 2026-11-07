#![allow(clippy::unwrap_used, clippy::expect_used)]
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

#[test]
fn macro_rings_after_urgent_publication() -> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "ZEROLOG_PRODUCER_GATE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "macro_rings_after_urgent_publication"])
            .env(CHILD, "1")
            .status()?;
        assert!(status.success());
        return Ok(());
    }
    use std::sync::atomic::Ordering;

    use zerolog::contract::{DOORBELL, DoorbellWord, LEVEL_GATE, LevelGateWord};
    LEVEL_GATE.store(LevelGateWord::pack(2, 4, false), Ordering::SeqCst);
    DOORBELL.store(0, Ordering::Release);
    zerolog::info!("ordinary={}", 42u32);
    assert_eq!(DOORBELL.load(Ordering::Acquire), 0);
    #[cfg(target_os = "linux")]
    let waiter = {
        use zerolog::contract::{DOORBELL_PARKED_BIT, futex_wait_doorbell_timeout};
        DOORBELL.store(DOORBELL_PARKED_BIT, Ordering::SeqCst);
        let (sender, receiver) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || -> std::io::Result<()> {
            let task = std::fs::read_link("/proc/thread-self")?;
            sender.send(task).map_err(std::io::Error::other)?;
            futex_wait_doorbell_timeout(DOORBELL_PARKED_BIT, Some(5_000_000_000))
        });
        let task = receiver.recv()?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            let stat =
                std::fs::read_to_string(std::path::Path::new("/proc").join(&task).join("stat"))?;
            if stat.rsplit_once(')').and_then(|(_, tail)| tail.split_whitespace().next())
                == Some("S")
            {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "waiter never parked");
            std::thread::yield_now();
        }
        waiter
    };
    zerolog::error!("urgent={}", 43u32);
    #[cfg(target_os = "linux")]
    waiter.join().map_err(|_| "waiter panicked")??;
    let notified = DoorbellWord::new(DOORBELL.load(Ordering::Acquire));
    assert_eq!(notified.get(DoorbellWord::RING_SEQ), 1);
    assert!(notified.test(DoorbellWord::URGENT));
    let pointer = get_thread_buffer();
    // SAFETY: no other consumer runs in this isolated subprocess; the producer is live.
    unsafe {
        let queue = &raw mut (*pointer).queue;
        for _ in 0..2 {
            assert!(zerolog::spsc_queue::SpscVarQueue::consumer_front(queue).is_some());
            zerolog::spsc_queue::SpscVarQueue::consumer_pop(queue);
        }
        assert!(zerolog::spsc_queue::SpscVarQueue::consumer_front(queue).is_none());
    }
    Ok(())
}

#[test]
fn cached_gate_notifies_late_publication() -> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "ZEROLOG_CACHED_GATE_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "cached_gate_notifies_late_publication"])
            .env(CHILD, "1")
            .status()?;
        assert!(status.success());
        return Ok(());
    }
    use std::sync::{atomic::Ordering, mpsc};

    use zerolog::contract::{
        DOORBELL, DOORBELL_PARKED_BIT, DoorbellWord, LEVEL_GATE, LevelGateWord,
    };
    LEVEL_GATE.store(LevelGateWord::pack(2, 4, false), Ordering::SeqCst);
    DOORBELL.store(0, Ordering::SeqCst);
    let (ready_tx, ready_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    let (finish_tx, finish_rx) = mpsc::channel();
    let producer = std::thread::spawn(move || -> std::io::Result<()> {
        zerolog::info!("paused={}", {
            ready_tx.send(()).map_err(std::io::Error::other)?;
            resume_rx.recv().map_err(std::io::Error::other)?;
            17u32
        });
        done_tx.send(()).map_err(std::io::Error::other)?;
        // Keep the producer alive so TLS retirement cannot supply a missing wake.
        finish_rx.recv().map_err(std::io::Error::other)?;
        Ok(())
    });
    ready_rx.recv_timeout(std::time::Duration::from_secs(2))?;
    LEVEL_GATE.store(LevelGateWord::pack(2, 0, true), Ordering::SeqCst);
    DOORBELL.store(DOORBELL_PARKED_BIT, Ordering::SeqCst);
    resume_tx.send(())?;
    done_rx.recv_timeout(std::time::Duration::from_secs(2))?;
    let observed = DoorbellWord::new(DOORBELL.load(Ordering::Acquire)).get(DoorbellWord::RING_SEQ);
    finish_tx.send(())?;
    producer.join().map_err(|_| "producer panicked")??;
    DOORBELL.store(0, Ordering::SeqCst);
    assert_eq!(observed, 1, "late publication must wake a deeply parked consumer");
    Ok(())
}

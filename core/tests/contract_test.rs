#![deny(clippy::unwrap_used, clippy::expect_used)]
use uarch::prelude::*;
use zerolog_core::contract::{LevelGateWord, RecordCursor};

#[test]
fn packed_fields_keep_neighbors() {
    use std::sync::atomic::Ordering;

    use zerolog_core::contract::LEVEL_GATE;

    assert_eq!(LevelGateWord::pack(2, 6, true), 0x1_0602);
    assert_eq!(LEVEL_GATE.load(Ordering::Relaxed), LevelGateWord::pack(1, 5, false));
    let cursor = RecordCursor::new(0).patch(patch!(RecordCursor {
        BLOCK_INDEX: 0x00ab_cdef,
        SLOT: 0xfe,
        SLOT_EPOCH: 0x1234_5678
    }));
    assert_eq!(cursor.raw(), 0x1234_5678_feab_cdef);
    assert_eq!(cursor.get(RecordCursor::BLOCK_INDEX), 0x00ab_cdef);
    assert_eq!(cursor.get(RecordCursor::SLOT), 254);
    assert_eq!(cursor.get(RecordCursor::SLOT_EPOCH), 0x1234_5678);
}

#[test]
fn slab_preserves_unwritten_suffix() {
    let bytes = Vec::from(&b"abcdef-unused"[..]).into_boxed_slice();
    let address = bytes.as_ptr();
    let mut slab = zerolog_core::FrozenSlab::new(bytes, 6);
    assert_eq!(slab.active_slice(), b"abcdef");
    assert!(!slab.advance(2));
    assert_eq!(slab.len(), 4);
    assert_eq!(slab.active_slice(), b"cdef");
    assert!(!slab.advance(0));
    assert!(slab.advance(4));
    assert!(slab.is_empty());
    let bytes = slab.into_inner_box();
    assert_eq!(bytes.as_ptr(), address);
    assert_eq!(&*bytes, b"abcdef-unused");
}

fn with_lease_lock<T>(body: impl FnOnce() -> T) -> T {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    body()
}

#[test]
fn lease_conflict_then_next_generation() -> Result<(), Box<dyn std::error::Error>> {
    with_lease_lock(lease_conflict_then_next_generation_locked)
}

fn lease_conflict_then_next_generation_locked() -> Result<(), Box<dyn std::error::Error>> {
    use zerolog_core::{ConsumerLease, ZeroLogError};
    let first = ConsumerLease::try_acquire()?;
    let generation = first.generation();
    assert!(generation > 0);
    assert!(matches!(ConsumerLease::try_acquire(), Err(ZeroLogError::ConsumerBusy)));
    let contenders: Vec<_> = (0..8)
        .map(|_| {
            std::thread::spawn(|| {
                matches!(ConsumerLease::try_acquire(), Err(ZeroLogError::ConsumerBusy))
            })
        })
        .collect();
    for contender in contenders {
        assert!(contender.join().map_err(|_| "contender panicked")?);
    }
    drop(first);
    let second = ConsumerLease::try_acquire()?;
    assert_eq!(second.generation(), generation + 1);
    Ok(())
}

#[test]
fn lease_generation_wraps_from_max() -> Result<(), Box<dyn std::error::Error>> {
    with_lease_lock(|| {
        use zerolog_core::ConsumerLease;
        assert!(ConsumerLease::seed_idle_generation(u32::MAX));
        let lease = ConsumerLease::try_acquire()?;
        assert_eq!(lease.generation(), 0);
        drop(lease);
        let next = ConsumerLease::try_acquire()?;
        assert_eq!(next.generation(), 1);
        Ok(())
    })
}

#[test]
fn lease_poison_child() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var_os("ZEROLOG_LEASE_POISON_CHILD").is_none() {
        return Ok(());
    }
    let lease = zerolog_core::ConsumerLease::try_acquire()?;
    lease.poison_on_abandon();
    assert!(matches!(
        zerolog_core::ConsumerLease::try_acquire(),
        Err(zerolog_core::ZeroLogError::ConsumerPoisoned)
    ));
    Ok(())
}

#[test]
#[cfg(not(miri))]
fn abandoned_lease_stays_poisoned() -> Result<(), Box<dyn std::error::Error>> {
    let status = std::process::Command::new(std::env::current_exe()?)
        .args(["--exact", "lease_poison_child"])
        .env("ZEROLOG_LEASE_POISON_CHILD", "1")
        .status()?;
    assert!(status.success());
    Ok(())
}

#[test]
#[cfg(all(target_os = "linux", not(miri)))]
fn doorbell_wakes_parked_waiter() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        sync::atomic::Ordering,
        time::{Duration, Instant},
    };

    use zerolog_core::contract::*;
    DOORBELL.store(DOORBELL_PARKED_BIT, Ordering::SeqCst);
    let (sender, receiver) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || -> std::io::Result<()> {
        // SAFETY: gettid takes no pointers and returns this thread's identity.
        let tid = unsafe { libc::syscall(libc::SYS_gettid) };
        sender.send(tid).map_err(std::io::Error::other)?;
        futex_wait_doorbell_timeout(DOORBELL_PARKED_BIT, Some(5_000_000_000))
    });
    let tid = receiver.recv()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat"))?;
        let state = stat.rsplit_once(')').and_then(|(_, tail)| tail.split_whitespace().next());
        if state == Some("S") {
            break;
        }
        assert!(Instant::now() < deadline, "waiter never parked");
        std::thread::yield_now();
    }
    ring_doorbell(true);
    waiter.join().map_err(|_| "waiter panicked")??;
    let value = DoorbellWord::new(DOORBELL.load(Ordering::Acquire));
    assert!(!value.test(DoorbellWord::PARKED));
    assert!(value.test(DoorbellWord::URGENT));
    assert_eq!(value.get(DoorbellWord::RING_SEQ), 1);
    let mismatch = futex_wait_doorbell_timeout(DOORBELL_PARKED_BIT, Some(0));
    assert_eq!(mismatch.err().and_then(|e| e.raw_os_error()), Some(libc::EAGAIN));
    let expired = futex_wait_doorbell_timeout(value.raw(), Some(0));
    assert_eq!(expired.err().and_then(|e| e.raw_os_error()), Some(libc::ETIMEDOUT));
    DOORBELL.store(u32::MAX, Ordering::SeqCst);
    ring_doorbell(false);
    assert_eq!(DOORBELL.load(Ordering::Acquire), 2);
    DOORBELL.store(0, Ordering::Release);
    Ok(())
}

#[test]
fn report_fields_keep_status_bits() {
    use zerolog_core::{DrainBudget, DrainFlags, DrainReport, HeapNode};
    let flags = DrainFlags::new(0).patch(
        BitPatch::EMPTY
            .with_flag(DrainFlags::MORE_WORK, true)
            .with_flag(DrainFlags::OUTPUT_READY, true)
            .with_flag(DrainFlags::FLUSH_DUE, true)
            .with_flag(DrainFlags::BACKPRESSURE, true)
            .with_field(DrainFlags::DRAINED, u32::MAX as u64),
    );
    let report = DrainReport { next_deadline_ns: -1, flags };
    assert_eq!(report.flags.raw(), 0xffff_ffff_0000_0017);
    assert!(!report.flags.test(DrainFlags::IDLE));
    assert_eq!(report.flags.get(DrainFlags::DRAINED), u32::MAX);
    let budget = DrainBudget { max_records: 1, max_bytes: u32::MAX };
    assert_eq!(budget.max_records, 1);
    let cursor = RecordCursor::new(u64::MAX);
    let node = HeapNode { raw_timestamp: i64::MIN, cursor };
    assert_eq!(node.cursor.get(RecordCursor::BLOCK_INDEX), 0x00ff_ffff);
    assert_eq!(node.raw_timestamp, i64::MIN);
}

#[test]
#[should_panic(expected = "slab length exceeds allocation")]
fn slab_rejects_invalid_length() {
    let _ = zerolog_core::FrozenSlab::new(Box::from(&b"abc"[..]), 4);
}

#[test]
#[should_panic(expected = "completion exceeds slab suffix")]
fn slab_rejects_oversized_completion() {
    let mut slab = zerolog_core::FrozenSlab::new(Box::from(&b"abc"[..]), 3);
    slab.advance(1);
    slab.advance(u32::MAX);
}

#[test]
fn errors_preserve_os_codes() {
    use std::io::{Error, ErrorKind};

    use zerolog_core::ZeroLogError;
    let write: Error = ZeroLogError::Write(28).into();
    let rotation: Error = ZeroLogError::Rotation { step: 4, errno: 13 }.into();
    assert_eq!(write.raw_os_error(), Some(28));
    assert_eq!(rotation.raw_os_error(), Some(13));
    for (error, kind) in [
        (ZeroLogError::ConsumerBusy, ErrorKind::WouldBlock),
        (ZeroLogError::SqRingFull, ErrorKind::WouldBlock),
        (ZeroLogError::WriteZero, ErrorKind::WriteZero),
        (ZeroLogError::InvalidConfig("bad"), ErrorKind::InvalidInput),
        (ZeroLogError::BlockingTargetOnSharedProactor, ErrorKind::Unsupported),
        (ZeroLogError::UnsupportedMode("poll"), ErrorKind::Unsupported),
        (ZeroLogError::ConsumerPoisoned, ErrorKind::Other),
        (ZeroLogError::SelfStop, ErrorKind::Other),
    ] {
        assert_eq!(Error::from(error).kind(), kind);
    }
}

use super::*;
use crate::test_support::{Core, pipe, slab};
#[test]
fn sync_backoff_preserves_written_prefix() -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Read;
    let (mut reader, writer) = pipe()?;
    let mut target = Rotation::from_fd(writer);
    let bytes = vec![b'x'; 1024 * 1024];
    let mut core = Core::default();
    core.pending.push_back(slab(&bytes));
    let mut retry = SyncRetry::default();
    let budget = DrainBudget { max_records: 1, max_bytes: u32::MAX };
    drain_and_write_sync(&mut core, &mut target, 0, budget, false, &mut retry);
    assert!(core.completed > 0 && core.completed < bytes.len() as u32);
    assert_eq!(retry.write.deadline_ns, Some(1_000_000));
    let prefix = core.completed;
    let mut received = vec![0; prefix as usize];
    reader.read_exact(&mut received)?;
    drain_and_write_sync(&mut core, &mut target, 999_999, budget, false, &mut retry);
    assert_eq!(core.completed, prefix);
    let mut now = 1_000_000;
    while core.has_unflushed() {
        let before = core.completed;
        drain_and_write_sync(&mut core, &mut target, now, budget, false, &mut retry);
        let count = core.completed - before;
        let mut part = vec![0; count as usize];
        reader.read_exact(&mut part)?;
        received.extend_from_slice(&part);
        now = retry.write.deadline_ns.unwrap_or(now + 1);
    }
    assert_eq!(received, bytes);
    assert_eq!(target.current_size(), bytes.len() as u64);
    assert!(retry.write.deadline_ns.is_none());
    assert_eq!(retry.write.backoff_ns, 0);
    assert!(core.drains > 1);
    Ok(())
}

#[test]
fn fallback_shutdown_failure_is_bounded() -> Result<(), Box<dyn std::error::Error>> {
    let mut core = Core::default();
    core.pending.push_back(slab(b"unwritable"));
    let target = Rotation::from_fd(std::fs::File::options().write(true).open("/dev/full")?.into());
    let mut config =
        WorkerConfig { shutdown_timeout: Duration::from_millis(1), ..WorkerConfig::default() };
    config.sink.idle_interval_ns = Some(1);
    let stop = AtomicBool::new(true);
    assert_eq!(
        run_futex(OwnedConsumer::embedded(core, drop), target, &stop, config),
        Err(ZeroLogError::Write(libc::ETIMEDOUT))
    );
    Ok(())
}

#[test]
fn fallback_rotates_completed_slabs() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("zl-futex-rotation-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let base = directory.join("events");
    let worker_base = base.clone();
    fn unavailable(_: ProactorConfig) -> liburing::Result<Proactor> {
        Err(UringError::RingInit(libc::EPERM))
    }
    let worker = UringConsumerThread::spawn_with_ring_factory(
        move || {
            let mut core = Core::default();
            core.pending.push_back(slab(b"one\n"));
            core.pending.push_back(slab(b"two\n"));
            Ok((OwnedConsumer::embedded(core, drop), Rotation::open(&worker_base, 4, 2, true)?))
        },
        WorkerConfig::default(),
        unavailable,
    )?;
    assert_eq!(worker.backend(), WorkerBackend::Futex { startup_errno: libc::EPERM });
    worker.shutdown()?;
    assert_eq!(std::fs::read(directory.join("events.2"))?, b"one\n");
    assert_eq!(std::fs::read(directory.join("events.1"))?, b"two\n");
    assert_eq!(std::fs::read(base)?, b"");
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn rotation_backoff_keeps_writes_running() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("zl-futex-backoff-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let base = directory.join("events");
    let mut target = Rotation::open(&base, 4, 1, false)?;
    target.write_all_sync(b"one\n")?;
    std::fs::rename(&base, directory.join("retained"))?;
    target.begin();
    target.complete_step(-libc::ENOENT)?;
    target.complete_step(0)?;
    std::fs::create_dir(&base)?;
    let mut core = Core::default();
    let mut retry = SyncRetry::default();
    let budget = DrainBudget { max_records: 1, max_bytes: u32::MAX };
    drain_and_write_sync(&mut core, &mut target, 0, budget, false, &mut retry);
    assert_eq!(retry.rotation.deadline_ns, Some(1_000_000));
    assert_eq!(retry.last_error, Some(ZeroLogError::Rotation { step: 5, errno: libc::EISDIR }));
    core.pending.push_back(slab(b"live\n"));
    drain_and_write_sync(&mut core, &mut target, 1, budget, false, &mut retry);
    assert_eq!(core.completed, 5);
    assert_eq!(std::fs::read(directory.join("retained"))?, b"one\nlive\n");
    assert_eq!(retry.rotation.deadline_ns, Some(1_000_000));
    assert!(retry.write.deadline_ns.is_none());
    std::fs::remove_dir(&base)?;
    drain_and_write_sync(&mut core, &mut target, 999_999, budget, false, &mut retry);
    assert_eq!(target.step(), RotationStep::Open);
    drain_and_write_sync(&mut core, &mut target, 1_000_000, budget, false, &mut retry);
    assert_eq!(target.step(), RotationStep::Idle);
    assert!(retry.rotation.deadline_ns.is_none());
    assert_eq!(retry.rotation.backoff_ns, 0);
    assert_eq!(std::fs::read(&base)?, b"");
    assert!(!directory.join("events.1").exists());
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

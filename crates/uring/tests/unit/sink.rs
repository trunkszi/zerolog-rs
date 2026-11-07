use super::*;
use crate::test_support::{Core, slab};
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn sink(target: Rotation) -> ProactorLogSink<Core> {
    sink_with(Core::default(), target)
}
pub(super) fn sink_with<C: ConsumerCore>(core: C, target: Rotation) -> ProactorLogSink<C> {
    ProactorLogSink {
        consumer: Some(OwnedConsumer::embedded(core, drop)),
        target: Some(target),
        subsystem: SubsystemId(1),
        owner_id: 0,
        config: SinkConfig::default(),
        origin: Instant::now(),
        lifecycle: LifecycleFsm::Running,
        wake: WakeFsm::Disarmed,
        timer: TimerFsm::Disarmed,
        write: WriteRotateFsm::Idle,
        continuation: None,
        timer_update: None,
        flags: SinkFlags::new(0),
        last_error: None,
        desired_deadline: None,
        retry_backoff_ns: 0,
        retry_deadline: None,
    }
}
fn key(slot: u64) -> liburing::Result<OperationKey> {
    OperationKey::from_payload(liburing::OperationPayload((1 << 24) | slot))
}
#[test]
fn teardown_uses_target_in_order() -> TestResult {
    use std::io::Read;
    let (mut reader, writer) = crate::test_support::pipe()?;
    let mut sink = sink(Rotation::from_fd(writer));
    let mut suffix = slab(b"old-ready");
    suffix.advance(4);
    sink.write = WriteRotateFsm::Ready(suffix);
    sink.core_mut().pending.push_back(slab(b"-pending"));
    sink.flush_pending_sync();
    let mut bytes = [0; 13];
    reader.read_exact(&mut bytes)?;
    assert_eq!(&bytes, b"ready-pending");
    assert_eq!(sink.core_mut().completed, 13);
    assert!(!sink.core_mut().has_unflushed());
    Ok(())
}
#[test]
fn timer_update_expiry_races_are_benign() -> TestResult {
    for errno in [libc::ENOENT, libc::EALREADY] {
        for target_first in [false, true] {
            let mut sink = sink(Rotation::from_fd(
                std::fs::File::options().write(true).open("/dev/null")?.into(),
            ));
            let timer = key(1)?;
            let update = key(2)?;
            sink.timer = TimerFsm::Armed {
                key: timer,
                timeout: Timeout::new(Duration::from_secs(1))?,
                deadline_ns: 1_000_000_000,
            };
            sink.timer_update = Some(update);
            if target_first {
                sink.handle_terminal(timer, -libc::ETIME);
            }
            sink.handle_terminal(update, -errno);
            assert!(sink.last_error().is_none());
            assert!(sink.timer_update.is_none());
            if !target_first {
                assert!(matches!(sink.timer, TimerFsm::Armed { .. }));
                sink.handle_terminal(timer, -libc::ETIME);
            }
            assert!(matches!(sink.timer, TimerFsm::Disarmed));
        }
    }
    Ok(())
}
#[test]
fn inflight_io_allows_pending_formatting() -> TestResult {
    let mut sink =
        sink(Rotation::from_fd(std::fs::File::options().write(true).open("/dev/null")?.into()));
    for rotating in [false, true] {
        sink.write = if rotating {
            WriteRotateFsm::Rotating { key: key(1)? }
        } else {
            WriteRotateFsm::Writing { key: key(1)?, slab: slab(b"frozen") }
        };
        sink.core_mut().frozen = true;
        sink.core_mut().incoming = Some(b"next".to_vec());
        assert!(
            sink.drain_core(0).is_some_and(|report| report.flags.test(DrainFlags::OUTPUT_READY))
        );
        assert_eq!(
            sink.core_mut().pending.back().map(FrozenSlab::active_slice),
            Some(&b"next"[..])
        );
        sink.write = WriteRotateFsm::Idle;
    }
    sink.write = WriteRotateFsm::Ready(slab(b"suffix"));
    let before = sink.core_mut().drains;
    assert!(sink.drain_core(0).is_none());
    assert_eq!(sink.core_mut().drains, before);
    Ok(())
}
#[test]
fn rotation_backoff_allows_current_write() -> TestResult {
    let path = std::env::temp_dir().join(format!("zl-backoff-unit-{}", std::process::id()));
    let mut target = Rotation::open(&path, 1, 1, false)?;
    target.record_written(1);
    target.begin();
    target.complete_step(-libc::ENOENT)?;
    target.complete_step(0)?;
    let mut ring = Proactor::new(liburing::ProactorConfig::default())?;
    let mut sink = sink(target);
    sink.owner_id = ring.owner_id();
    sink.core_mut().pending.push_back(slab(b"during-backoff"));
    sink.output_failed(ZeroLogError::Rotation { step: 5, errno: libc::EIO });
    sink.retry_deadline = Some(i64::MAX);
    sink.take_last_error();
    ring.prepare_keyed_sqe_with(SubsystemId(2), |sqe| {
        // SAFETY: NOP has no borrowed operands.
        unsafe { ops::prep_nop(sqe) };
    })?;
    ring.wait_completions(
        1,
        Some(Duration::from_secs(1)),
        &mut |completion: Completion, submissions: &mut SubmissionContext<'_>| {
            submissions.claim_completion(completion)?;
            sink.service(submissions)
        },
    )?;
    assert!(matches!(sink.write, WriteRotateFsm::Writing { .. }));
    ring.wait_completions(1, Some(Duration::from_secs(1)), &mut sink)?;
    assert_eq!(sink.core_mut().completed, 14);
    assert_eq!(sink.target().step(), RotationStep::Open);
    assert_eq!(sink.retry_deadline, Some(i64::MAX));
    assert_eq!(std::fs::read(&path)?, b"during-backoff");
    ring.shutdown_with(&mut sink)?;
    std::fs::remove_file(path)?;
    Ok(())
}

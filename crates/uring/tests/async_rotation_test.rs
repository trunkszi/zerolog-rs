#![deny(clippy::unwrap_used, clippy::expect_used)]
use zerolog_uring::{Rotation, RotationStep};

#[test]
fn reopen_retry_keeps_old_descriptor() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("zl-rotation-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("events");
    let mut rotation = Rotation::open(&path, 1, 2, false)?;
    let old = rotation.current_fd();
    rotation.record_written(9);
    assert!(rotation.begin());
    assert_eq!(rotation.step(), RotationStep::Unlink);
    rotation.complete_step(-libc::ENOENT)?;
    assert_eq!(rotation.step(), RotationStep::Rename(1));
    rotation.complete_step(-libc::ENOENT)?;
    assert_eq!(rotation.step(), RotationStep::RenameBase);
    rotation.complete_step(0)?;
    assert!(rotation.reopen_pending());
    assert!(rotation.complete_step(-libc::EACCES).is_err());
    assert_eq!(rotation.step(), RotationStep::Open);
    assert!(std::rc::Rc::ptr_eq(&old, &rotation.current_fd()));
    let replacement =
        std::fs::OpenOptions::new().create(true).append(true).open(directory.join("new"))?;
    rotation.opened(replacement.into())?;
    assert_eq!(rotation.step(), RotationStep::Idle);
    assert_eq!(rotation.current_size(), 0);
    assert!(!rotation.reopen_pending());
    assert!(!std::rc::Rc::ptr_eq(&old, &rotation.current_fd()));
    drop(rotation);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

use std::{
    cell::Cell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

use liburing::{Proactor, ProactorConfig, SubsystemId};
use zerolog_core::{
    ConsumerCore, ConsumerLease, DrainBudget, DrainFlags, DrainReport, FrozenSlab, ZeroLogError,
};
use zerolog_uring::{OwnedConsumer, ProactorLogSink, SinkConfig};
struct Batches {
    pending: VecDeque<FrozenSlab>,
    written: Rc<Cell<u64>>,
}
impl ConsumerCore for Batches {
    fn drain(&mut self, now: i64, _: DrainBudget) -> DrainReport {
        let mut flags = DrainFlags::new(0);
        if !self.pending.is_empty() {
            flags |= DrainFlags::OUTPUT_READY;
        }
        DrainReport { next_deadline_ns: now + 1000000, flags }
    }
    fn freeze_output(&mut self) -> Option<FrozenSlab> {
        self.pending.pop_front()
    }
    fn complete_output(&mut self, mut slab: FrozenSlab, written: u32) -> Option<FrozenSlab> {
        self.written.set(self.written.get() + written as u64);
        if slab.advance(written) { None } else { Some(slab) }
    }
    fn write_failed(&mut self, slab: FrozenSlab, _: ZeroLogError) {
        self.pending.push_front(slab);
    }
    fn has_unflushed(&self) -> bool {
        !self.pending.is_empty()
    }
    fn flush_sync(&mut self) -> Result<(), ZeroLogError> {
        self.pending.clear();
        Ok(())
    }
}
#[test]
fn rotation_separates_completed_batches() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("zl-async-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let base = directory.join("events");
    let rotation = Rotation::open(&base, 1, 2, true)?;
    let written = Rc::new(Cell::new(0));
    let core = Batches {
        pending: VecDeque::from([
            FrozenSlab::new(Box::from(&b"one\n"[..]), 4),
            FrozenSlab::new(Box::from(&b"two\n"[..]), 4),
        ]),
        written: written.clone(),
    };
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
        rotation,
        SinkConfig::default(),
    )?;
    let deadline = Instant::now() + Duration::from_secs(3);
    while written.get() != 8
        || sink.target().step() != RotationStep::Idle
        || sink.target().current_size() != 0
    {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut sink)?;
        assert!(Instant::now() < deadline, "rotation stalled");
    }
    sink.begin_shutdown();
    sink.kick(&mut ring)?;
    while !sink.is_quiescent() {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut sink)?;
        assert!(Instant::now() < deadline);
    }
    ring.shutdown_with(&mut sink)?;
    assert_eq!(std::fs::read(directory.join("events.1"))?, b"two\n");
    assert_eq!(std::fs::read(directory.join("events.2"))?, b"one\n");
    assert_eq!(std::fs::read(&base)?, b"");
    drop(sink);
    std::fs::remove_dir_all(directory)?;
    native_open_failure_retry()?;
    Ok(())
}

struct BlockReopen {
    sink: ProactorLogSink<Batches>,
    base: std::path::PathBuf,
    injected: bool,
}
impl liburing::CompletionDispatcher for BlockReopen {
    fn dispatch(
        &mut self,
        completion: liburing::Completion,
        submissions: &mut liburing::SubmissionContext<'_>,
    ) -> liburing::Result<()> {
        let is_base_rename = matches!(
            self.sink.write_state(),
            zerolog_uring::WriteRotateFsm::Rotating { key }
                if key.payload() == completion.operation_id.payload()
        ) && self.sink.target().step() == RotationStep::RenameBase;
        if is_base_rename && completion.result == 0 && !self.injected {
            // The kernel has renamed the base; block only the subsequent OPENAT.
            std::fs::create_dir(&self.base)
                .map_err(|_| liburing::UringError::InvalidConfig("failed to block reopen"))?;
            self.injected = true;
        }
        self.sink.dispatch(completion, submissions)
    }
}
fn native_open_failure_retry() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::temp_dir().join(format!("zl-retry-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let base = directory.join("events");
    let rotation = Rotation::open(&base, 1, 2, false)?;
    let old = rotation.current_fd();
    let core = Batches {
        pending: VecDeque::from([FrozenSlab::new(Box::from(&b"keep\n"[..]), 5)]),
        written: Rc::new(Cell::new(0)),
    };
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
        rotation,
        SinkConfig::default(),
    )?;
    let mut dispatcher = BlockReopen { sink, base: base.clone(), injected: false };
    let deadline = Instant::now() + Duration::from_secs(3);
    while dispatcher.sink.last_error().is_none() {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut dispatcher)?;
        assert!(Instant::now() < deadline, "OPENAT failure stalled");
    }
    assert!(dispatcher.injected);
    assert!(matches!(
        dispatcher.sink.last_error(),
        Some(ZeroLogError::Rotation { errno: libc::EISDIR, .. })
    ));
    assert!(dispatcher.sink.target().reopen_pending());
    assert_eq!(dispatcher.sink.target().step(), RotationStep::Open);
    assert!(Rc::ptr_eq(&old, &dispatcher.sink.target().current_fd()));
    assert_eq!(std::fs::read(directory.join("events.1"))?, b"keep\n");
    std::fs::remove_dir(&base)?;
    dispatcher.sink.retry(&mut ring)?;
    while dispatcher.sink.target().step() != RotationStep::Idle {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut dispatcher)?;
        assert!(Instant::now() < deadline, "OPENAT retry stalled");
    }
    assert!(!dispatcher.sink.target().reopen_pending());
    assert!(!Rc::ptr_eq(&old, &dispatcher.sink.target().current_fd()));
    assert_eq!(std::fs::read(directory.join("events.1"))?, b"keep\n");
    assert!(!directory.join("events.2").exists(), "retry repeated namespace rotation");
    dispatcher.sink.begin_shutdown();
    dispatcher.sink.kick(&mut ring)?;
    while !dispatcher.sink.is_quiescent() {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut dispatcher)?;
        assert!(Instant::now() < deadline);
    }
    ring.shutdown_with(&mut dispatcher)?;
    drop(dispatcher);
    drop(old);
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

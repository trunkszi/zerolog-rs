#![deny(clippy::unwrap_used, clippy::expect_used)]
use std::{
    cell::Cell,
    collections::VecDeque,
    rc::Rc,
    time::{Duration, Instant},
};

use liburing::{CompletionDispatcher, Proactor, ProactorConfig, SubsystemId, define_subsystems};
use zerolog_core::{
    ConsumerCore, ConsumerLease, DrainBudget, DrainFlags, DrainReport, FrozenSlab, ZeroLogError,
};
use zerolog_uring::{OwnedConsumer, ProactorLogSink, Rotation, SinkConfig};

struct FakeConsumer {
    pending: VecDeque<FrozenSlab>,
    work: Rc<Cell<u32>>,
    completed: Rc<Cell<u64>>,
    sync: Option<std::fs::File>,
}
impl ConsumerCore for FakeConsumer {
    fn drain(&mut self, now_ns: i64, budget: DrainBudget) -> DrainReport {
        let drained = self.work.get().min(budget.max_records);
        self.work.set(self.work.get() - drained);
        let mut flags = DrainFlags::new(0);
        if self.work.get() > 0 {
            flags |= DrainFlags::MORE_WORK;
        }
        if !self.pending.is_empty() {
            flags |= DrainFlags::OUTPUT_READY;
        }
        DrainReport { next_deadline_ns: now_ns + 10_000_000, flags }
    }
    fn freeze_output(&mut self) -> Option<FrozenSlab> {
        self.pending.pop_front()
    }
    fn complete_output(&mut self, mut slab: FrozenSlab, written: u32) -> Option<FrozenSlab> {
        self.completed.set(self.completed.get() + written as u64);
        if slab.advance(written) { None } else { Some(slab) }
    }
    fn write_failed(&mut self, slab: FrozenSlab, _error: ZeroLogError) {
        self.pending.push_front(slab);
    }
    fn has_unflushed(&self) -> bool {
        !self.pending.is_empty()
    }
    fn flush_sync(&mut self) -> Result<(), ZeroLogError> {
        if let Some(file) = &mut self.sync {
            use std::io::Write;
            while let Some(slab) = self.pending.pop_front() {
                file.write_all(slab.active_slice()).map_err(|error| {
                    ZeroLogError::Write(error.raw_os_error().unwrap_or(libc::EIO))
                })?;
                self.completed.set(self.completed.get() + slab.len() as u64);
            }
            file.flush()
                .map_err(|error| ZeroLogError::Write(error.raw_os_error().unwrap_or(libc::EIO)))?;
        } else {
            self.pending.clear();
        }
        Ok(())
    }
}

define_subsystems! { enum Routes { Logs = 1, Peer = 2 } }
#[test]
fn multiplex_and_shutdown() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serial();
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let path = std::env::temp_dir().join(format!("zl-sink-{}.log", std::process::id()));
    let file = std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(&path)?;
    let completed = Rc::new(Cell::new(0));
    let work = Rc::new(Cell::new(20));
    let core = FakeConsumer {
        pending: VecDeque::from([
            FrozenSlab::new(Box::from(&b"alpha\n"[..]), 6),
            FrozenSlab::new(Box::from(&b"beta\n"[..]), 5),
        ]),
        work: work.clone(),
        completed: completed.clone(),
        sync: None,
    };
    let owner = OwnedConsumer::new(core, ConsumerLease::try_acquire()?);
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        owner,
        Rotation::from_fd(file.into()),
        SinkConfig { budget: DrainBudget { max_records: 2, max_bytes: 64 }, ..Default::default() },
    )?;
    ring.prepare_keyed_sqe_with(SubsystemId(2), |sqe| {
        // SAFETY: NOP references no external operands.
        unsafe { liburing::ops::prep_nop(sqe) };
    })?;
    let mut peer = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while completed.get() < 11 || !peer {
        let mut state = (&mut sink, &mut peer);
        let mut dispatcher =
            Routes::dispatcher(&mut state, |route, state, cqe, submissions| match route {
                Routes::Logs => state.0.dispatch(cqe, submissions),
                Routes::Peer => {
                    assert!(work.get() > 0, "sibling must run before all budget slices finish");
                    submissions.claim_completion(cqe)?;
                    *state.1 = true;
                    Ok(())
                }
            });
        ring.wait_completions(1, Some(Duration::from_millis(100)), &mut dispatcher)?;
        assert!(Instant::now() < deadline, "logger stalled");
    }
    let waiting = sink.wake_state();
    assert!(matches!(waiting, zerolog_uring::WakeFsm::Waiting { .. }));
    zerolog_core::contract::ring_doorbell(true);
    while sink.wake_state() == waiting {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut sink)?;
        assert!(Instant::now() < deadline, "doorbell target did not complete");
    }
    sink.begin_shutdown();
    sink.kick(&mut ring)?;
    while !sink.is_quiescent() {
        ring.wait_completions(1, Some(Duration::from_millis(100)), &mut sink)?;
        if sink.needs_service() {
            sink.kick(&mut ring)?;
        }
        assert!(Instant::now() < deadline, "shutdown stalled");
    }
    assert_eq!(std::fs::read(&path)?, b"alpha\nbeta\n");
    assert_eq!(sink.owned_targets(), 0);
    assert_eq!(work.get(), 0);
    ring.shutdown_with(&mut sink)?;
    drop(sink);
    assert!(ConsumerLease::try_acquire().is_ok());
    std::fs::remove_file(path)?;
    Ok(())
}

static SERIAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
struct Serial;
fn serial() -> Serial {
    while SERIAL
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::Acquire,
            std::sync::atomic::Ordering::Relaxed,
        )
        .is_err()
    {
        std::thread::yield_now();
    }
    Serial
}
impl Drop for Serial {
    fn drop(&mut self) {
        SERIAL.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[test]
fn worker_drains_both_backends() -> Result<(), Box<dyn std::error::Error>> {
    use zerolog_uring::{UringConsumerThread, WorkerBackend, WorkerConfig};
    let _serial = serial();
    for fallback in [false, true] {
        let path =
            std::env::temp_dir().join(format!("zl-worker-{}-{fallback}", std::process::id()));
        let worker_path = path.clone();
        let factory = move || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(worker_path)
                .map_err(|_| ZeroLogError::Write(libc::EIO))?;
            let target = Rotation::from_fd(
                file.try_clone().map_err(|_| ZeroLogError::Write(libc::EIO))?.into(),
            );
            let core = FakeConsumer {
                pending: VecDeque::from([FrozenSlab::new(Box::from(&b"worker\n"[..]), 7)]),
                work: Rc::new(Cell::new(16)),
                completed: Rc::new(Cell::new(0)),
                sync: None,
            };
            Ok((OwnedConsumer::new(core, ConsumerLease::try_acquire()?), target))
        };
        fn unavailable(_: ProactorConfig) -> liburing::Result<Proactor> {
            Err(liburing::UringError::RingInit(libc::EPERM))
        }
        let worker = if fallback {
            UringConsumerThread::spawn_with_ring_factory(
                factory,
                WorkerConfig::default(),
                unavailable,
            )?
        } else {
            UringConsumerThread::spawn(factory, WorkerConfig::default())?
        };
        assert_eq!(
            worker.backend(),
            if fallback {
                WorkerBackend::Futex { startup_errno: libc::EPERM }
            } else {
                WorkerBackend::Uring
            }
        );
        worker.shutdown()?;
        assert_eq!(std::fs::read(&path)?, b"worker\n");
        std::fs::remove_file(path)?;
    }
    Ok(())
}

fn close_sink(
    ring: &mut Proactor,
    sink: &mut ProactorLogSink<FakeConsumer>,
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    sink.begin_shutdown();
    sink.kick(ring)?;
    while !sink.is_quiescent() {
        ring.wait_completions(1, Some(Duration::from_millis(50)), sink)?;
        if sink.needs_service() {
            sink.kick(ring)?;
        }
        assert!(Instant::now() < deadline, "shutdown stalled");
    }
    ring.shutdown_with(sink)?;
    Ok(())
}

#[test]
fn partial_pipe_writes_resume_suffix() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::Read,
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
    };
    let _serial = serial();
    let mut descriptors = [-1; 2];
    // SAFETY: pipe2 initializes both distinct descriptors on success.
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
        0
    );
    // SAFETY: each successful pipe2 descriptor is adopted exactly once.
    let mut reader = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(descriptors[0]) });
    // SAFETY: the write descriptor is distinct and uniquely owned.
    let writer = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    // SAFETY: fcntl changes capacity on this live, exclusively owned pipe descriptor.
    let capacity = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) };
    assert!(capacity > 0);
    let mut bytes: Vec<_> =
        (0..capacity as u32 * 4).map(|i| ((i * 31 + i / 251) % 253) as u8).collect();
    let first_len = bytes.len();
    let first = FrozenSlab::new(bytes.clone().into_boxed_slice(), first_len as u32);
    bytes.extend_from_slice(b"tail\n");
    let completed = Rc::new(Cell::new(0));
    let core = FakeConsumer {
        pending: VecDeque::from([first, FrozenSlab::new(Box::from(&b"tail\n"[..]), 5)]),
        work: Rc::new(Cell::new(0)),
        completed: completed.clone(),
        sync: None,
    };
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
        Rotation::from_fd(writer),
        SinkConfig::default(),
    )?;
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut received = Vec::new();
    let mut scratch = [0u8; 4096];
    let mut saw_short = false;
    while completed.get() < bytes.len() as u64 {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut sink)?;
        saw_short |= completed.get() > 0 && completed.get() < first_len as u64;
        loop {
            match reader.read(&mut scratch) {
                Ok(0) => break,
                Ok(count) => received.extend_from_slice(&scratch[..count]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.into()),
            }
        }
        if let Some(error) = sink.last_error() {
            assert_eq!(error, ZeroLogError::Write(libc::EAGAIN));
            sink.retry(&mut ring)?;
        }
        assert!(Instant::now() < deadline, "pipe write stalled");
    }
    assert!(saw_short);
    assert_eq!(received, bytes);
    close_sink(&mut ring, &mut sink)?;
    Ok(())
}

#[test]
fn authentic_dispatch_and_ring_shutdown() -> Result<(), Box<dyn std::error::Error>> {
    assert_eq!(
        ProactorLogSink::<FakeConsumer>::check_mode(liburing::ProactorMode::IoPoll),
        Err(ZeroLogError::UnsupportedMode("IoPoll")),
    );
    let _serial = serial();
    let core = FakeConsumer {
        pending: VecDeque::new(),
        work: Rc::new(Cell::new(0)),
        completed: Rc::new(Cell::new(0)),
        sync: None,
    };
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
        Rotation::from_fd(std::fs::OpenOptions::new().write(true).open("/dev/null")?.into()),
        SinkConfig::default(),
    )?;
    let mut wrong_ring = Proactor::new(ProactorConfig::default())?;
    wrong_ring.prepare_keyed_sqe_with(SubsystemId(1), |sqe| {
        // SAFETY: pointer-free NOP in exclusive SQE storage.
        unsafe { liburing::ops::prep_nop(sqe) };
    })?;
    wrong_ring.wait_completions(
        1,
        Some(Duration::from_secs(1)),
        &mut |completion, submissions: &mut liburing::SubmissionContext<'_>| {
            assert!(sink.service(submissions).is_err());
            submissions.claim_completion(completion)
        },
    )?;
    wrong_ring.shutdown()?;
    ring.wait_completions(
        1,
        Some(Duration::from_secs(1)),
        &mut |completion: liburing::Completion,
              submissions: &mut liburing::SubmissionContext<'_>| {
            let mut changed = completion;
            changed.result = 43;
            assert!(sink.dispatch(changed, submissions).is_err());
            sink.dispatch(completion, submissions)
        },
    )?;
    assert!(sink.owned_targets() > 0);
    let mut canceled = false;
    ring.shutdown_with(
        &mut |completion: liburing::Completion,
              submissions: &mut liburing::SubmissionContext<'_>| {
            canceled |= completion.result == -libc::ECANCELED;
            sink.dispatch(completion, submissions)
        },
    )?;
    assert!(canceled);
    assert_eq!(sink.owned_targets(), 0);
    assert!(sink.is_quiescent());
    drop(sink);
    assert!(ConsumerLease::try_acquire().is_ok());
    Ok(())
}

#[test]
fn abandoned_targets_poison_child() -> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "ZEROLOG_URING_POISON_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "abandoned_targets_poison_child"])
            .env(CHILD, "1")
            .status()?;
        assert!(status.success());
        return Ok(());
    }
    let core = FakeConsumer {
        pending: VecDeque::from([FrozenSlab::new(Box::from(&b"retained\n"[..]), 9)]),
        work: Rc::new(Cell::new(0)),
        completed: Rc::new(Cell::new(0)),
        sync: None,
    };
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
        Rotation::from_fd(std::fs::OpenOptions::new().write(true).open("/dev/null")?.into()),
        SinkConfig::default(),
    )?;
    ring.wait_completions(1, Some(Duration::from_secs(1)), &mut sink)?;
    assert!(sink.owned_targets() > 0);
    drop(sink);
    assert!(matches!(ConsumerLease::try_acquire(), Err(ZeroLogError::ConsumerPoisoned)));
    ring.shutdown()?;
    Ok(())
}

struct DeadlineConsumer(FakeConsumer);
impl ConsumerCore for DeadlineConsumer {
    fn drain(&mut self, now: i64, budget: DrainBudget) -> DrainReport {
        if now < 3_000_000 {
            DrainReport { next_deadline_ns: 3_000_000, flags: DrainFlags::new(0) }
        } else {
            self.0.drain(now, budget)
        }
    }
    fn freeze_output(&mut self) -> Option<FrozenSlab> {
        self.0.freeze_output()
    }
    fn complete_output(&mut self, slab: FrozenSlab, written: u32) -> Option<FrozenSlab> {
        self.0.complete_output(slab, written)
    }
    fn write_failed(&mut self, slab: FrozenSlab, error: ZeroLogError) {
        self.0.write_failed(slab, error);
    }
    fn has_unflushed(&self) -> bool {
        self.0.has_unflushed()
    }
    fn flush_sync(&mut self) -> Result<(), ZeroLogError> {
        self.0.flush_sync()
    }
}
#[test]
fn deadline_flushes_without_new_arrivals() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serial();
    let path = std::env::temp_dir().join(format!("zl-deadline-{}", std::process::id()));
    let file = std::fs::File::create(&path)?;
    let completed = Rc::new(Cell::new(0));
    let core = DeadlineConsumer(FakeConsumer {
        pending: VecDeque::from([FrozenSlab::new(Box::from(&b"late\n"[..]), 5)]),
        work: Rc::new(Cell::new(0)),
        completed: completed.clone(),
        sync: None,
    });
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
        Rotation::from_fd(file.into()),
        SinkConfig::default(),
    )?;
    let bound = Instant::now() + Duration::from_secs(3);
    while completed.get() != 5 {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut sink)?;
        assert!(Instant::now() < bound, "deadline stranded pending output");
    }
    sink.begin_shutdown();
    sink.kick(&mut ring)?;
    while !sink.is_quiescent() {
        ring.wait_completions(1, Some(Duration::from_millis(50)), &mut sink)?;
        assert!(Instant::now() < bound);
    }
    ring.shutdown_with(&mut sink)?;
    assert_eq!(std::fs::read(&path)?, b"late\n");
    drop(sink);
    std::fs::remove_file(path)?;
    Ok(())
}

struct IdleConsumer;
impl ConsumerCore for IdleConsumer {
    fn drain(&mut self, _: i64, _: DrainBudget) -> DrainReport {
        DrainReport { next_deadline_ns: i64::MAX, flags: DrainFlags::new(0) }
    }
    fn freeze_output(&mut self) -> Option<FrozenSlab> {
        None
    }
    fn complete_output(&mut self, _: FrozenSlab, _: u32) -> Option<FrozenSlab> {
        None
    }
    fn write_failed(&mut self, _: FrozenSlab, _: ZeroLogError) {}
    fn has_unflushed(&self) -> bool {
        false
    }
    fn flush_sync(&mut self) -> Result<(), ZeroLogError> {
        Ok(())
    }
}
#[test]
fn idle_drop_releases_lease_child() -> Result<(), Box<dyn std::error::Error>> {
    const CHILD: &str = "ZEROLOG_IDLE_DROP_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "idle_drop_releases_lease_child"])
            .env(CHILD, "1")
            .status()?;
        assert!(status.success());
        return Ok(());
    }
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(IdleConsumer, ConsumerLease::try_acquire()?),
        Rotation::from_fd(std::fs::OpenOptions::new().write(true).open("/dev/null")?.into()),
        SinkConfig::default(),
    )?;
    ring.wait_completions(1, Some(Duration::from_secs(1)), &mut sink)?;
    ring.tick(&mut sink)?;
    assert!(matches!(sink.wake_state(), zerolog_uring::WakeFsm::Waiting { .. }));
    assert!(matches!(sink.timer_state(), zerolog_uring::TimerFsm::Disarmed));
    assert!(matches!(sink.write_state(), zerolog_uring::WriteRotateFsm::Idle));
    assert_eq!(sink.owned_targets(), 1);
    drop(sink);
    assert!(ConsumerLease::try_acquire().is_ok(), "static wait poisoned lease");
    assert_eq!(
        zerolog_core::contract::DOORBELL.load(std::sync::atomic::Ordering::Acquire)
            & zerolog_core::contract::DOORBELL_PARKED_BIT,
        0
    );
    ring.shutdown()?;
    Ok(())
}
#[test]
fn transient_write_retries_automatically() -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
    };
    let _serial = serial();
    let mut descriptors = [-1; 2];
    // SAFETY: the array provides two writable descriptor slots.
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
        0
    );
    // SAFETY: successful pipe2 descriptors are each adopted once.
    let mut reader = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(descriptors[0]) });
    // SAFETY: the writer is distinct from the owned reader.
    let mut writer = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(descriptors[1]) });
    // SAFETY: this live pipe accepts F_SETPIPE_SZ.
    let capacity = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) };
    assert!(capacity > 0);
    writer.write_all(&vec![0xa5; capacity as usize])?;
    let completed = Rc::new(Cell::new(0));
    let core = FakeConsumer {
        pending: VecDeque::from([FrozenSlab::new(Box::from(&b"recover\n"[..]), 8)]),
        work: Rc::new(Cell::new(0)),
        completed: completed.clone(),
        sync: None,
    };
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
        Rotation::from_fd(writer.into()),
        SinkConfig { write_flags: libc::RWF_NOWAIT as u32, ..Default::default() },
    )?;
    let bound = Instant::now() + Duration::from_secs(3);
    while sink.last_error().is_none() {
        ring.wait_completions(1, Some(Duration::from_millis(20)), &mut sink)?;
        assert!(Instant::now() < bound);
    }
    assert_eq!(sink.last_error(), Some(ZeroLogError::Write(libc::EAGAIN)));
    assert_eq!(sink.retry_backoff_ns(), 1_000_000);
    assert_eq!(sink.take_last_error(), Some(ZeroLogError::Write(libc::EAGAIN)));
    while sink.retry_backoff_ns() < 2_000_000 {
        ring.wait_completions(1, Some(Duration::from_millis(20)), &mut sink)?;
        assert!(Instant::now() < bound, "retry backoff did not advance");
    }
    assert_eq!(sink.retry_backoff_ns(), 2_000_000);
    assert_eq!(completed.get(), 0);
    assert!(matches!(sink.write_state(), zerolog_uring::WriteRotateFsm::Idle));
    let mut filler = vec![0; capacity as usize];
    reader.read_exact(&mut filler)?;
    assert!(filler.iter().all(|&byte| byte == 0xa5));
    while completed.get() != 8 {
        ring.wait_completions(1, Some(Duration::from_millis(20)), &mut sink)?;
        assert!(Instant::now() < bound, "transient write never retried");
    }
    assert_eq!(sink.retry_backoff_ns(), 0);
    let mut output = [0; 8];
    reader.read_exact(&mut output)?;
    assert_eq!(&output, b"recover\n");
    close_sink(&mut ring, &mut sink)?;
    Ok(())
}

struct ObservedFailures {
    inner: FakeConsumer,
    failures: std::sync::Arc<std::sync::atomic::AtomicU32>,
}
impl ConsumerCore for ObservedFailures {
    fn drain(&mut self, now: i64, budget: DrainBudget) -> DrainReport {
        self.inner.drain(now, budget)
    }
    fn freeze_output(&mut self) -> Option<FrozenSlab> {
        self.inner.freeze_output()
    }
    fn complete_output(&mut self, slab: FrozenSlab, count: u32) -> Option<FrozenSlab> {
        self.inner.complete_output(slab, count)
    }
    fn write_failed(&mut self, slab: FrozenSlab, error: ZeroLogError) {
        self.inner.write_failed(slab, error);
        self.failures.fetch_add(1, std::sync::atomic::Ordering::Release);
    }
    fn has_unflushed(&self) -> bool {
        self.inner.has_unflushed()
    }
    fn flush_sync(&mut self) -> Result<(), ZeroLogError> {
        self.inner.flush_sync()
    }
}
#[test]
fn worker_survives_transient_write() -> Result<(), Box<dyn std::error::Error>> {
    worker_transient(false)
}
#[test]
fn fallback_survives_transient_write() -> Result<(), Box<dyn std::error::Error>> {
    worker_transient(true)
}
fn worker_transient(fallback: bool) -> Result<(), Box<dyn std::error::Error>> {
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd, OwnedFd},
    };
    let _serial = serial();
    let mut descriptors = [-1; 2];
    // SAFETY: two writable slots receive distinct descriptors.
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) },
        0
    );
    // SAFETY: each initialized descriptor is adopted exactly once.
    let mut reader = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(descriptors[0]) });
    // SAFETY: this descriptor differs from the reader.
    let mut writer = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(descriptors[1]) });
    // SAFETY: fcntl operates on a live owned pipe.
    let capacity = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) };
    assert!(capacity > 0);
    writer.write_all(&vec![0x7a; capacity as usize])?;
    let failures = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let worker_failures = failures.clone();
    let mut config = zerolog_uring::WorkerConfig::default();
    config.sink.write_flags = libc::RWF_NOWAIT as u32;
    let factory = move || {
        let core = ObservedFailures {
            inner: FakeConsumer {
                pending: VecDeque::from([FrozenSlab::new(Box::from(&b"live\n"[..]), 5)]),
                work: Rc::new(Cell::new(0)),
                completed: Rc::new(Cell::new(0)),
                sync: None,
            },
            failures: worker_failures,
        };
        Ok((
            OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
            Rotation::from_fd(writer.into()),
        ))
    };
    fn unavailable(_: ProactorConfig) -> liburing::Result<Proactor> {
        Err(liburing::UringError::RingInit(libc::EPERM))
    }
    let worker = if fallback {
        zerolog_uring::UringConsumerThread::spawn_with_ring_factory(factory, config, unavailable)?
    } else {
        zerolog_uring::UringConsumerThread::spawn(factory, config)?
    };
    let bound = Instant::now() + Duration::from_secs(3);
    while failures.load(std::sync::atomic::Ordering::Acquire) == 0 {
        assert!(Instant::now() < bound, "worker never observed transient failure");
        std::thread::yield_now();
    }
    let mut filler = vec![0; capacity as usize];
    reader.read_exact(&mut filler)?;
    assert!(filler.iter().all(|&byte| byte == 0x7a));
    let mut received = Vec::new();
    while received.len() < 5 {
        let mut bytes = [0; 5];
        match reader.read(&mut bytes) {
            Ok(count) => received.extend_from_slice(&bytes[..count]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::yield_now()
            }
            Err(error) => return Err(error.into()),
        }
        assert!(Instant::now() < bound, "worker exited on recoverable error");
    }
    assert_eq!(received, b"live\n");
    worker.shutdown()?;
    Ok(())
}
#[test]
fn cancellation_flushes_restored_output() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serial();
    let path = std::env::temp_dir().join(format!("zl-cancel-flush-{}", std::process::id()));
    let file = std::fs::File::create(&path)?;
    let completed = Rc::new(Cell::new(0));
    let core = FakeConsumer {
        pending: VecDeque::from([FrozenSlab::new(Box::from(&b"final\n"[..]), 6)]),
        work: Rc::new(Cell::new(0)),
        completed: completed.clone(),
        sync: None,
    };
    let mut ring = Proactor::new(ProactorConfig::default())?;
    let mut sink = ProactorLogSink::new(
        &mut ring,
        SubsystemId(1),
        OwnedConsumer::new(core, ConsumerLease::try_acquire()?),
        Rotation::from_fd(file.into()),
        SinkConfig::default(),
    )?;
    ring.wait_completions(1, Some(Duration::from_secs(1)), &mut sink)?;
    assert!(matches!(sink.write_state(), zerolog_uring::WriteRotateFsm::Writing { .. }));
    sink.begin_shutdown();
    ring.shutdown_with(&mut sink)?;
    assert!(sink.is_quiescent());
    assert_eq!(completed.get(), 6);
    assert_eq!(std::fs::read(&path)?, b"final\n");
    drop(sink);
    std::fs::remove_file(path)?;
    Ok(())
}

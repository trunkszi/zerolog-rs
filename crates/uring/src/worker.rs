#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Explicit standalone ownership; consumer construction happens on the worker thread.
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use liburing::{Proactor, ProactorConfig, SubsystemId, UringError};
use zerolog_core::{
    ConsumerCore, DrainBudget, DrainFlags, DrainReport, ZeroLogError,
    contract::{DOORBELL, DOORBELL_PARKED_BIT, futex_wait_doorbell_timeout, ring_doorbell},
};

use crate::{
    Rotation, RotationStep,
    sink::{OwnedConsumer, ProactorLogSink, SinkConfig, map_uring},
};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerBackend {
    Uring,
    Futex { startup_errno: i32 },
}
#[derive(Debug, Clone, Copy)]
pub struct WorkerConfig {
    pub ring: ProactorConfig,
    pub sink: SinkConfig,
    pub shutdown_timeout: Duration,
}
impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            ring: ProactorConfig::default(),
            sink: SinkConfig::default(),
            shutdown_timeout: Duration::from_secs(5),
        }
    }
}
pub struct UringConsumerThread {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<Result<(), ZeroLogError>>>,
    backend: WorkerBackend,
}
impl UringConsumerThread {
    pub fn backend(&self) -> WorkerBackend {
        self.backend
    }
    pub fn spawn<C, F>(factory: F, config: WorkerConfig) -> Result<Self, ZeroLogError>
    where
        C: ConsumerCore + 'static,
        F: FnOnce() -> Result<(OwnedConsumer<C>, Rotation), ZeroLogError> + Send + 'static,
    {
        Self::spawn_with_ring_factory(factory, config, Proactor::new)
    }
    /// Injects only ring construction, so startup unavailability is deterministic in tests.
    pub fn spawn_with_ring_factory<C, F>(
        factory: F,
        config: WorkerConfig,
        make_ring: fn(ProactorConfig) -> liburing::Result<Proactor>,
    ) -> Result<Self, ZeroLogError>
    where
        C: ConsumerCore + 'static,
        F: FnOnce() -> Result<(OwnedConsumer<C>, Rotation), ZeroLogError> + Send + 'static,
    {
        ProactorLogSink::<C>::check_mode(config.ring.mode)?;
        if config.sink.budget.max_records == 0
            || config.sink.budget.max_bytes == 0
            || config.sink.idle_interval_ns == Some(0)
            || config.shutdown_timeout.is_zero()
            || config.ring.operation_capacity < 5
            || config.ring.staged_capacity < 5
        {
            return Err(ZeroLogError::InvalidConfig("invalid worker budget or timeout"));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let (ready_tx, ready_rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("zerolog-uring".into())
            .spawn(move || {
                let (owner, target) = match factory() {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return Err(error);
                    }
                };
                match make_ring(config.ring) {
                    Ok(mut ring) => {
                        let mut sink = match ProactorLogSink::new(
                            &mut ring,
                            SubsystemId(1),
                            owner,
                            target,
                            config.sink,
                        ) {
                            Ok(sink) => sink,
                            Err(error) => {
                                let _ = ready_tx.send(Err(error));
                                return Err(error);
                            }
                        };
                        let _ = ready_tx.send(Ok(WorkerBackend::Uring));
                        run_ring(&mut ring, &mut sink, &worker_stop, config.shutdown_timeout)
                    }
                    Err(error) => {
                        if let Some(errno) = fallback_errno(error) {
                            let _ =
                                ready_tx.send(Ok(WorkerBackend::Futex { startup_errno: errno }));
                            run_futex(owner, target, &worker_stop, config)
                        } else {
                            let error = map_uring(error);
                            let _ = ready_tx.send(Err(error));
                            Err(error)
                        }
                    }
                }
            })
            .map_err(|error| ZeroLogError::Write(error.raw_os_error().unwrap_or(libc::EIO)))?;
        let backend = match ready_rx.recv() {
            Ok(Ok(backend)) => backend,
            Ok(Err(error)) => {
                let _ = handle.join();
                return Err(error);
            }
            Err(_) => {
                let _ = handle.join();
                return Err(ZeroLogError::InvalidConfig("worker failed before startup"));
            }
        };
        Ok(Self { stop, handle: Some(handle), backend })
    }
    pub fn shutdown(mut self) -> Result<(), ZeroLogError> {
        self.stop.store(true, Ordering::Release);
        ring_doorbell(false);
        if let Some(handle) = self.handle.take() {
            if handle.thread().id() == thread::current().id() {
                return Err(ZeroLogError::SelfStop);
            }
            return match handle.join() {
                Ok(result) => result,
                Err(_) => Err(ZeroLogError::InvalidConfig("logger worker panicked")),
            };
        }
        Ok(())
    }
}
impl Drop for UringConsumerThread {
    fn drop(&mut self) {
        // Drop requests stop without blocking an application destructor on I/O.
        self.stop.store(true, Ordering::Release);
        ring_doorbell(false);
    }
}
fn fallback_errno(error: UringError) -> Option<i32> {
    match error {
        UringError::RingInit(errno) | UringError::Syscall { errno, .. } => Some(errno),
        UringError::UnsupportedKernelVersion { .. } | UringError::UnsupportedCapability(_) => {
            Some(libc::ENOSYS)
        }
        _ => None,
    }
}
fn run_ring<C: ConsumerCore>(
    ring: &mut Proactor,
    sink: &mut ProactorLogSink<C>,
    stop: &AtomicBool,
    timeout: Duration,
) -> Result<(), ZeroLogError> {
    let result = run_ring_loop(ring, sink, stop, timeout);
    if !sink.is_quiescent() {
        let _ = ring.shutdown_with(sink);
    }
    sink.flush_pending_sync();
    result
}
fn run_ring_loop<C: ConsumerCore>(
    ring: &mut Proactor,
    sink: &mut ProactorLogSink<C>,
    stop: &AtomicBool,
    timeout: Duration,
) -> Result<(), ZeroLogError> {
    let mut stopping_since = None;
    loop {
        if stop.load(Ordering::Acquire) {
            sink.begin_shutdown();
            stopping_since.get_or_insert_with(Instant::now);
        }
        if sink.is_quiescent() {
            return ring.shutdown_with(sink).map_err(map_uring);
        }
        if stopping_since.is_some_and(|since: Instant| since.elapsed() >= timeout) {
            ring.shutdown_with(sink).map_err(map_uring)?;
            return Err(ZeroLogError::Write(libc::ETIMEDOUT));
        }
        if sink.needs_service()
            && let Err(error) = sink.kick(ring)
            && error != UringError::SqRingFull
        {
            return Err(map_uring(error));
        }
        if ring.mode().is_event_driven() {
            let wait = stopping_since.map(|_| Duration::from_millis(10));
            ring.wait_completions(1, wait, sink).map_err(map_uring)?;
        } else {
            ring.tick(sink).map_err(map_uring)?;
        }
    }
}
fn run_futex<C: ConsumerCore>(
    mut owner: OwnedConsumer<C>,
    mut target: Rotation,
    stop: &AtomicBool,
    config: WorkerConfig,
) -> Result<(), ZeroLogError> {
    let origin = Instant::now();
    let _parking = ParkingGuard;
    let mut retry = SyncRetry::default();
    let mut stopping_since = None;
    loop {
        let expected =
            DOORBELL.fetch_or(DOORBELL_PARKED_BIT, Ordering::SeqCst) | DOORBELL_PARKED_BIT;
        let now = origin.elapsed().as_nanos().min(i64::MAX as u128) as i64;
        let stopping = stop.load(Ordering::Acquire);
        if stopping {
            let since = stopping_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= config.shutdown_timeout {
                return Err(ZeroLogError::Write(libc::ETIMEDOUT));
            }
        }
        let report = drain_and_write_sync(
            owner.core_mut(),
            &mut target,
            now,
            config.sink.budget,
            stopping,
            &mut retry,
        );
        if stop.load(Ordering::Acquire)
            && !report.flags.test(DrainFlags::MORE_WORK)
            && !owner.core_mut().has_unflushed()
            && target.step() == RotationStep::Idle
        {
            DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, Ordering::SeqCst);
            return Ok(());
        }
        if report.flags.test(DrainFlags::MORE_WORK) && retry.write.deadline_ns.is_none() {
            DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, Ordering::SeqCst);
            continue;
        }
        let next = retry.next_deadline(report.next_deadline_ns);
        let remaining =
            (next >= 0 && next != i64::MAX).then(|| next.saturating_sub(now).max(1) as u64);
        let wait = match (remaining, config.sink.idle_interval_ns) {
            (Some(deadline), Some(idle)) => Some(deadline.min(idle)),
            (deadline, None) => deadline,
            (None, idle) => idle,
        };
        let wait = if stopping { Some(wait.unwrap_or(10_000_000).min(10_000_000)) } else { wait };
        let outcome = futex_wait_doorbell_timeout(expected, wait);
        DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, Ordering::SeqCst);
        if let Err(error) = outcome
            && !matches!(error.raw_os_error(), Some(code) if code==libc::EAGAIN || code==libc::EINTR || code==libc::ETIMEDOUT)
        {
            return Err(ZeroLogError::Write(error.raw_os_error().unwrap_or(libc::EIO)));
        }
    }
}

#[derive(Default)]
struct RetryWindow {
    backoff_ns: u64,
    deadline_ns: Option<i64>,
}
impl RetryWindow {
    fn ready(&self, now: i64, force: bool) -> bool {
        force || self.deadline_ns.is_none_or(|deadline| now >= deadline)
    }
    fn failed(&mut self, now: i64) {
        self.backoff_ns = self.backoff_ns.saturating_mul(2).clamp(1_000_000, 1_000_000_000);
        self.deadline_ns = Some(now.saturating_add(self.backoff_ns as i64));
    }
}
#[derive(Default)]
struct SyncRetry {
    write: RetryWindow,
    rotation: RetryWindow,
    last_error: Option<ZeroLogError>,
}
impl SyncRetry {
    fn rotate(&mut self, target: &mut Rotation, now: i64, force: bool) {
        if !self.rotation.ready(now, force) {
            return;
        }
        match target.rotate_sync() {
            Ok(()) => self.rotation = RetryWindow::default(),
            Err(error) => {
                self.last_error = Some(error);
                self.rotation.failed(now);
            }
        }
    }
    fn next_deadline(&self, drain: i64) -> i64 {
        if let Some(write) = self.write.deadline_ns {
            return write;
        }
        match self.rotation.deadline_ns {
            Some(rotation) if drain < 0 || drain == i64::MAX => rotation,
            Some(rotation) => rotation.min(drain),
            None => drain,
        }
    }
}
fn drain_and_write_sync<C: ConsumerCore>(
    core: &mut C,
    target: &mut Rotation,
    now: i64,
    budget: DrainBudget,
    force_flush: bool,
    retry: &mut SyncRetry,
) -> DrainReport {
    let report = core.drain(now, budget);
    if !retry.write.ready(now, force_flush) {
        return report;
    }
    let mut wrote_slab = false;
    if force_flush
        || report.flags.test(DrainFlags::OUTPUT_READY)
        || report.flags.test(DrainFlags::FLUSH_DUE)
        || core.has_unflushed()
    {
        while let Some(slab) = core.freeze_output() {
            let empty = slab.is_empty();
            match target.write_slab_sync(core, slab) {
                Ok(()) => {
                    wrote_slab = true;
                    retry.write = RetryWindow::default();
                    retry.rotate(target, now, force_flush);
                }
                Err(error) => {
                    retry.last_error = Some(error);
                    retry.write.failed(now);
                    break;
                }
            }
            if empty || !force_flush {
                break;
            }
        }
    }
    if !wrote_slab && retry.write.deadline_ns.is_none() && target.step() != RotationStep::Idle {
        retry.rotate(target, now, force_flush);
    }
    report
}

struct ParkingGuard;
impl Drop for ParkingGuard {
    fn drop(&mut self) {
        DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, Ordering::SeqCst);
    }
}

#[cfg(test)]
#[path = "../tests/unit/worker.rs"]
mod tests;

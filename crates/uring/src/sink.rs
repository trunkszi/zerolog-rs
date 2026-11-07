#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Thread-affine completion-driven output with target-CQE ownership.
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use liburing::{
    Completion, CompletionDispatcher, OperationId, Proactor, ProactorMode, SubmissionContext,
    SubsystemId, UringError,
    operation_slab::OperationKey,
    ops::{self, Timeout},
};
use uarch::prelude::*;
use zerolog_core::{
    ConsumerCore, ConsumerLease, DrainBudget, DrainFlags, DrainReport, FrozenSlab, ZeroLogError,
    contract::{DOORBELL, DOORBELL_PARKED_BIT, DOORBELL_URGENT_BIT, ring_doorbell},
};

use crate::rotation::{Rotation, RotationStep};

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleFsm {
    Running,
    Draining,
    Quiescent,
}
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeFsm {
    Disarmed,
    Waiting { key: OperationKey },
}
#[repr(u8)]
pub enum TimerFsm {
    Disarmed,
    Armed { key: OperationKey, timeout: Timeout, deadline_ns: i64 },
}
#[repr(u8)]
pub enum WriteRotateFsm {
    Idle,
    Ready(FrozenSlab),
    Writing { key: OperationKey, slab: FrozenSlab },
    Rotating { key: OperationKey },
}
bitword! {
    struct SinkFlags(u32) { [0] FUTEX_SUPPORTED, [1] MORE_WORK, [2] RETRY, [3] RETIME, [4] RING_CLOSING, [5] ROTATION_RETRY }
}
assert_layout!(SinkFlags, size = 4, align = 4);

#[derive(Debug, Clone, Copy)]
pub struct SinkConfig {
    pub budget: DrainBudget,
    /// Per-write RWF_* flags; RWF_NOWAIT exposes readiness failures to the backoff policy.
    pub write_flags: u32,
    /// None requires a consumer-side deep-park protocol; unsupported futex still polls.
    pub idle_interval_ns: Option<u64>,
}
impl Default for SinkConfig {
    fn default() -> Self {
        Self {
            budget: DrainBudget { max_records: 256, max_bytes: 65536 },
            write_flags: 0,
            idle_interval_ns: None,
        }
    }
}

/// A consumer with either an external lease or an explicit embedded-lease poison hook.
pub struct OwnedConsumer<C> {
    core: Option<C>,
    lease: Option<ConsumerLease>,
    poison: Option<fn(C)>,
}
impl<C: ConsumerCore> OwnedConsumer<C> {
    pub fn new(core: C, lease: ConsumerLease) -> Self {
        Self { core: Some(core), lease: Some(lease), poison: None }
    }
    /// `poison` must permanently retain or poison the lease embedded in `core`.
    pub fn embedded(core: C, poison: fn(C)) -> Self {
        Self { core: Some(core), lease: None, poison: Some(poison) }
    }
    pub(crate) fn core_mut(&mut self) -> &mut C {
        match self.core.as_mut() {
            Some(core) => core,
            None => unreachable!("consumer already transferred"),
        }
    }
    fn poison(mut self) {
        if let Some(lease) = self.lease.take() {
            lease.poison_on_abandon();
        }
        if let (Some(poison), Some(core)) = (self.poison, self.core.take()) {
            poison(core);
        }
    }
}

/// Drive `kick` after host turns that free staging credit when `needs_service` is true.
/// Quiesce producers before shutdown if every final record must be included.
pub struct ProactorLogSink<C: ConsumerCore> {
    consumer: Option<OwnedConsumer<C>>,
    target: Option<Rotation>,
    subsystem: SubsystemId,
    owner_id: u64,
    config: SinkConfig,
    origin: Instant,
    lifecycle: LifecycleFsm,
    wake: WakeFsm,
    timer: TimerFsm,
    write: WriteRotateFsm,
    continuation: Option<OperationKey>,
    timer_update: Option<OperationKey>,
    flags: SinkFlags,
    last_error: Option<ZeroLogError>,
    desired_deadline: Option<i64>,
    retry_backoff_ns: u64,
    retry_deadline: Option<i64>,
}
impl<C: ConsumerCore> ProactorLogSink<C> {
    pub fn check_mode(mode: ProactorMode) -> Result<(), ZeroLogError> {
        match mode {
            ProactorMode::IoPoll => Err(ZeroLogError::UnsupportedMode("IoPoll")),
            ProactorMode::SqPollIoPoll(_) => Err(ZeroLogError::UnsupportedMode("SqPollIoPoll")),
            _ => Ok(()),
        }
    }
    pub fn new(
        ring: &mut Proactor,
        subsystem: SubsystemId,
        consumer: OwnedConsumer<C>,
        target: Rotation,
        config: SinkConfig,
    ) -> Result<Self, ZeroLogError> {
        Self::check_mode(ring.mode())?;
        if subsystem == SubsystemId::INTERNAL
            || config.budget.max_records == 0
            || config.budget.max_bytes == 0
            || config.idle_interval_ns == Some(0)
        {
            return Err(ZeroLogError::InvalidConfig("invalid logger subsystem or budget"));
        }
        let mut flags = SinkFlags::new(0);
        flags |= SinkFlags::FUTEX_SUPPORTED;
        let mut sink = Self {
            consumer: Some(consumer),
            target: Some(target),
            subsystem,
            owner_id: ring.owner_id(),
            config,
            origin: Instant::now(),
            lifecycle: LifecycleFsm::Running,
            wake: WakeFsm::Disarmed,
            timer: TimerFsm::Disarmed,
            write: WriteRotateFsm::Idle,
            continuation: None,
            timer_update: None,
            flags,
            last_error: None,
            desired_deadline: None,
            retry_backoff_ns: 0,
            retry_deadline: None,
        };
        // Bootstrap contains no borrowed operands; all DMA preparation occurs in dispatch.
        sink.kick(ring).map_err(map_uring)?;
        Ok(sink)
    }
    pub fn lifecycle(&self) -> LifecycleFsm {
        self.lifecycle
    }
    pub fn wake_state(&self) -> WakeFsm {
        self.wake
    }
    pub fn timer_state(&self) -> &TimerFsm {
        &self.timer
    }
    pub fn write_state(&self) -> &WriteRotateFsm {
        &self.write
    }
    /// The most recent recorded error persists across recovery until taken or explicitly retried.
    pub fn last_error(&self) -> Option<ZeroLogError> {
        self.last_error
    }
    pub fn take_last_error(&mut self) -> Option<ZeroLogError> {
        self.last_error.take()
    }
    pub fn retry_backoff_ns(&self) -> u64 {
        self.retry_backoff_ns
    }
    pub fn target(&self) -> &Rotation {
        match self.target.as_ref() {
            Some(target) => target,
            None => unreachable!("target already transferred"),
        }
    }
    pub fn is_quiescent(&self) -> bool {
        self.lifecycle == LifecycleFsm::Quiescent
    }
    pub fn needs_service(&self) -> bool {
        self.flags.test(SinkFlags::RETRY)
    }
    pub fn owned_targets(&self) -> usize {
        usize::from(self.timer_update.is_some())
            + usize::from(matches!(self.wake, WakeFsm::Waiting { .. }))
            + usize::from(matches!(self.timer, TimerFsm::Armed { .. }))
            + usize::from(matches!(
                self.write,
                WriteRotateFsm::Writing { .. } | WriteRotateFsm::Rotating { .. }
            ))
            + usize::from(self.continuation.is_some())
    }
    pub(crate) fn core_mut(&mut self) -> &mut C {
        match self.consumer.as_mut() {
            Some(owner) => owner.core_mut(),
            None => unreachable!("consumer already transferred"),
        }
    }
    fn target_mut(&mut self) -> &mut Rotation {
        match self.target.as_mut() {
            Some(target) => target,
            None => unreachable!("target already transferred"),
        }
    }
    fn now_ns(&self) -> i64 {
        self.origin.elapsed().as_nanos().min(i64::MAX as u128) as i64
    }
    /// Stages a pointer-free continuation. The caller still owns and drives the ring.
    pub fn kick(&mut self, ring: &mut Proactor) -> liburing::Result<()> {
        if ring.owner_id() != self.owner_id {
            return Err(UringError::InvalidConfig("logger belongs to another proactor"));
        }
        if !self.is_quiescent() && self.continuation.is_none() {
            let key = ring.prepare_keyed_sqe_with(self.subsystem, |sqe| {
                // SAFETY: NOP has no pointer operands; SQE storage is exclusive.
                unsafe { ops::prep_nop(sqe) };
            })?;
            self.continuation = Some(key);
        }
        Ok(())
    }
    pub fn retry(&mut self, ring: &mut Proactor) -> liburing::Result<()> {
        self.last_error = None;
        self.retry_deadline = None;
        self.flags &= !SinkFlags::ROTATION_RETRY;
        self.kick(ring)
    }
    pub fn begin_shutdown(&mut self) {
        if self.lifecycle == LifecycleFsm::Running {
            self.lifecycle = LifecycleFsm::Draining;
            self.flags |= SinkFlags::RETRY;
            ring_doorbell(false);
            DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, Ordering::SeqCst);
        }
    }
    fn key_matches(&self, key: OperationKey) -> bool {
        self.timer_update == Some(key)
            || self.continuation == Some(key)
            || matches!(self.wake, WakeFsm::Waiting { key: k } if k == key)
            || matches!(self.timer, TimerFsm::Armed { key: k, .. } if k == key)
            || matches!(self.write, WriteRotateFsm::Writing { key: k, .. } | WriteRotateFsm::Rotating { key: k } if k == key)
    }
    fn handle_terminal(&mut self, key: OperationKey, result: i32) {
        if result == -libc::ECANCELED {
            self.lifecycle = LifecycleFsm::Draining;
            self.flags |= SinkFlags::RING_CLOSING;
        }
        if self.timer_update == Some(key) {
            self.timer_update = None;
            if result < 0
                && result != -libc::ENOENT
                && result != -libc::EALREADY
                && result != -libc::ECANCELED
            {
                self.last_error = Some(ZeroLogError::Write(result.saturating_neg()));
            }
            return;
        }
        if self.continuation == Some(key) {
            self.continuation = None;
            return;
        }
        if matches!(self.wake, WakeFsm::Waiting { key: k } if k == key) {
            self.wake = WakeFsm::Disarmed;
            DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, Ordering::SeqCst);
            if matches!(result, r if r == -libc::EINVAL || r == -libc::ENOSYS) {
                self.flags &= !SinkFlags::FUTEX_SUPPORTED;
            } else if result < 0
                && !matches!(result, r if r == -libc::EAGAIN || r == -libc::EINTR || r == -libc::ECANCELED)
            {
                self.last_error = Some(ZeroLogError::Write(result.saturating_neg()));
            }
            return;
        }
        if matches!(self.timer, TimerFsm::Armed { key: k, .. } if k == key) {
            self.timer = TimerFsm::Disarmed;
            self.flags &= !SinkFlags::RETIME;
            if result == -libc::ETIME
                && self.retry_deadline.is_some_and(|deadline| self.now_ns() >= deadline)
            {
                self.retry_deadline = None;
            }
            if result < 0 && result != -libc::ETIME && result != -libc::ECANCELED {
                self.last_error = Some(ZeroLogError::Write(result.saturating_neg()));
            }
            return;
        }
        match std::mem::replace(&mut self.write, WriteRotateFsm::Idle) {
            WriteRotateFsm::Writing { slab, .. } => {
                if result <= 0 || result as u32 > slab.len() {
                    let error = if result == 0 {
                        ZeroLogError::WriteZero
                    } else {
                        ZeroLogError::Write(if result < 0 {
                            result.saturating_neg()
                        } else {
                            libc::EIO
                        })
                    };
                    self.output_failed(error);
                    self.core_mut().write_failed(slab, error);
                } else {
                    if !self.rotation_backoff() {
                        self.output_succeeded();
                    }
                    self.target_mut().record_written(result as u32);
                    if let Some(suffix) = self.core_mut().complete_output(slab, result as u32) {
                        self.write = WriteRotateFsm::Ready(suffix);
                    }
                }
            }
            WriteRotateFsm::Rotating { .. } => {
                let outcome = if self.target().step() == RotationStep::Open && result >= 0 {
                    // SAFETY: this authenticated terminal OPENAT CQE transfers one new fd.
                    self.target_mut().opened(unsafe { OwnedFd::from_raw_fd(result) })
                } else {
                    self.target_mut().complete_step(result)
                };
                match outcome {
                    Ok(()) => self.output_succeeded(),
                    Err(error) => self.output_failed(error),
                }
            }
            other => {
                self.write = other;
            }
        }
    }
    fn output_failed(&mut self, error: ZeroLogError) {
        self.last_error = Some(error);
        self.flags =
            self.flags.patch(BitPatch::EMPTY.with_flag(
                SinkFlags::ROTATION_RETRY,
                matches!(error, ZeroLogError::Rotation { .. }),
            ));
        self.retry_backoff_ns =
            self.retry_backoff_ns.saturating_mul(2).clamp(1_000_000, 1_000_000_000);
        self.retry_deadline = Some(self.now_ns().saturating_add(self.retry_backoff_ns as i64));
    }
    fn output_succeeded(&mut self) {
        self.flags &= !SinkFlags::ROTATION_RETRY;
        self.retry_backoff_ns = 0;
        self.retry_deadline = None;
    }
    /// Called only after terminal cancellation has restored ownership of output operands.
    pub(crate) fn flush_pending_sync(&mut self) {
        if matches!(self.write, WriteRotateFsm::Writing { .. } | WriteRotateFsm::Rotating { .. }) {
            return;
        }
        if let WriteRotateFsm::Ready(slab) =
            std::mem::replace(&mut self.write, WriteRotateFsm::Idle)
            && let Err(error) = self.flush_slab_sync(slab)
        {
            self.output_failed(error);
            return;
        }
        while let Some(slab) = self.core_mut().freeze_output() {
            let empty = slab.is_empty();
            if let Err(error) = self.flush_slab_sync(slab) {
                self.output_failed(error);
                break;
            }
            if empty {
                break;
            }
        }
    }
    fn flush_slab_sync(&mut self, slab: FrozenSlab) -> Result<(), ZeroLogError> {
        match (&mut self.consumer, &mut self.target) {
            (Some(owner), Some(target)) => target.write_slab_sync(owner.core_mut(), slab),
            _ => unreachable!("output ownership already transferred"),
        }
    }
    fn write_backoff(&self) -> bool {
        self.retry_deadline.is_some() && !self.flags.test(SinkFlags::ROTATION_RETRY)
    }
    fn rotation_backoff(&self) -> bool {
        self.retry_deadline.is_some() && self.flags.test(SinkFlags::ROTATION_RETRY)
    }
    fn drain_core(&mut self, now: i64) -> Option<DrainReport> {
        if matches!(self.write, WriteRotateFsm::Ready(_)) {
            return None;
        }
        let budget = self.config.budget;
        Some(self.core_mut().drain(now, budget))
    }

    fn try_stage(&mut self, result: liburing::Result<OperationKey>) -> Option<OperationKey> {
        match result {
            Ok(key) => Some(key),
            Err(UringError::SqRingFull) => {
                self.flags |= SinkFlags::RETRY;
                None
            }
            Err(error) => {
                self.last_error = Some(map_uring(error));
                None
            }
        }
    }
    fn stage_nop(&mut self, submissions: &mut SubmissionContext<'_>) {
        if self.continuation.is_none() {
            let result = submissions.prepare_keyed_sqe_with(self.subsystem, |sqe| {
                // SAFETY: initialized SQE, no borrowed NOP operands.
                unsafe { ops::prep_nop(sqe) };
            });
            self.continuation = self.try_stage(result);
        }
    }
    fn expire_timer(&mut self, submissions: &mut SubmissionContext<'_>) {
        if self.timer_update.is_none()
            && let TimerFsm::Armed { key, .. } = self.timer
        {
            let target = OperationId::pack(self.subsystem, key.payload()).raw();
            let result = submissions.prepare_keyed_sqe_with(self.subsystem, |sqe| {
                // SAFETY: the update operand is immutable process-lifetime storage.
                unsafe { ops::prep_timeout_update(sqe, &IMMEDIATE_TIMEOUT, target, 0) };
            });
            self.timer_update = self.try_stage(result);
        }
    }
    fn cancel_aux(&mut self, submissions: &mut SubmissionContext<'_>) {
        self.expire_timer(submissions);
        // The doorbell sequence change terminates waits, including late SQ consumption.
        if self.owned_targets() > 0 {
            self.flags |= SinkFlags::RETRY;
        }
    }
    /// May be called by a host dispatcher when sibling completions free credit.
    pub fn service(&mut self, submissions: &mut SubmissionContext<'_>) -> liburing::Result<()> {
        if submissions.owner_id() != self.owner_id {
            return Err(UringError::InvalidConfig("logger belongs to another proactor"));
        }
        if self.is_quiescent() {
            return Ok(());
        }
        self.flags &= !SinkFlags::RETRY;
        if self.flags.test(SinkFlags::RING_CLOSING) {
            self.flush_pending_sync();
            self.flags &= !SinkFlags::MORE_WORK;
            self.check_quiescent();
            return Ok(());
        }
        let expected = if self.lifecycle == LifecycleFsm::Running
            && self.flags.test(SinkFlags::FUTEX_SUPPORTED)
            && self.wake == WakeFsm::Disarmed
        {
            Some(
                (DOORBELL.fetch_or(DOORBELL_PARKED_BIT, Ordering::SeqCst) | DOORBELL_PARKED_BIT)
                    & !DOORBELL_URGENT_BIT,
            )
        } else {
            None
        };
        // Arm before consuming urgency: a producer must either be observed here or wake the wait.
        let urgent =
            DOORBELL.fetch_and(!DOORBELL_URGENT_BIT, Ordering::AcqRel) & DOORBELL_URGENT_BIT != 0;
        let now = self.now_ns();
        let output_blocked = matches!(self.write, WriteRotateFsm::Ready(_));
        if matches!(self.write, WriteRotateFsm::Idle) && self.retry_deadline.is_none() {
            self.target_mut().begin();
        }
        {
            let report = self
                .drain_core(now)
                .unwrap_or(DrainReport { next_deadline_ns: -1, flags: DrainFlags::new(0) });
            self.flags = self.flags.patch(
                BitPatch::EMPTY
                    .with_flag(SinkFlags::MORE_WORK, report.flags.test(DrainFlags::MORE_WORK)),
            );
            self.desired_deadline = if output_blocked { None } else { self.deadline(now, report) };
            if !self.write_backoff()
                && matches!(self.write, WriteRotateFsm::Idle)
                && (self.target().step() == RotationStep::Idle || self.retry_deadline.is_some())
                && (urgent
                    || self.lifecycle == LifecycleFsm::Draining
                    || report.flags.test(DrainFlags::OUTPUT_READY)
                    || report.flags.test(DrainFlags::FLUSH_DUE))
                && let Some(slab) = self.core_mut().freeze_output()
            {
                if slab.is_empty() {
                    let _ = self.core_mut().complete_output(slab, 0);
                } else {
                    self.write = WriteRotateFsm::Ready(slab);
                }
            }
            if report.flags.test(DrainFlags::BACKPRESSURE) {
                self.flags &= !SinkFlags::MORE_WORK;
            }
            self.stage_output(submissions);
        }
        if let Some(retry) = self.retry_deadline {
            self.desired_deadline = Some(
                self.desired_deadline
                    .filter(|&deadline| deadline > now)
                    .map_or(retry, |d| d.min(retry)),
            );
        }
        if self.lifecycle == LifecycleFsm::Draining {
            if self.retry_deadline.is_some() {
                self.stage_timer(now, submissions);
            } else {
                self.cancel_aux(submissions);
            }
            self.check_quiescent();
        } else {
            if !output_blocked || self.retry_deadline.is_some() {
                self.stage_timer(now, submissions);
            }
            if let Some(expected) = expected {
                self.stage_wake(expected, submissions);
            }
        }
        if expected.is_some() && self.wake == WakeFsm::Disarmed {
            DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, Ordering::SeqCst);
        }
        if !self.is_quiescent()
            && (self.flags.test(SinkFlags::MORE_WORK)
                || self.flags.test(SinkFlags::RETRY)
                || self.flags.test(SinkFlags::RETIME))
        {
            self.stage_nop(submissions);
        }
        Ok(())
    }
    fn deadline(&self, now: i64, report: DrainReport) -> Option<i64> {
        let periodic = if self.flags.test(SinkFlags::FUTEX_SUPPORTED) {
            self.config.idle_interval_ns
        } else {
            Some(self.config.idle_interval_ns.unwrap_or(1_000_000))
        };
        let periodic = periodic.map(|ns| now.saturating_add(ns.min(i64::MAX as u64) as i64));
        match (periodic, report.next_deadline_ns) {
            (Some(p), r) if r >= 0 && r != i64::MAX => Some(p.min(r)),
            (None, r) if r >= 0 && r != i64::MAX => Some(r),
            (p, _) => p,
        }
    }
    fn stage_output(&mut self, submissions: &mut SubmissionContext<'_>) {
        if self.write_backoff() {
            return;
        }
        if let WriteRotateFsm::Ready(slab) = &self.write {
            let pointer = slab.active_slice().as_ptr();
            let len = slab.len();
            let fd = self.target().current_fd();
            let raw_fd = fd.as_raw_fd();
            let write_flags = self.config.write_flags;
            let result =
                submissions.prepare_keyed_owned_sqe_with(self.subsystem, fd, None, |sqe| {
                    // SAFETY: Ready slab is frozen and retained through the target final CQE.
                    unsafe {
                        ops::prep_write(sqe, raw_fd, pointer, len, u64::MAX);
                        (*sqe).set_op_flags(write_flags);
                    };
                });
            if let Some(key) = self.try_stage(result)
                && let WriteRotateFsm::Ready(slab) =
                    std::mem::replace(&mut self.write, WriteRotateFsm::Idle)
            {
                self.write = WriteRotateFsm::Writing { key, slab };
            }
        } else if self.retry_deadline.is_none()
            && matches!(self.write, WriteRotateFsm::Idle)
            && self.target_mut().begin()
        {
            let target = self.target();
            let fd = target.current_fd();
            let result =
                submissions.prepare_keyed_owned_sqe_with(self.subsystem, fd, None, |sqe| {
                    // SAFETY: target's immutable CStrings are retained through this target CQE.
                    unsafe { target.prepare_step(sqe) };
                });
            if let Some(key) = self.try_stage(result) {
                self.write = WriteRotateFsm::Rotating { key };
            }
        }
    }
    fn stage_timer(&mut self, now: i64, submissions: &mut SubmissionContext<'_>) {
        if let TimerFsm::Armed { deadline_ns, .. } = self.timer {
            if self.desired_deadline.is_none_or(|desired| desired < deadline_ns) {
                self.flags |= SinkFlags::RETIME;
                self.expire_timer(submissions);
            }
            return;
        }
        if let Some(deadline) = self.desired_deadline {
            if submissions.remaining_capacity() == 0 {
                self.flags |= SinkFlags::RETRY;
                return;
            }
            let timeout = match Timeout::new(Duration::from_nanos(
                deadline.saturating_sub(now).max(1) as u64,
            )) {
                Ok(timeout) => timeout,
                Err(error) => {
                    self.last_error = Some(map_uring(error));
                    return;
                }
            };
            let result = submissions.prepare_keyed_sqe_with(self.subsystem, |sqe| {
                // SAFETY: the Timeout owner moves into Armed after successful staging.
                unsafe { timeout.prepare(sqe) };
            });
            if let Some(key) = self.try_stage(result) {
                self.timer = TimerFsm::Armed { key, timeout, deadline_ns: deadline };
            }
        }
    }
    fn stage_wake(&mut self, expected: u32, submissions: &mut SubmissionContext<'_>) {
        let result = submissions.prepare_keyed_sqe_with(self.subsystem, |sqe| {
            // SAFETY: DOORBELL is aligned process-lifetime atomic storage; plain u32 flags only.
            unsafe {
                ops::prep_futex_wait(
                    sqe,
                    DOORBELL.as_ptr(),
                    expected as u64,
                    ops::FUTEX_BITSET_MATCH_ANY,
                    liburing::futex2::FutexFlags::PRIVATE_U32.bits(),
                )
            };
        });
        if let Some(key) = self.try_stage(result) {
            self.wake = WakeFsm::Waiting { key };
        }
    }
    fn check_quiescent(&mut self) {
        if self.lifecycle == LifecycleFsm::Draining
            && self.owned_targets() == 0
            && matches!(self.write, WriteRotateFsm::Idle)
            && !self.flags.test(SinkFlags::MORE_WORK)
            && !self.core_mut().has_unflushed()
        {
            self.lifecycle = LifecycleFsm::Quiescent;
        }
    }
}
impl<C: ConsumerCore> CompletionDispatcher for ProactorLogSink<C> {
    fn dispatch(
        &mut self,
        completion: Completion,
        submissions: &mut SubmissionContext<'_>,
    ) -> liburing::Result<()> {
        if submissions.owner_id() != self.owner_id
            || completion.operation_id.subsystem_id() != self.subsystem
        {
            return Err(UringError::InvalidConfig("completion owner mismatch"));
        }
        let key = OperationKey::from_payload(completion.operation_id.payload())?;
        if !self.key_matches(key) || completion.has_more() {
            return Err(UringError::InvalidConfig("unexpected logger target completion"));
        }
        submissions.claim_completion(completion)?;
        self.handle_terminal(key, completion.result);
        self.service(submissions)
    }
}
impl<C: ConsumerCore> Drop for ProactorLogSink<C> {
    fn drop(&mut self) {
        let has_memory_inflight = matches!(self.timer, TimerFsm::Armed { .. })
            || matches!(
                self.write,
                WriteRotateFsm::Writing { .. } | WriteRotateFsm::Rotating { .. }
            );
        if self.wake != WakeFsm::Disarmed {
            DOORBELL.fetch_and(!DOORBELL_PARKED_BIT, Ordering::SeqCst);
        }
        if has_memory_inflight {
            if let TimerFsm::Armed { timeout, .. } =
                std::mem::replace(&mut self.timer, TimerFsm::Disarmed)
            {
                std::mem::forget(timeout);
            }
            if let WriteRotateFsm::Writing { slab, .. } =
                std::mem::replace(&mut self.write, WriteRotateFsm::Idle)
            {
                std::mem::forget(slab);
            }
            if let Some(target) = self.target.take() {
                std::mem::forget(target);
            }
            if let Some(consumer) = self.consumer.take() {
                consumer.poison();
            }
        }
    }
}
pub(crate) fn map_uring(error: UringError) -> ZeroLogError {
    match error {
        UringError::SqRingFull => ZeroLogError::SqRingFull,
        UringError::RingInit(errno)
        | UringError::Submit(errno)
        | UringError::Wait(errno)
        | UringError::Syscall { errno, .. } => ZeroLogError::Write(errno),
        _ => ZeroLogError::UnsupportedMode("io_uring operation unavailable"),
    }
}

static IMMEDIATE_TIMEOUT: ops::KernelTimespec = ops::KernelTimespec { tv_sec: 0, tv_nsec: 1 };

#[cfg(test)]
#[path = "../tests/unit/sink.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/unit/backpressure.rs"]
mod backpressure_tests;

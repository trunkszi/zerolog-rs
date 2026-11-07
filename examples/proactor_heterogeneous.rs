//! A shared proactor drives an application timer and the logging subsystem.
#![deny(clippy::unwrap_used, clippy::expect_used)]

#[cfg(target_os = "linux")]
mod linux {
    use std::{
        cell::Cell,
        path::Path,
        rc::Rc,
        time::{Duration, Instant},
    };

    use liburing::{
        Completion, CompletionDispatcher, Proactor, ProactorConfig, SubmissionContext, SubsystemId,
        UringError, define_subsystems, operation_slab::OperationKey, ops::Timeout,
    };
    use zerolog::{
        ConsumerCore, DrainBudget, DrainReport, FrozenSlab, LogConsumer, ProactorLogSink,
        SinkConfig, UringTarget, WakePolicy, ZeroLogError,
    };
    use zerolog_uring::OwnedConsumer;

    define_subsystems! { enum AppSubsystems { AppTimer = 1, ZeroLog = 2 } }
    const PRODUCERS: usize = 2;
    const RECORDS: usize = 128;
    const EXPECTED: usize = PRODUCERS * (RECORDS + 1);

    pub struct RunStats {
        pub lines: Vec<String>,
        pub timer_ticks: usize,
        pub timer_during_drain: bool,
        pub owned_targets: usize,
        pub registered_slots: u32,
    }
    struct MeasuredConsumer {
        inner: LogConsumer,
        drained: Rc<Cell<usize>>,
    }
    impl MeasuredConsumer {
        fn poison(self) {
            self.inner.poison_on_abandon();
        }
    }
    impl ConsumerCore for MeasuredConsumer {
        fn drain(&mut self, now: i64, budget: DrainBudget) -> DrainReport {
            let report = self.inner.drain(now, budget);
            self.drained
                .set(self.drained.get() + report.flags.get(zerolog::DrainFlags::DRAINED) as usize);
            report
        }
        fn freeze_output(&mut self) -> Option<FrozenSlab> {
            self.inner.freeze_output()
        }
        fn complete_output(&mut self, slab: FrozenSlab, bytes: u32) -> Option<FrozenSlab> {
            self.inner.complete_output(slab, bytes)
        }
        fn write_failed(&mut self, slab: FrozenSlab, error: ZeroLogError) {
            self.inner.write_failed(slab, error);
        }
        fn has_unflushed(&self) -> bool {
            self.inner.has_unflushed()
        }
        fn flush_sync(&mut self) -> Result<(), ZeroLogError> {
            self.inner.flush_sync()
        }
    }
    struct Timer {
        pending: Option<(OperationKey, Timeout)>,
        ticks: usize,
        during_drain: bool,
    }
    impl Timer {
        fn arm(&mut self, ring: &mut Proactor) -> liburing::Result<()> {
            if self.pending.is_some() {
                return Ok(());
            }
            let timeout = Timeout::new(Duration::from_nanos(1))?;
            let key = ring.prepare_keyed_sqe_with(SubsystemId(1), |sqe| {
                // SAFETY: the boxed timespec remains owned until this target's terminal CQE.
                unsafe {
                    timeout.prepare(sqe);
                }
            })?;
            self.pending = Some((key, timeout));
            Ok(())
        }
        fn complete(
            &mut self,
            completion: Completion,
            submissions: &mut SubmissionContext<'_>,
            drained: usize,
        ) -> liburing::Result<()> {
            let Some((key, _)) = self.pending.as_ref() else {
                return Err(UringError::InvalidConfig("unexpected application timer"));
            };
            if key.payload() != completion.operation_id.payload() || completion.has_more() {
                return Err(UringError::InvalidConfig("application timer identity mismatch"));
            }
            submissions.claim_completion(completion)?;
            self.pending.take();
            if completion.result == -libc::ETIME {
                self.ticks += 1;
                self.during_drain |= drained > 0 && drained < EXPECTED;
            } else if completion.result != -libc::ECANCELED {
                return Err(UringError::InvalidConfig("application timer failed"));
            }
            Ok(())
        }
    }
    impl Drop for Timer {
        fn drop(&mut self) {
            if let Some((_, timeout)) = self.pending.take() {
                // An early host error must not free a timespec still referenced by the kernel.
                std::mem::forget(timeout);
            }
        }
    }

    pub fn run(path: &Path) -> Result<RunStats, Box<dyn std::error::Error>> {
        zerolog::stop_polling_thread()?;
        zerolog::set_level(zerolog::Level::TRC);
        zerolog::set_flush_on(zerolog::Level::ERR);
        zerolog::set_flush_delay(1_000_000);
        zerolog::set_flush_buffer_size(2048);
        zerolog::set_queue_full_policy(zerolog::QueueFullPolicy::Drop);
        zerolog::set_output_writer(std::io::sink());
        zerolog::set_header_pattern("{date} {time}.{ns} {level} ")?;
        let drained = Rc::new(Cell::new(0));
        let consumer = MeasuredConsumer {
            inner: zerolog::acquire_consumer(WakePolicy::default())?,
            drained: drained.clone(),
        };
        let mut ring = Proactor::new(ProactorConfig::default())?;
        let mut sink = ProactorLogSink::new(
            &mut ring,
            SubsystemId(2),
            OwnedConsumer::embedded(consumer, MeasuredConsumer::poison),
            UringTarget::open(path, 2048, 64, false)?,
            SinkConfig {
                budget: DrainBudget { max_records: 16, max_bytes: 4096 },
                ..Default::default()
            },
        )?;
        let mut timer = Timer { pending: None, ticks: 0, during_drain: false };
        timer.arm(&mut ring)?;
        let mut producers: Vec<_> = (0..PRODUCERS).map(|producer| std::thread::spawn(move || {
            zerolog::thread_buffer::preallocate();
            for sequence in 0..RECORDS {
                zerolog::event!(zerolog::Level::INF, "record"; producer => producer as u32, sequence => sequence as u32);
            }
            zerolog::error!("urgent producer={}", producer as u32);
        })).collect();
        let bound = Instant::now() + Duration::from_secs(10);
        let mut stopping = false;
        loop {
            if !stopping && producers.iter().all(std::thread::JoinHandle::is_finished) {
                for producer in producers.drain(..) {
                    producer.join().map_err(|_| "producer panicked")?;
                }
                sink.begin_shutdown();
                stopping = true;
            }
            if sink.is_quiescent() && timer.pending.is_none() {
                break;
            }
            {
                let mut state = (&mut sink, &mut timer);
                let mut dispatcher = AppSubsystems::dispatcher(
                    &mut state,
                    |route, state, completion, submissions| match route {
                        AppSubsystems::ZeroLog => state.0.dispatch(completion, submissions),
                        AppSubsystems::AppTimer => {
                            state.1.complete(completion, submissions, drained.get())
                        }
                    },
                );
                ring.wait_completions(1, Some(Duration::from_millis(20)), &mut dispatcher)?;
            }
            // Host contract: sibling CQEs may free staging credit without any logger CQE.
            // After every host turn, retry a pointer-free continuation if needs_service() is set.
            if sink.needs_service()
                && let Err(error) = sink.kick(&mut ring)
                && error != UringError::SqRingFull
            {
                return Err(error.into());
            }
            if !stopping
                && let Err(error) = timer.arm(&mut ring)
                && error != UringError::SqRingFull
            {
                return Err(error.into());
            }
            if let Some(error) = sink.last_error() {
                return Err(error.into());
            }
            if Instant::now() >= bound {
                return Err("shared proactor drain timed out".into());
            }
        }
        for producer in producers {
            producer.join().map_err(|_| "producer panicked")?;
        }
        {
            let mut state = (&mut sink, &mut timer);
            let mut dispatcher =
                AppSubsystems::dispatcher(&mut state, |route, state, completion, submissions| {
                    match route {
                        AppSubsystems::ZeroLog => state.0.dispatch(completion, submissions),
                        AppSubsystems::AppTimer => {
                            state.1.complete(completion, submissions, drained.get())
                        }
                    }
                });
            ring.shutdown_with(&mut dispatcher)?;
        }
        let owned_targets = sink.owned_targets() + usize::from(timer.pending.is_some());
        drop(sink);
        drop(zerolog::acquire_consumer(WakePolicy::default())?);
        let registered_slots = zerolog::contract::REGISTERED_SLOTS
            .iter()
            .map(|word| word.load(std::sync::atomic::Ordering::Acquire).count_ones())
            .sum();
        let mut lines = Vec::new();
        for suffix in (1..=64).rev() {
            let history = path.with_file_name(format!(
                "{}.{}",
                path.file_name().ok_or("missing base name")?.to_string_lossy(),
                suffix
            ));
            match std::fs::read_to_string(history) {
                Ok(text) => lines.extend(text.lines().map(str::to_owned)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        lines.extend(std::fs::read_to_string(path)?.lines().map(str::to_owned));
        Ok(RunStats {
            lines,
            timer_ticks: timer.ticks,
            timer_during_drain: timer.during_drain,
            owned_targets,
            registered_slots,
        })
    }
}
#[cfg(target_os = "linux")]
pub use linux::run;

#[cfg(target_os = "linux")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path =
        std::env::args_os().nth(1).unwrap_or_else(|| "/tmp/zerolog-heterogeneous.log".into());
    let result = run(std::path::Path::new(&path))?;
    println!(
        "{} records; {} application timer completions; {} live logger targets",
        result.lines.len(),
        result.timer_ticks,
        result.owned_targets
    );
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This shared io_uring example requires Linux.");
}

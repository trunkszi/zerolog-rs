#![cfg(target_os = "linux")]
#![deny(clippy::unwrap_used, clippy::expect_used)]
#[allow(dead_code)]
#[path = "../examples/proactor_heterogeneous.rs"]
mod heterogeneous;

#[test]
fn shared_proactor_preserves_rotated_records() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serial();
    let initial_fds = std::fs::read_dir("/proc/self/fd")?.count();
    let directory = std::env::temp_dir().join(format!("zerolog-e2e-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    let path = directory.join("events");
    let result = heterogeneous::run(&path)?;
    assert_eq!(result.lines.len(), 258);
    assert!(
        result.lines.windows(2).all(|pair| pair[0][..29] <= pair[1][..29]),
        "timestamps were reordered across output batches"
    );
    for producer in 0..2 {
        for sequence in 0..128 {
            let marker = format!("record producer={producer} sequence={sequence}");
            assert_eq!(
                result.lines.iter().filter(|line| line.ends_with(&marker)).count(),
                1,
                "{marker}"
            );
        }
        let marker = format!("urgent producer={producer}");
        assert_eq!(result.lines.iter().filter(|line| line.ends_with(&marker)).count(), 1);
    }
    assert!(path.with_file_name("events.1").exists(), "burst never rotated");
    assert!(result.timer_ticks > 0);
    assert!(result.timer_during_drain, "application timer starved behind logger drain");
    assert_eq!(result.owned_targets, 0);
    assert_eq!(result.registered_slots, 0);
    assert_eq!(
        std::fs::read_dir("/proc/self/fd")?.count(),
        initial_fds,
        "shared proactor leaked file descriptors"
    );
    std::fs::remove_dir_all(directory)?;
    Ok(())
}

use std::{
    cell::Cell,
    io::{self, Write},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use zerolog::{
    ConsumerCore, DrainBudget, DrainFlags, DrainReport, FrozenSlab, LogConsumer, ZeroLogError,
};
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}
const BODY_BYTES: usize = 8192;
const RECORD_BYTES: usize = BODY_BYTES + 1;
#[derive(Clone, Copy, Default)]
struct PressureStats {
    records: usize,
    backpressure: bool,
    written: u64,
    max_slab: usize,
}
struct PressureConsumer {
    inner: LogConsumer,
    stats: Rc<Cell<PressureStats>>,
}
impl PressureConsumer {
    fn poison(self) {
        self.inner.poison_on_abandon();
    }
}
impl ConsumerCore for PressureConsumer {
    fn drain(&mut self, now: i64, budget: DrainBudget) -> DrainReport {
        let report = self.inner.drain(now, budget);
        let mut stats = self.stats.get();
        stats.records += report.flags.get(DrainFlags::DRAINED) as usize;
        stats.backpressure |= report.flags.test(DrainFlags::BACKPRESSURE);
        self.stats.set(stats);
        report
    }
    fn freeze_output(&mut self) -> Option<FrozenSlab> {
        let slab = self.inner.freeze_output()?;
        let mut stats = self.stats.get();
        stats.max_slab = stats.max_slab.max(slab.len() as usize);
        self.stats.set(stats);
        Some(slab)
    }
    fn complete_output(&mut self, slab: FrozenSlab, bytes: u32) -> Option<FrozenSlab> {
        let mut stats = self.stats.get();
        stats.written += bytes as u64;
        self.stats.set(stats);
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
struct BlockedWriter;
impl Write for BlockedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Err(io::ErrorKind::WouldBlock.into())
    }
}
#[test]
fn permanently_full_target_bounds_memory() -> Result<(), Box<dyn std::error::Error>> {
    let _serial = serial();
    const CHILD: &str = "ZEROLOG_PRESSURE_E2E_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let status = std::process::Command::new(std::env::current_exe()?)
            .args(["--exact", "permanently_full_target_bounds_memory"])
            .env(CHILD, "1")
            .status()?;
        assert!(status.success());
        return Ok(());
    }
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    let mut descriptors = [-1; 2];
    // SAFETY: the array provides two writable descriptor slots.
    assert_eq!(
        unsafe { libc::pipe2(descriptors.as_mut_ptr(), libc::O_NONBLOCK | libc::O_CLOEXEC) },
        0
    );
    // SAFETY: successful pipe2 descriptors are adopted exactly once.
    let _reader = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    // SAFETY: this is the separate owned write descriptor.
    let mut writer = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(descriptors[1]) });
    // SAFETY: fcntl operates on a live pipe.
    let capacity = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_SETPIPE_SZ, 4096) };
    assert!(capacity > 0);
    writer.write_all(&vec![0; capacity as usize])?;
    zerolog::set_header_pattern("")?;
    zerolog::set_output_writer(BlockedWriter);
    zerolog::set_level(zerolog::Level::INF);
    zerolog::set_queue_full_policy(zerolog::QueueFullPolicy::Drop);
    zerolog::reset_queue_full_count();
    let stats = Rc::new(Cell::new(PressureStats::default()));
    let consumer = PressureConsumer {
        inner: zerolog::acquire_consumer(zerolog::WakePolicy::default())?,
        stats: stats.clone(),
    };
    let mut ring = liburing::Proactor::new(liburing::ProactorConfig::default())?;
    let mut sink = zerolog::ProactorLogSink::new(
        &mut ring,
        liburing::SubsystemId(1),
        zerolog_uring::OwnedConsumer::embedded(consumer, PressureConsumer::poison),
        zerolog::UringTarget::from_fd(writer.into()),
        zerolog::SinkConfig {
            budget: DrainBudget { max_records: 1024, max_bytes: 16 * 1024 * 1024 },
            write_flags: libc::RWF_NOWAIT as u32,
            ..Default::default()
        },
    )?;
    let stop = Arc::new(AtomicBool::new(false));
    let producer_stop = stop.clone();
    let producer = std::thread::spawn(move || {
        let body = "x".repeat(BODY_BYTES);
        zerolog::thread_buffer::preallocate();
        while !producer_stop.load(Ordering::Acquire) {
            zerolog::info!("{}", body.as_str());
        }
    });
    let bound = Instant::now() + Duration::from_secs(10);
    let limit = zerolog::output::MAX_PENDING_BYTES;
    let mut saturated_drops = None;
    loop {
        ring.wait_completions(1, Some(Duration::from_millis(20)), &mut sink)?;
        if sink.needs_service() {
            sink.kick(&mut ring)?;
        }
        let observed = stats.get();
        assert_eq!(observed.written, 0, "full pipe unexpectedly accepted log bytes");
        assert!(
            observed.records * RECORD_BYTES <= limit,
            "formatted output escaped its memory budget"
        );
        assert!(observed.max_slab <= limit);
        if observed.backpressure && observed.records * RECORD_BYTES > limit - RECORD_BYTES {
            let previous = *saturated_drops.get_or_insert_with(zerolog::queue_full_count);
            if zerolog::queue_full_count() > previous {
                break;
            }
        }
        assert!(
            Instant::now() < bound,
            "did not reach bounded backpressure: {} formatted bytes",
            observed.records * RECORD_BYTES
        );
    }
    assert!(sink.retry_backoff_ns() > 0);
    assert!(zerolog::queue_full_count() > 0);
    stop.store(true, Ordering::Release);
    producer.join().map_err(|_| "producer panicked")?;
    ring.shutdown_with(&mut sink)?;
    assert_eq!(sink.owned_targets(), 0);
    drop(sink);
    drop(zerolog::acquire_consumer(zerolog::WakePolicy::default())?);
    Ok(())
}

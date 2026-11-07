use std::{
    io::Write,
    sync::{Arc, atomic::AtomicUsize},
};

use zerolog_core::{StaticInfo, TimestampCounter, WakePolicy, output::MAX_PENDING_BYTES};

use super::*;

const RECORD_BYTES: usize = 1024 * 1024 + 32;
struct LegacyWriter(Arc<AtomicUsize>);
impl Write for LegacyWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.fetch_add(bytes.len(), Ordering::Relaxed);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
unsafe fn decode(_: *const u8, output: &mut Vec<u8>) {
    output.resize(output.len() + RECORD_BYTES - 1, b'x');
}
static INFO: StaticInfo =
    StaticInfo { level: zerolog_core::Level::INF, location: "pressure", format_string: "", decode };
fn publish() -> Result<(), &'static str> {
    let size = zerolog_core::PAYLOAD_HEADER_SIZE as u32;
    let header = zerolog_core::alloc_message(size).ok_or("producer allocation failed")?;
    // SAFETY: this producer owns the reservation; metadata matches INFO's decoder.
    unsafe {
        let bytes = zerolog_core::spsc_queue::MessageHeader::payload_pointer_raw(header);
        bytes.cast::<*const StaticInfo>().write_unaligned(&INFO);
        bytes
            .add(zerolog_core::PAYLOAD_TIMESTAMP_OFFSET)
            .cast::<i64>()
            .write_unaligned(TimestampCounter::read_timestamp_counter());
        bytes.add(zerolog_core::PAYLOAD_SPAN_ID_OFFSET).cast::<u64>().write_unaligned(0);
        (*header).push(size);
    }
    Ok(())
}
#[test]
fn inflight_overflow_never_uses_legacy() -> Result<(), Box<dyn std::error::Error>> {
    use std::os::fd::AsRawFd;
    zerolog_core::poll(true)?;
    zerolog_core::set_header_pattern("")?;
    let legacy = Arc::new(AtomicUsize::new(0));
    zerolog_core::set_output_writer(LegacyWriter(legacy.clone()));
    std::thread::spawn(|| {
        for _ in 0..22 {
            publish()?;
        }
        Ok::<_, &'static str>(())
    })
    .join()
    .map_err(|_| "producer panicked")??;
    let (_reader, writer) = crate::test_support::pipe()?;
    // SAFETY: F_GETPIPE_SZ only queries the live pipe descriptor.
    let capacity = unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETPIPE_SZ) };
    assert!(capacity > 0);
    std::fs::File::from(writer.try_clone()?).write_all(&vec![b'f'; capacity as usize])?;
    let mut core = zerolog_core::acquire_consumer(WakePolicy::default())?;
    let first = core.drain(0, DrainBudget { max_records: 1, max_bytes: u32::MAX });
    assert_eq!(first.flags.get(DrainFlags::DRAINED), 1);
    let frozen = core.freeze_output().ok_or("initial frozen output missing")?;
    let mut sink = super::tests::sink_with(core, Rotation::from_fd(writer));
    sink.config.budget = DrainBudget { max_records: 256, max_bytes: u32::MAX };
    // No SQE is submitted: this retains the same frozen owner as a blocked WRITE.
    let key = OperationKey::from_payload(liburing::OperationPayload(1 << 24))?;
    sink.write = WriteRotateFsm::Writing { key, slab: frozen };
    let report = sink.drain_core(1).ok_or("inflight drain was blocked")?;
    let accepted = ((MAX_PENDING_BYTES - RECORD_BYTES) / RECORD_BYTES) as u32;
    assert_eq!(report.flags.get(DrainFlags::DRAINED), accepted);
    assert!(report.flags.test(DrainFlags::BACKPRESSURE));
    assert!(report.flags.test(DrainFlags::MORE_WORK));
    assert_eq!(legacy.load(Ordering::Relaxed), 0);
    let paused = sink.drain_core(2).ok_or("inflight drain was blocked")?;
    assert_eq!(paused.flags.get(DrainFlags::DRAINED), 0);
    assert!(paused.flags.test(DrainFlags::BACKPRESSURE));
    let WriteRotateFsm::Writing { slab, .. } =
        std::mem::replace(&mut sink.write, WriteRotateFsm::Idle)
    else {
        return Err("simulated WRITE owner lost".into());
    };
    sink.core_mut().write_failed(slab, ZeroLogError::Write(libc::EAGAIN));
    let pending = sink.core_mut().freeze_output().ok_or("pending slab missing")?;
    assert_eq!(pending.len() as usize, (1 + accepted as usize) * RECORD_BYTES);
    assert!((pending.len() as usize) <= MAX_PENDING_BYTES);
    let len = pending.len();
    assert!(sink.core_mut().complete_output(pending, len).is_none());
    let rest = sink.drain_core(3).ok_or("final drain blocked")?;
    assert_eq!(1 + accepted + rest.flags.get(DrainFlags::DRAINED), 22);
    let pending = sink.core_mut().freeze_output().ok_or("remaining output missing")?;
    let len = pending.len();
    assert!(sink.core_mut().complete_output(pending, len).is_none());
    assert_eq!(legacy.load(Ordering::Relaxed), 0);
    drop(sink);
    zerolog_core::set_output_writer(std::io::sink());
    Ok(())
}

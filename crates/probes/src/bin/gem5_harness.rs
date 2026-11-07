#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Three-point phase-aligned guest for differential O3 simulation.
//!
//! Accepts a zero-padded iteration count (`0000000512`, `0000002560`, `0000004608`).
//! `--pmu` runs a longer native workload and exits successfully for hardware profilers.
//! Simulator runs append `--gem5` to reset and dump only the measured region.
//! Queue and header warmup run before the checksum loop. The process exits with
//! `checksum & 0x7f` so the result cannot be deleted as dead code.

use std::{
    alloc::{Layout, alloc_zeroed, dealloc, handle_alloc_error},
    io::{self, Write},
    process::ExitCode,
};

use zerolog_core::{
    header::HeaderDateCache,
    number_format::format_u64,
    spsc_queue::{MessageHeader, SpscVarQueue},
};
use zerolog_probes::probe_header_format;

const PAYLOAD: u32 = 32;
const WARMUP: u64 = 32;
const PMU_ITERATIONS: u64 = 100_000_000;
const FOLD: u64 = 0x9E37_79B9_7F4A_7C15;

fn main() -> ExitCode {
    let mut args = std::env::args();
    let _program = args.next();
    let Some(raw) = args.next() else {
        emit(io::stderr(), "usage: gem5_harness N|--pmu");
        return ExitCode::from(255);
    };
    let simulated = args.next().map(|arg| arg == "--gem5");
    if simulated == Some(false) || args.next().is_some() {
        emit(io::stderr(), "usage: gem5_harness N|--pmu [--gem5]");
        return ExitCode::from(255);
    }
    let simulated = simulated.unwrap_or(false);
    let pmu = raw == "--pmu";
    if simulated && (pmu || !cfg!(target_arch = "x86_64")) {
        emit(io::stderr(), "gem5 markers require an x86_64 phase-count run");
        return ExitCode::from(255);
    }
    let iterations = if pmu {
        PMU_ITERATIONS
    } else {
        let Some(count) = parse_iterations(&raw) else {
            emit(io::stderr(), "N must be 512, 2560, 4608, or --pmu");
            return ExitCode::from(255);
        };
        count
    };

    let layout = Layout::new::<SpscVarQueue>();
    // SAFETY: the layout is the queue type's own layout and is nonzero.
    let queue = unsafe { alloc_zeroed(layout) }.cast::<SpscVarQueue>();
    if queue.is_null() {
        handle_alloc_error(layout);
    }
    let mut cache = HeaderDateCache::new();
    warm_header(&mut cache);
    warm_queue(queue);

    #[cfg(target_arch = "x86_64")]
    if simulated {
        // SAFETY: --gem5 is supplied only inside the simulator that implements the m5 ABI.
        unsafe {
            gem5_stats::<0x40>();
        }
    }
    let checksum = core::hint::black_box(run_roi(queue, iterations));
    #[cfg(target_arch = "x86_64")]
    if simulated {
        // SAFETY: the simulator consumes this first stats block before process teardown.
        unsafe {
            gem5_stats::<0x41>();
        }
    }
    emit(io::stdout(), &format!("checksum {checksum}"));
    // SAFETY: `queue` came from `alloc_zeroed` with this layout and is still exclusively owned.
    unsafe { dealloc(queue.cast(), layout) };
    let status = if pmu { 0 } else { u8::try_from(checksum & 0x7f).unwrap_or(0) };
    ExitCode::from(status)
}

// These control-plane marker encodings follow gem5's util/m5/src/abi/x86/m5op.S.
// Memory clobbering prevents compiler motion across the measurement boundary.
#[cfg(target_arch = "x86_64")]
unsafe fn gem5_stats<const OP: u16>() {
    // SAFETY: caller runs only in gem5; rdi/rsi are zero delay and period per m5ops.h.
    unsafe {
        core::arch::asm!(".byte 0x0f, 0x04", ".word {op}", op = const OP,
            in("rdi") 0u64, in("rsi") 0u64, clobber_abi("C"), options(nostack));
    }
}

fn emit(mut sink: impl Write, line: &str) {
    writeln!(sink, "{line}").ok();
}

fn parse_iterations(raw: &str) -> Option<u64> {
    let iterations = raw.parse::<u64>().ok()?;
    matches!(iterations, 512 | 2560 | 4608).then_some(iterations)
}

fn warm_header(cache: &mut HeaderDateCache) {
    let base = 1_700_000_000_000_000_000_i64;
    let mut out = [0_u8; 64];
    let _ = probe_header_format(cache, base, &mut out);
    let _ = probe_header_format(cache, base + 50_000, &mut out);
    let _ = probe_header_format(cache, base + 2_000_000_000, &mut out);
}

fn warm_queue(queue: *mut SpscVarQueue) {
    let mut index = 0_u64;
    while index < WARMUP {
        let _ = core::hint::black_box(cycle_once(queue, index));
        index += 1;
    }
}

fn run_roi(queue: *mut SpscVarQueue, iterations: u64) -> u64 {
    let mut checksum = 0_u64;
    let mut index = 0_u64;
    while index < iterations {
        checksum = fold(checksum, cycle_once(queue, index));
        checksum = core::hint::black_box(checksum);
        index += 1;
    }
    checksum
}

fn cycle_once(queue: *mut SpscVarQueue, index: u64) -> u64 {
    let payload_val = index.wrapping_mul(FOLD) ^ 0xBF58_476D_1CE4_E5B9;
    // SAFETY: this process is the only producer and consumer, and warmup initialized the ring.
    let header = unsafe { SpscVarQueue::producer_alloc(queue, PAYLOAD) };
    let Some(header) = header else {
        panic!("spsc reservation failed");
    };
    // SAFETY: the reservation is unpublished and large enough for one u64.
    unsafe {
        MessageHeader::payload_pointer_raw(header).cast::<u64>().write_unaligned(payload_val);
        (*header).push(PAYLOAD);
    }
    // SAFETY: the publish above is visible to this thread's consumer cursor.
    let front = unsafe { SpscVarQueue::consumer_front(queue) };
    let Some(front) = front else {
        panic!("spsc consumer missed a published record");
    };
    // SAFETY: the acquired record is still unpublished to other consumers and holds the u64.
    let observed =
        unsafe { MessageHeader::payload_pointer_raw(front).cast::<u64>().read_unaligned() };
    // SAFETY: the payload read above finished before the consumer releases the record.
    unsafe { SpscVarQueue::consumer_pop(queue) };

    let mut digits = [0_u8; 24];
    let written = format_u64(&mut digits, payload_val);
    fold(observed, written as u64) ^ (digits[0] as u64)
}

fn fold(acc: u64, value: u64) -> u64 {
    acc.wrapping_mul(FOLD).wrapping_add(value)
}

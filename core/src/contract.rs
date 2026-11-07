#![deny(clippy::unwrap_used, clippy::expect_used)]
//! Value layouts and ownership boundaries shared by logging adapters.

use std::{
    marker::PhantomData,
    rc::Rc,
    sync::atomic::{AtomicU32, AtomicU64},
};

use uarch::prelude::*;

use crate::ZeroLogError;

bitword! {
    #[derive(Debug)]
    pub struct LevelGateWord(u32) {
        [0..=7] pub MIN_LEVEL: u8,
        [8..=15] pub RING_LEVEL: u8,
        [16] pub DEEP_PARK,
    }
}
impl LevelGateWord {
    pub const fn pack(min_level: u8, ring_level: u8, deep_park: bool) -> u32 {
        Self::new(0)
            .patch(
                BitPatch::EMPTY
                    .with_field(Self::MIN_LEVEL, min_level as u64)
                    .with_field(Self::RING_LEVEL, ring_level as u64)
                    .with_flag(Self::DEEP_PARK, deep_park),
            )
            .raw()
    }
}
bitword! {
    #[derive(Debug)]
    pub struct DoorbellWord(u32) {
        [0] pub PARKED,
        [1] pub URGENT,
        [2..=31] pub RING_SEQ: u32,
    }
}
pub const DOORBELL_PARKED_BIT: u32 = DoorbellWord::PARKED.mask() as u32;
pub const DOORBELL_URGENT_BIT: u32 = DoorbellWord::URGENT.mask() as u32;
pub const DOORBELL_RING_ONE: u32 =
    DoorbellWord::new(0).patch(BitPatch::EMPTY.with_field(DoorbellWord::RING_SEQ, 1)).raw();
bitword! {
    #[derive(Debug)]
    pub struct RecordCursor(u64) {
        [0..=23] pub BLOCK_INDEX: u32,
        [24..=31] pub SLOT: u8,
        [32..=63] pub SLOT_EPOCH: u32,
    }
}
#[register_pass]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeapNode {
    pub raw_timestamp: i64,
    pub cursor: RecordCursor,
}
#[register_pass]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainBudget {
    pub max_records: u32,
    pub max_bytes: u32,
}
bitword! {
    #[derive(Debug)]
    pub struct DrainFlags(u64) {
        [0] pub MORE_WORK,
        [1] pub OUTPUT_READY,
        [2] pub FLUSH_DUE,
        [3] pub IDLE,
        [4] pub BACKPRESSURE,
        [32..=63] pub DRAINED: u32,
    }
}
#[register_pass]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DrainReport {
    pub next_deadline_ns: i64,
    pub flags: DrainFlags,
}
assert_layout!(LevelGateWord, size = 4, align = 4);
assert_layout!(DoorbellWord, size = 4, align = 4);
assert_layout!(RecordCursor, size = 8, align = 8);
assert_layout!(HeapNode, size = 16, align = 8, fields = { raw_timestamp: 0, cursor: 8 });
assert_layout!(DrainBudget, size = 8, align = 4, fields = { max_records: 0, max_bytes: 4 });
assert_layout!(DrainFlags, size = 8, align = 8);
assert_layout!(DrainReport, size = 16, align = 8, fields = { next_deadline_ns: 0, flags: 8 });

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WakePolicy {
    AdaptiveTimer { min_interval_ns: u64, max_idle_interval_ns: u64 },
    DeepPark { deep_park_after_ns: u64 },
    Fixed { interval_ns: u64 },
}
impl Default for WakePolicy {
    fn default() -> Self {
        Self::AdaptiveTimer { min_interval_ns: 1_000_000, max_idle_interval_ns: 100_000_000 }
    }
}

#[derive(Debug)]
pub struct FrozenSlab {
    bytes: Box<[u8]>,
    offset: u32,
    len: u32,
}
#[derive(Debug)]
pub struct ConsumerLease {
    generation: u32,
    poisoned: bool,
    _owner_thread: PhantomData<Rc<()>>,
}

pub trait ConsumerCore {
    fn drain(&mut self, now_ns: i64, budget: DrainBudget) -> DrainReport;
    fn freeze_output(&mut self) -> Option<FrozenSlab>;
    fn complete_output(&mut self, slab: FrozenSlab, written: u32) -> Option<FrozenSlab>;
    fn write_failed(&mut self, slab: FrozenSlab, error: ZeroLogError);
    fn has_unflushed(&self) -> bool;
    fn flush_sync(&mut self) -> Result<(), ZeroLogError>;
}

pub static LEVEL_GATE: Isolated<AtomicU32> =
    Isolated::new(AtomicU32::new(LevelGateWord::pack(1, 5, false)));
pub static DOORBELL: Isolated<AtomicU32> = Isolated::new(AtomicU32::new(0));
#[cfg(test)]
pub(crate) static DOORBELL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
pub static QUEUE_FULL_COUNT: Isolated<AtomicU64> = Isolated::new(AtomicU64::new(0));
pub static REGISTERED_SLOTS: Isolated<[AtomicU64; 4]> =
    Isolated::new([const { AtomicU64::new(0) }; 4]);
static CONSUMER_LEASE_STATE: Isolated<AtomicU64> = Isolated::new(AtomicU64::new(0));
assert_layout!(Isolated<AtomicU32>, size = 128, align = 128);
assert_layout!(Isolated<AtomicU64>, size = 128, align = 128);
assert_layout!(Isolated<[AtomicU64; 4]>, size = 128, align = 128);

impl FrozenSlab {
    /// # Panics
    /// Panics if the initialized length exceeds the allocation.
    pub fn new(bytes: Box<[u8]>, len: u32) -> Self {
        assert!(len as usize <= bytes.len(), "slab length exceeds allocation");
        Self { bytes, offset: 0, len }
    }
    pub fn active_slice(&self) -> &[u8] {
        &self.bytes[self.offset as usize..self.len as usize]
    }
    /// # Panics
    /// Panics if the completion exceeds the unwritten suffix.
    pub fn advance(&mut self, written: u32) -> bool {
        assert!(written <= self.len(), "completion exceeds slab suffix");
        self.offset += written;
        self.is_empty()
    }
    pub fn into_inner_box(self) -> Box<[u8]> {
        self.bytes
    }
    pub fn len(&self) -> u32 {
        self.len - self.offset
    }
    pub fn is_empty(&self) -> bool {
        self.offset == self.len
    }
}

bitword! {
    struct LeaseWord(u64) {
        [0] LEASED,
        [1] POISONED,
        [32..=63] GENERATION: u32,
    }
}
assert_layout!(LeaseWord, size = 8, align = 8);

impl ConsumerLease {
    pub fn try_acquire() -> Result<Self, ZeroLogError> {
        use std::sync::atomic::Ordering;
        let mut previous = CONSUMER_LEASE_STATE.load(Ordering::Acquire);
        loop {
            let state = LeaseWord::new(previous);
            if state.test(LeaseWord::POISONED) {
                return Err(ZeroLogError::ConsumerPoisoned);
            }
            if state.test(LeaseWord::LEASED) {
                return Err(ZeroLogError::ConsumerBusy);
            }
            let next = state
                .wrapping_add_top_field(LeaseWord::GENERATION, 1)
                .patch(BitPatch::EMPTY.with_flag(LeaseWord::LEASED, true));
            match CONSUMER_LEASE_STATE.compare_exchange_weak(
                previous,
                next.raw(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Ok(Self {
                        generation: next.get(LeaseWord::GENERATION),
                        poisoned: false,
                        _owner_thread: PhantomData,
                    });
                }
                Err(actual) => previous = actual,
            }
        }
    }
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Writes `generation` into an idle lease word.
    ///
    /// The word is private, so a test that must observe the wrap from
    /// `u32::MAX` seeds it here instead of acquiring `2^32` times.
    /// A leased or poisoned word is left unchanged.
    #[doc(hidden)]
    pub fn seed_idle_generation(generation: u32) -> bool {
        use std::sync::atomic::Ordering;
        let mut previous = CONSUMER_LEASE_STATE.load(Ordering::Acquire);
        loop {
            let state = LeaseWord::new(previous);
            if state.test(LeaseWord::LEASED) || state.test(LeaseWord::POISONED) {
                return false;
            }
            let next = LeaseWord::new(0)
                .patch(BitPatch::EMPTY.with_field(LeaseWord::GENERATION, u64::from(generation)));
            match CONSUMER_LEASE_STATE.compare_exchange_weak(
                previous,
                next.raw(),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(actual) => previous = actual,
            }
        }
    }
    /// Permanently blocks reuse when kernel pointers may still be live.
    pub fn poison_on_abandon(mut self) {
        CONSUMER_LEASE_STATE
            .fetch_or(LeaseWord::POISONED.mask(), std::sync::atomic::Ordering::Release);
        self.poisoned = true;
    }
}
impl Drop for ConsumerLease {
    fn drop(&mut self) {
        if !self.poisoned {
            CONSUMER_LEASE_STATE
                .fetch_and(!LeaseWord::LEASED.mask(), std::sync::atomic::Ordering::Release);
        }
    }
}

/// Publishes a notification before waking a consumer that advertised parking.
#[cold_sink]
pub fn ring_doorbell(urgent: bool) {
    use std::sync::atomic::Ordering;
    let mut previous = DOORBELL.load(Ordering::Relaxed);
    loop {
        let next = (previous.wrapping_add(DOORBELL_RING_ONE) & !DOORBELL_PARKED_BIT)
            | if urgent { DOORBELL_URGENT_BIT } else { 0 };
        match DOORBELL.compare_exchange_weak(previous, next, Ordering::SeqCst, Ordering::Relaxed) {
            Ok(old) => {
                if old & DOORBELL_PARKED_BIT != 0 {
                    let _ = futex_wake_doorbell();
                }
                return;
            }
            Err(actual) => previous = actual,
        }
    }
}

/// Returns the number of kernel waiters woken, including io_uring waiters.
#[cfg(target_os = "linux")]
pub fn futex_wake_doorbell() -> std::io::Result<u32> {
    // SAFETY: the aligned atomic word has process lifetime, and wake does not
    // dereference any additional userspace operand.
    let result = unsafe {
        libc::syscall(
            libc::SYS_futex,
            DOORBELL.as_ptr(),
            libc::FUTEX_WAKE | libc::FUTEX_PRIVATE_FLAG,
            i32::MAX,
        )
    };
    if result < 0 { Err(std::io::Error::last_os_error()) } else { Ok(result as u32) }
}

/// Waits while the comparison word equals `expected`.
/// EAGAIN, EINTR and ETIMEDOUT remain visible so drivers can recheck their state.
#[cfg(target_os = "linux")]
pub fn futex_wait_doorbell_timeout(expected: u32, timeout_ns: Option<u64>) -> std::io::Result<()> {
    let timeout = timeout_ns
        .map(|ns| -> std::io::Result<libc::timespec> {
            Ok(libc::timespec {
                tv_sec: libc::time_t::try_from(ns / 1_000_000_000).map_err(|_| {
                    std::io::Error::from(ZeroLogError::InvalidConfig("futex timeout overflow"))
                })?,
                tv_nsec: (ns % 1_000_000_000) as libc::c_long,
            })
        })
        .transpose()?;
    let pointer = timeout.as_ref().map_or(std::ptr::null(), std::ptr::from_ref);
    // SAFETY: both the process-lifetime atomic and the optional stack timespec
    // remain valid throughout this synchronous syscall; no pointers are retained.
    let result = unsafe {
        libc::syscall(
            libc::SYS_futex,
            DOORBELL.as_ptr(),
            libc::FUTEX_WAIT | libc::FUTEX_PRIVATE_FLAG,
            expected,
            pointer,
            std::ptr::null::<u32>(),
            0u32,
        )
    };
    if result < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
}

#[cfg(not(target_os = "linux"))]
pub fn futex_wake_doorbell() -> std::io::Result<u32> {
    Err(ZeroLogError::UnsupportedMode("Linux futex required").into())
}
#[cfg(not(target_os = "linux"))]
pub fn futex_wait_doorbell_timeout(
    _expected: u32,
    _timeout_ns: Option<u64>,
) -> std::io::Result<()> {
    Err(ZeroLogError::UnsupportedMode("Linux futex required").into())
}

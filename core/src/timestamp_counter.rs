use std::{
    sync::atomic::{AtomicU32, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

/// Number of nanoseconds in one second.
const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;

/// High-precision timestamp counter.
///
/// On x86_64, uses the RDTSC instruction (~20ns). Calibrates the mapping
/// between TSC and system clock via a seqlock.
#[repr(C, align(64))]
pub struct TimestampCounter {
    parameter_sequence: AtomicU32,
    nanoseconds_per_tick: f64,
    base_timestamp: i64,
    base_nanoseconds: i64,
    calibrate_interval_nanoseconds: i64,
    base_nanoseconds_error: i64,
    next_calibrate_timestamp: i64,
}

impl Default for TimestampCounter {
    fn default() -> Self {
        Self::new()
    }
}

impl TimestampCounter {
    /// Creates a new, uninitialized `TimestampCounter`.
    pub fn new() -> Self {
        Self {
            parameter_sequence: AtomicU32::new(0),
            nanoseconds_per_tick: 0.0,
            base_timestamp: 0,
            base_nanoseconds: 0,
            calibrate_interval_nanoseconds: 3 * NANOSECONDS_PER_SECOND,
            base_nanoseconds_error: 0,
            next_calibrate_timestamp: 0,
        }
    }

    /// Initializes the counter by calibrating the TSC-to-system-clock mapping.
    pub fn init(&mut self, initial_calibrate_nanoseconds: i64) {
        let (base_timestamp, base_nanoseconds) = Self::synchronize_time();
        let expire_nanoseconds = base_nanoseconds + initial_calibrate_nanoseconds;
        while Self::read_system_nanoseconds() < expire_nanoseconds {
            std::thread::yield_now();
        }
        let (delayed_timestamp, delayed_nanoseconds) = Self::synchronize_time();
        let initial_nanoseconds_per_tick = (delayed_nanoseconds - base_nanoseconds) as f64
            / (delayed_timestamp - base_timestamp) as f64;
        self.save_parameters(base_timestamp, base_nanoseconds, 0, initial_nanoseconds_per_tick);
    }

    /// Initializes with the default calibration duration (20ms).
    pub fn init_default(&mut self) {
        self.init(20_000_000);
    }

    /// Performs periodic calibration. Returns immediately if the calibration
    /// interval has not elapsed since the last calibration.
    pub fn calibrate(&mut self) {
        if Self::read_timestamp_counter() < self.next_calibrate_timestamp {
            return;
        }
        let (timestamp, nanoseconds) = Self::synchronize_time();
        let mut nanoseconds_error = self.timestamp_to_nanoseconds(timestamp) - nanoseconds;
        nanoseconds_error = nanoseconds_error.clamp(-1_000_000, 1_000_000);
        let new_nanoseconds_per_tick = self.nanoseconds_per_tick
            * (1.0
                - (nanoseconds_error + nanoseconds_error - self.base_nanoseconds_error) as f64
                    / ((timestamp - self.base_timestamp) as f64 * self.nanoseconds_per_tick));
        self.save_parameters(timestamp, nanoseconds, nanoseconds_error, new_nanoseconds_per_tick);
    }

    /// Reads the CPU timestamp counter.
    #[inline(always)]
    pub fn read_timestamp_counter() -> i64 {
        #[cfg(target_arch = "x86_64")]
        unsafe {
            core::arch::x86_64::_rdtsc() as i64
        }

        #[cfg(target_arch = "aarch64")]
        unsafe {
            let value: u64;
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) value);
            value as i64
        }

        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        {
            Self::read_system_nanoseconds()
        }
    }

    /// Converts a TSC value to nanoseconds. Uses a seqlock to ensure read consistency.
    ///
    /// # Fence ordering
    ///
    /// The read side of the seqlock uses `Acquire` fences (not `AcqRel`).
    /// On x86, `Acquire` fences are free (compiler barrier only) because the
    /// hardware already guarantees load-load ordering. `AcqRel` would emit an
    /// `mfence` instruction (~30-50ns) which is unnecessary here.
    /// On ARM, `Acquire` emits a lightweight `dmb ishld` instead of a full
    /// `dmb ish` barrier.
    #[inline]
    pub fn timestamp_to_nanoseconds(&self, timestamp: i64) -> i64 {
        loop {
            let before = self.parameter_sequence.load(Ordering::Acquire) & !1;
            core::sync::atomic::fence(Ordering::Acquire);
            let nanoseconds = self.base_nanoseconds
                + ((timestamp - self.base_timestamp) as f64 * self.nanoseconds_per_tick) as i64;
            core::sync::atomic::fence(Ordering::Acquire);
            let after = self.parameter_sequence.load(Ordering::Acquire);
            if before == after {
                return nanoseconds;
            }
        }
    }

    /// Reads the current nanosecond timestamp.
    #[inline]
    pub fn read_nanoseconds(&self) -> i64 {
        self.timestamp_to_nanoseconds(Self::read_timestamp_counter())
    }

    /// Reads system clock nanoseconds (used as fallback and for calibration).
    #[inline]
    pub fn read_system_nanoseconds() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as i64
    }

    /// Returns the TSC frequency in GHz.
    pub fn get_tsc_ghz(&self) -> f64 {
        1.0 / self.nanoseconds_per_tick
    }

    /// Synchronizes TSC with the system clock by taking multiple samples
    /// and selecting the pair with the smallest TSC delta.
    fn synchronize_time() -> (i64, i64) {
        #[cfg(target_os = "windows")]
        const SAMPLES: usize = 15;
        #[cfg(not(target_os = "windows"))]
        const SAMPLES: usize = 3;

        let mut timestamps = [0i64; SAMPLES + 1];
        let mut nanoseconds = [0i64; SAMPLES + 1];

        timestamps[0] = Self::read_timestamp_counter();
        for i in 1..=SAMPLES {
            nanoseconds[i] = Self::read_system_nanoseconds();
            timestamps[i] = Self::read_timestamp_counter();
        }

        let mut best = 1;
        for i in 2..=SAMPLES {
            if timestamps[i] - timestamps[i - 1] < timestamps[best] - timestamps[best - 1] {
                best = i;
            }
        }

        let timestamp_out = (timestamps[best] + timestamps[best - 1]) >> 1;
        let nanoseconds_out = nanoseconds[best];
        (timestamp_out, nanoseconds_out)
    }

    /// Updates the seqlock-protected calibration parameters.
    ///
    /// # Fence ordering
    ///
    /// The write side uses `Release` fences:
    /// - After incrementing the sequence to odd (write-in-progress), a `Release`
    ///   fence ensures all subsequent data stores are not reordered before it.
    /// - Before incrementing the sequence to even (write-complete), a `Release`
    ///   fence ensures all data stores are visible before the reader sees the
    ///   even sequence number.
    fn save_parameters(
        &mut self,
        base_timestamp: i64,
        system_nanoseconds: i64,
        base_nanoseconds_error: i64,
        new_nanoseconds_per_tick: f64,
    ) {
        self.base_nanoseconds_error = base_nanoseconds_error;
        self.next_calibrate_timestamp = base_timestamp
            + ((self.calibrate_interval_nanoseconds - 1000) as f64 / new_nanoseconds_per_tick)
                as i64;

        let sequence = self.parameter_sequence.load(Ordering::Relaxed);
        // Mark write-in-progress (odd sequence)
        self.parameter_sequence.store(sequence + 1, Ordering::Release);
        core::sync::atomic::fence(Ordering::Release);

        self.base_timestamp = base_timestamp;
        self.base_nanoseconds = system_nanoseconds + base_nanoseconds_error;
        self.nanoseconds_per_tick = new_nanoseconds_per_tick;

        // Mark write-complete (even sequence)
        core::sync::atomic::fence(Ordering::Release);
        self.parameter_sequence.store(sequence + 2, Ordering::Release);
    }
}

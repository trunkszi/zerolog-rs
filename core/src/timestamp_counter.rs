use std::sync::atomic::{AtomicU32, Ordering};
#[cfg(not(miri))]
use std::time::{SystemTime, UNIX_EPOCH};

/// Number of nanoseconds in one second.
const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;

/// High-precision timestamp counter.
///
/// On x86_64, uses the RDTSC instruction (~20ns). Calibrates the mapping
/// between TSC and system clock under ordinary Rust exclusive ownership.
/// Calibration requires `&mut self`; sequence markers do not permit concurrent
/// access to the plain calibration fields. Tick sampling is unfenced and does
/// not establish publication order across producers.
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
        let expire_nanoseconds =
            base_nanoseconds.saturating_add(initial_calibrate_nanoseconds.max(0));
        while Self::read_system_nanoseconds() < expire_nanoseconds {
            std::thread::yield_now();
        }
        let (delayed_timestamp, delayed_nanoseconds) = Self::synchronize_time();
        let elapsed_nanoseconds = delayed_nanoseconds.saturating_sub(base_nanoseconds).max(1);
        let elapsed_ticks = delayed_timestamp.saturating_sub(base_timestamp).max(1);
        let initial_nanoseconds_per_tick = elapsed_nanoseconds as f64 / elapsed_ticks as f64;
        self.save_parameters(base_timestamp, base_nanoseconds, 0, initial_nanoseconds_per_tick);
    }

    /// Initializes with the default calibration duration (20ms).
    pub fn init_default(&mut self) {
        #[cfg(not(miri))]
        self.init(20_000_000);
        #[cfg(miri)]
        self.init(0);
    }

    /// Performs periodic calibration. Returns immediately if the calibration
    /// interval has not elapsed since the last calibration.
    pub fn calibrate(&mut self) {
        if !self.nanoseconds_per_tick.is_finite() || self.nanoseconds_per_tick <= 0.0 {
            self.init_default();
            return;
        }
        if Self::read_timestamp_counter() < self.next_calibrate_timestamp {
            return;
        }
        let (timestamp, nanoseconds) = Self::synchronize_time();
        let elapsed_ticks = timestamp.saturating_sub(self.base_timestamp);
        if elapsed_ticks <= 0 {
            return;
        }
        let mut nanoseconds_error =
            self.timestamp_to_nanoseconds(timestamp).saturating_sub(nanoseconds);
        nanoseconds_error = nanoseconds_error.clamp(-1_000_000, 1_000_000);
        let new_nanoseconds_per_tick = self.nanoseconds_per_tick
            * (1.0
                - (nanoseconds_error + nanoseconds_error - self.base_nanoseconds_error) as f64
                    / (elapsed_ticks as f64 * self.nanoseconds_per_tick));
        if !new_nanoseconds_per_tick.is_finite() || new_nanoseconds_per_tick <= 0.0 {
            return;
        }
        self.save_parameters(timestamp, nanoseconds, nanoseconds_error, new_nanoseconds_per_tick);
    }

    /// Reads the CPU timestamp counter.
    #[inline(always)]
    pub fn read_timestamp_counter() -> i64 {
        #[cfg(all(target_arch = "x86_64", not(miri)))]
        // SAFETY: RDTSC is available on x86_64 and does not access memory.
        unsafe {
            core::arch::x86_64::_rdtsc() as i64
        }

        #[cfg(all(target_arch = "aarch64", not(miri)))]
        // SAFETY: this reads the user-accessible architectural timer register.
        unsafe {
            let value: u64;
            core::arch::asm!("mrs {}, cntvct_el0", out(reg) value);
            value as i64
        }

        #[cfg(any(miri, not(any(target_arch = "x86_64", target_arch = "aarch64"))))]
        {
            Self::read_system_nanoseconds()
        }
    }

    /// Converts a tick sample using the exclusively owned calibration mapping.
    /// Updates require `&mut self`; the revision check is not synchronization for
    /// racing non-atomic field access. Converted values are not an output watermark.
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
    /// Miri uses a deterministic logical clock because isolated execution has
    /// no access to the host wall clock.
    #[inline]
    pub fn read_system_nanoseconds() -> i64 {
        #[cfg(not(miri))]
        {
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as i64
        }
        #[cfg(miri)]
        {
            static CLOCK: std::sync::atomic::AtomicI64 =
                std::sync::atomic::AtomicI64::new(1_700_000_000_000_000_000);
            CLOCK.fetch_add(1_000, Ordering::Relaxed)
        }
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

    /// Updates calibration under exclusive mutable access. The revision markers
    /// do not change the aliasing requirements for the ordinary data fields.
    fn save_parameters(
        &mut self,
        base_timestamp: i64,
        system_nanoseconds: i64,
        base_nanoseconds_error: i64,
        new_nanoseconds_per_tick: f64,
    ) {
        self.base_nanoseconds_error = base_nanoseconds_error;
        self.next_calibrate_timestamp = base_timestamp.saturating_add(
            ((self.calibrate_interval_nanoseconds - 1000) as f64 / new_nanoseconds_per_tick) as i64,
        );

        let sequence = self.parameter_sequence.load(Ordering::Relaxed);
        // Mark write-in-progress (odd sequence)
        self.parameter_sequence.store(sequence.wrapping_add(1), Ordering::Release);
        core::sync::atomic::fence(Ordering::Release);

        self.base_timestamp = base_timestamp;
        self.base_nanoseconds = system_nanoseconds + base_nanoseconds_error;
        self.nanoseconds_per_tick = new_nanoseconds_per_tick;

        // Mark write-complete (even sequence)
        core::sync::atomic::fence(Ordering::Release);
        self.parameter_sequence.store(sequence.wrapping_add(2), Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_duration_initialization_produces_a_positive_finite_scale() {
        let mut counter = TimestampCounter::new();
        counter.init(0);
        assert!(counter.nanoseconds_per_tick.is_finite());
        assert!(counter.nanoseconds_per_tick > 0.0);
        assert!(counter.get_tsc_ghz().is_finite());
        counter.calibrate();
        assert!(counter.read_nanoseconds() > 0);
    }

    #[test]
    fn calibration_initializes_an_uninitialized_counter() {
        let mut counter = TimestampCounter::new();
        counter.calibrate();
        assert!(counter.nanoseconds_per_tick.is_finite());
        assert!(counter.nanoseconds_per_tick > 0.0);
    }

    #[test]
    fn calibration_ignores_nonpositive_elapsed_ticks() {
        let mut counter = TimestampCounter::new();
        counter.save_parameters(
            TimestampCounter::read_timestamp_counter().saturating_add(i64::MAX / 4),
            TimestampCounter::read_system_nanoseconds(),
            0,
            1.0,
        );
        counter.next_calibrate_timestamp = 0;
        counter.calibrate();
        assert_eq!(counter.nanoseconds_per_tick, 1.0);
        assert_eq!(counter.parameter_sequence.load(Ordering::Relaxed), 2);
    }
}

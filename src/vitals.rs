//! Cardiac vital sign diagnostics — BPM, IBI statistics, rhythm classification.
//!
//! All timing is in microseconds (wall-clock). No tick counters.
//! The vitals system measures heart rhythm from real beat timing.

use std::time::Instant;

/// Cardiac rhythm classification.
///
/// Clinically grounded states derived from inter-beat interval (IBI) statistics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum CardiacRhythm {
    /// Regular rhythm with healthy variability. IBI CV 0.05-0.20.
    NormalSinus,
    /// Regular but too fast. BPM above tachycardia threshold.
    SinusTachycardia,
    /// Regular but too slow. BPM below bradycardia threshold.
    SinusBradycardia,
    /// Irregular rhythm. IBI CV > 0.40.
    Arrhythmia,
    /// Chaotic, no discernible pattern. IBI CV > 0.80.
    Fibrillation,
    /// No beats detected for extended period.
    Asystole,
    /// Not enough data to classify (fewer than 3 beats recorded).
    Indeterminate,
}

/// A single beat event emitted when the myocardium contracts.
#[derive(Clone, Debug)]
pub struct BeatEvent {
    /// Wall-clock instant of the beat.
    pub instant: Instant,
    /// Sequential beat number (0-indexed).
    pub beat_number: u64,
    /// Microseconds since previous beat (0 for first beat).
    pub ibi_us: u64,
    /// Contractile force of this beat (0-1000).
    pub stroke_force: u16,
}

/// Cardiac vital sign monitor.
///
/// Tracks inter-beat intervals (IBI) in a ring buffer and computes
/// BPM, IBI statistics, and rhythm classification from real wall-clock timing.
pub struct CardiacVitals {
    /// Ring buffer of last 8 inter-beat intervals (in microseconds).
    ibi_history: [u64; 8],
    /// Write head position in the ring buffer.
    ibi_head: usize,
    /// Number of IBIs recorded (0-8). Saturates at 8.
    ibi_count: u8,
    /// Instant of the most recent beat.
    last_beat_instant: Option<Instant>,
    /// Total beats recorded.
    pub beat_count: u64,

    // Classification thresholds
    /// BPM above this → tachycardia.
    pub tachycardia_bpm: u16,
    /// BPM below this → bradycardia.
    pub bradycardia_bpm: u16,
    /// Microseconds without a beat before declaring asystole.
    /// Default: 5 seconds (5_000_000 us).
    pub asystole_timeout_us: u64,
}

impl CardiacVitals {
    /// Create a new vitals monitor with default thresholds.
    pub fn new() -> Self {
        Self {
            ibi_history: [0; 8],
            ibi_head: 0,
            ibi_count: 0,
            last_beat_instant: None,
            beat_count: 0,
            tachycardia_bpm: 150,
            bradycardia_bpm: 40,
            asystole_timeout_us: 5_000_000, // 5 seconds
        }
    }

    /// Record a beat event. Updates IBI history and beat count.
    pub fn record_beat(&mut self, now: Instant) -> BeatEvent {
        let ibi_us = if let Some(last) = self.last_beat_instant {
            let elapsed = now.duration_since(last);
            elapsed.as_micros() as u64
        } else {
            0
        };

        // Record IBI (skip the first beat — no interval yet)
        if self.beat_count > 0 && ibi_us > 0 {
            self.ibi_history[self.ibi_head] = ibi_us;
            self.ibi_head = (self.ibi_head + 1) % 8;
            if self.ibi_count < 8 {
                self.ibi_count += 1;
            }
        }

        let beat_number = self.beat_count;
        self.beat_count += 1;
        self.last_beat_instant = Some(now);

        BeatEvent {
            instant: now,
            beat_number,
            ibi_us,
            // a neutral default; the full pipeline OVERWRITES this with NE-driven contractility (positive inotropy)
            // at the beat. Standalone callers (vitals-only tests) get this resting-ish value (0..1000 range).
            stroke_force: 800,
        }
    }

    /// Mean inter-beat interval in microseconds. Returns 0 if no IBIs recorded.
    pub fn ibi_mean_us(&self) -> u64 {
        if self.ibi_count == 0 {
            return 0;
        }
        let sum: u64 = self.active_ibis().iter().sum();
        sum / self.ibi_count as u64
    }

    /// Estimated beats per minute from IBI history.
    /// Returns 0 if no IBIs recorded.
    pub fn bpm(&self) -> u16 {
        let mean_us = self.ibi_mean_us();
        if mean_us == 0 {
            return 0;
        }
        // BPM = 60_000_000 us/min / mean_ibi_us
        let bpm = 60_000_000u64 / mean_us;
        bpm.min(u16::MAX as u64) as u16
    }

    /// IBI coefficient of variation, mapped to 0-255.
    ///
    /// CV = stddev / mean. Mapped: 0 = perfectly regular, 255 = maximally variable.
    /// Returns 0 if fewer than 2 IBIs recorded.
    pub fn ibi_cv(&self) -> u8 {
        if self.ibi_count < 2 {
            return 0;
        }

        let mean = self.ibi_mean_us();
        if mean == 0 {
            return 0;
        }

        let ibis = self.active_ibis();
        let mut variance_sum: u128 = 0;
        for &ibi in ibis.iter() {
            let diff = if ibi >= mean { ibi - mean } else { mean - ibi };
            variance_sum += (diff as u128) * (diff as u128);
        }
        let variance = (variance_sum / self.ibi_count as u128) as u64;
        let stddev = isqrt(variance);

        // CV = stddev / mean, scaled to 0-255 (CV of 1.0 = 255)
        let cv_scaled = (stddev * 255) / mean;
        cv_scaled.min(255) as u8
    }

    /// RMSSD — root mean square of successive IBI differences.
    ///
    /// A standard short-term HRV metric reflecting vagal tone.
    /// Returns value in microseconds. 0 if fewer than 2 IBIs.
    pub fn rmssd_us(&self) -> u64 {
        if self.ibi_count < 2 {
            return 0;
        }

        let ibis = self.active_ibis();
        let n = ibis.len();
        let mut sum_sq_diff: u128 = 0;
        let mut pairs: u64 = 0;

        for i in 1..n {
            let a = ibis[i - 1];
            let b = ibis[i];
            let diff = if a >= b { a - b } else { b - a };
            sum_sq_diff += (diff as u128) * (diff as u128);
            pairs += 1;
        }

        if pairs == 0 {
            return 0;
        }

        isqrt((sum_sq_diff / pairs as u128) as u64)
    }

    /// Classify the current cardiac rhythm.
    pub fn classify(&self, now: Instant) -> CardiacRhythm {
        // Check asystole — no beats for too long
        if let Some(last) = self.last_beat_instant {
            let silence_us = now.duration_since(last).as_micros() as u64;
            if silence_us > self.asystole_timeout_us {
                return CardiacRhythm::Asystole;
            }
        } else if self.beat_count == 0 {
            return CardiacRhythm::Indeterminate;
        }

        if self.ibi_count < 2 {
            return CardiacRhythm::Indeterminate;
        }

        let cv = self.ibi_cv();

        // Fibrillation: extremely high variability (CV > 0.80 → cv > 204)
        if cv > 204 {
            return CardiacRhythm::Fibrillation;
        }

        // Arrhythmia: high variability (CV > 0.40 → cv > 102)
        if cv > 102 {
            return CardiacRhythm::Arrhythmia;
        }

        // Regular rhythm — check rate
        let bpm = self.bpm();

        if bpm > self.tachycardia_bpm {
            return CardiacRhythm::SinusTachycardia;
        }

        if bpm > 0 && bpm < self.bradycardia_bpm {
            return CardiacRhythm::SinusBradycardia;
        }

        CardiacRhythm::NormalSinus
    }

    /// Microseconds since last beat. Returns None if no beats yet.
    pub fn silence_us(&self, now: Instant) -> Option<u64> {
        self.last_beat_instant
            .map(|last| now.duration_since(last).as_micros() as u64)
    }

    /// Get the active IBI slice (only filled entries).
    fn active_ibis(&self) -> &[u64] {
        let count = self.ibi_count as usize;
        if count < 8 {
            &self.ibi_history[..count]
        } else {
            &self.ibi_history
        }
    }
}

impl Default for CardiacVitals {
    fn default() -> Self {
        Self::new()
    }
}

/// Integer square root via Newton's method.
fn isqrt(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isqrt_correctness() {
        assert_eq!(isqrt(0), 0);
        assert_eq!(isqrt(1), 1);
        assert_eq!(isqrt(4), 2);
        assert_eq!(isqrt(9), 3);
        assert_eq!(isqrt(100), 10);
        assert_eq!(isqrt(99), 9); // floor
        assert_eq!(isqrt(10000), 100);
    }

    #[test]
    fn vitals_no_beats() {
        let v = CardiacVitals::new();
        assert_eq!(v.ibi_mean_us(), 0);
        assert_eq!(v.bpm(), 0);
        assert_eq!(v.ibi_cv(), 0);
        assert_eq!(v.classify(Instant::now()), CardiacRhythm::Indeterminate);
    }

    #[test]
    fn vitals_regular_rhythm() {
        let mut v = CardiacVitals::new();
        let base = Instant::now();
        // Simulate regular beats every 800ms (75 BPM)
        for i in 0..10 {
            let beat_time = base + std::time::Duration::from_millis(i * 800);
            v.record_beat(beat_time);
        }
        assert_eq!(v.ibi_mean_us(), 800_000);
        assert_eq!(v.bpm(), 75);
        assert_eq!(v.ibi_cv(), 0);
    }

    #[test]
    fn vitals_variable_rhythm() {
        let mut v = CardiacVitals::new();
        let base = Instant::now();
        // Alternating 600ms and 1000ms intervals
        let mut t = 0u64;
        v.record_beat(base);
        for i in 0..8 {
            t += if i % 2 == 0 { 600 } else { 1000 };
            v.record_beat(base + std::time::Duration::from_millis(t));
        }
        assert_eq!(v.ibi_mean_us(), 800_000);
        assert!(v.ibi_cv() > 0);
    }
}

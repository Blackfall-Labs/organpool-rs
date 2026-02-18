//! Respiratory vital sign diagnostics — breaths per minute, cycle statistics, rhythm classification.
//!
//! All timing is in microseconds (wall-clock). No tick counters.
//! Parallels the cardiac `vitals.rs` module.

use std::time::Instant;

use crate::respiratory::RespiratoryPhase;

/// Respiratory rhythm classification.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RespiratoryRhythm {
    /// Normal breathing with moderate variability.
    Eupnea,
    /// Breathing rate above tachypnea threshold (default: 25/min).
    Tachypnea,
    /// Breathing rate below bradypnea threshold (default: 8/min).
    Bradypnea,
    /// No breaths detected for extended period.
    Apnea,
    /// Not enough data to classify (fewer than 3 breaths recorded).
    Indeterminate,
}

/// A single breath event emitted when a full breath cycle completes.
#[derive(Clone, Debug)]
pub struct BreathEvent {
    /// Wall-clock instant of cycle completion.
    pub instant: Instant,
    /// Sequential breath number (0-indexed).
    pub breath_number: u64,
    /// Total breath cycle duration in microseconds.
    pub cycle_us: u64,
    /// Tidal volume for this breath (0-255 lung units).
    pub tidal_volume: u8,
    /// Peak expiratory flow during this breath (0-255).
    pub peak_expiratory_flow: u8,
    /// Phase at emission (always EndExpiratory → Inspiration transition).
    pub phase: RespiratoryPhase,
}

/// Respiratory vital sign monitor.
///
/// Tracks breath cycle durations in a ring buffer and computes
/// breaths per minute, cycle statistics, and rhythm classification.
pub struct RespiratoryVitals {
    /// Ring buffer of last 8 breath cycle durations (in microseconds).
    cycle_history: [u64; 8],
    /// Write head position.
    cycle_head: usize,
    /// Number of cycles recorded (0-8). Saturates at 8.
    cycle_count: u8,
    /// Instant of the most recent breath cycle completion.
    last_breath_instant: Option<Instant>,
    /// Total breaths recorded.
    pub breath_count: u64,
    /// Last recorded tidal volume.
    pub last_tidal_volume: u8,
    /// Last recorded peak expiratory flow.
    pub last_peak_flow: u8,

    // Classification thresholds
    /// Breaths per minute above this → tachypnea.
    pub tachypnea_bpm: u16,
    /// Breaths per minute below this → bradypnea.
    pub bradypnea_bpm: u16,
    /// Microseconds without a breath before declaring apnea.
    pub apnea_timeout_us: u64,
}

impl RespiratoryVitals {
    /// Create a new vitals monitor with default thresholds.
    pub fn new() -> Self {
        Self {
            cycle_history: [0; 8],
            cycle_head: 0,
            cycle_count: 0,
            last_breath_instant: None,
            breath_count: 0,
            last_tidal_volume: 0,
            last_peak_flow: 0,
            tachypnea_bpm: 25,
            bradypnea_bpm: 8,
            apnea_timeout_us: 10_000_000, // 10 seconds
        }
    }

    /// Record a breath event. Updates cycle history and breath count.
    pub fn record_breath(
        &mut self,
        now: Instant,
        tidal_volume: u8,
        peak_flow: u8,
    ) -> BreathEvent {
        let cycle_us = if let Some(last) = self.last_breath_instant {
            now.duration_since(last).as_micros() as u64
        } else {
            0
        };

        // Record cycle duration (skip the first breath — no interval yet)
        if self.breath_count > 0 && cycle_us > 0 {
            self.cycle_history[self.cycle_head] = cycle_us;
            self.cycle_head = (self.cycle_head + 1) % 8;
            if self.cycle_count < 8 {
                self.cycle_count += 1;
            }
        }

        let breath_number = self.breath_count;
        self.breath_count += 1;
        self.last_breath_instant = Some(now);
        self.last_tidal_volume = tidal_volume;
        self.last_peak_flow = peak_flow;

        BreathEvent {
            instant: now,
            breath_number,
            cycle_us,
            tidal_volume,
            peak_expiratory_flow: peak_flow,
            phase: RespiratoryPhase::Inspiration, // emitted at cycle start
        }
    }

    /// Mean breath cycle duration in microseconds. Returns 0 if no cycles recorded.
    pub fn cycle_mean_us(&self) -> u64 {
        if self.cycle_count == 0 {
            return 0;
        }
        let sum: u64 = self.active_cycles().iter().sum();
        sum / self.cycle_count as u64
    }

    /// Estimated breaths per minute from cycle history.
    pub fn breaths_per_minute(&self) -> u16 {
        let mean_us = self.cycle_mean_us();
        if mean_us == 0 {
            return 0;
        }
        let bpm = 60_000_000u64 / mean_us;
        bpm.min(u16::MAX as u64) as u16
    }

    /// Cycle coefficient of variation, mapped to 0-255.
    pub fn cycle_cv(&self) -> u8 {
        if self.cycle_count < 2 {
            return 0;
        }

        let mean = self.cycle_mean_us();
        if mean == 0 {
            return 0;
        }

        let cycles = self.active_cycles();
        let mut variance_sum: u128 = 0;
        for &cycle in cycles.iter() {
            let diff = if cycle >= mean { cycle - mean } else { mean - cycle };
            variance_sum += (diff as u128) * (diff as u128);
        }
        let variance = (variance_sum / self.cycle_count as u128) as u64;
        let stddev = isqrt(variance);

        let cv_scaled = (stddev * 255) / mean;
        cv_scaled.min(255) as u8
    }

    /// Classify the current respiratory rhythm.
    pub fn classify(&self, now: Instant) -> RespiratoryRhythm {
        // Check apnea — no breaths for too long
        if let Some(last) = self.last_breath_instant {
            let silence_us = now.duration_since(last).as_micros() as u64;
            if silence_us > self.apnea_timeout_us {
                return RespiratoryRhythm::Apnea;
            }
        } else if self.breath_count == 0 {
            return RespiratoryRhythm::Indeterminate;
        }

        if self.cycle_count < 2 {
            return RespiratoryRhythm::Indeterminate;
        }

        let bpm = self.breaths_per_minute();

        if bpm > self.tachypnea_bpm {
            return RespiratoryRhythm::Tachypnea;
        }

        if bpm > 0 && bpm < self.bradypnea_bpm {
            return RespiratoryRhythm::Bradypnea;
        }

        RespiratoryRhythm::Eupnea
    }

    /// Get the active cycle slice (only filled entries).
    fn active_cycles(&self) -> &[u64] {
        let count = self.cycle_count as usize;
        if count < 8 {
            &self.cycle_history[..count]
        } else {
            &self.cycle_history
        }
    }
}

impl Default for RespiratoryVitals {
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
    use std::time::Duration;

    #[test]
    fn respiratory_vitals_no_breaths() {
        let v = RespiratoryVitals::new();
        assert_eq!(v.cycle_mean_us(), 0);
        assert_eq!(v.breaths_per_minute(), 0);
        assert_eq!(v.cycle_cv(), 0);
        assert_eq!(v.classify(Instant::now()), RespiratoryRhythm::Indeterminate);
    }

    #[test]
    fn regular_breathing_tracks_rate() {
        let mut v = RespiratoryVitals::new();
        let base = Instant::now();
        // Simulate regular breaths every 4 seconds (15 BPM)
        for i in 0..10 {
            let breath_time = base + Duration::from_millis(i * 4000);
            v.record_breath(breath_time, 30, 50);
        }
        assert_eq!(v.cycle_mean_us(), 4_000_000);
        assert_eq!(v.breaths_per_minute(), 15);
        assert_eq!(v.cycle_cv(), 0); // perfectly regular
    }

    #[test]
    fn classify_tachypnea() {
        let mut v = RespiratoryVitals::new();
        let base = Instant::now();
        // Simulate fast breaths every 2 seconds (30 BPM)
        for i in 0..10 {
            let breath_time = base + Duration::from_millis(i * 2000);
            v.record_breath(breath_time, 20, 40);
        }
        let now = base + Duration::from_millis(9 * 2000 + 500);
        assert_eq!(v.classify(now), RespiratoryRhythm::Tachypnea);
    }

    #[test]
    fn classify_apnea() {
        let mut v = RespiratoryVitals::new();
        let base = Instant::now();
        // Record a few breaths
        for i in 0..5 {
            v.record_breath(base + Duration::from_millis(i * 4000), 30, 50);
        }
        // Then silence for 15 seconds
        let now = base + Duration::from_secs(35);
        assert_eq!(v.classify(now), RespiratoryRhythm::Apnea);
    }
}

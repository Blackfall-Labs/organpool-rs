//! Cardiac cell physics — ion channel cycle, zone types, configuration.
//!
//! Each cardiac zone simulates the HCN → Ca²⁺ → K⁺ → refractory cycle
//! that produces autonomous rhythmic depolarization. The SA node is the
//! master pacemaker with the fastest intrinsic rate. AV node and Purkinje
//! fibers have slower intrinsic rates (subsidiary/escape pacemakers) that
//! are normally suppressed by SA node overdrive — they only manifest when
//! the SA node fails or slows below their intrinsic rate.
//!
//! Physics use wall-clock time — zones track real `Instant` timestamps
//! and compute membrane dynamics from elapsed `Duration`. There is no
//! tick counter. The ion channel cycle runs continuously.

use std::time::{Duration, Instant};

/// Phase of the cardiac ion channel cycle.
///
/// ```text
/// Diastolic ──(leak reaches threshold)──→ Upstroke
///     ↑                                       │
///     │                                       ↓
///     └───(refractory expires)─── Refractory ←┘
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardiacPhase {
    /// HCN leak is slowly depolarizing the membrane toward threshold.
    /// This is the diastolic interval — the "ramp" between beats.
    Diastolic,
    /// Ca²⁺ channels open — sharp depolarization. The beat.
    Upstroke,
    /// K⁺ repolarization — absolute refractory period.
    /// The cell CANNOT fire again during this window, regardless of input.
    /// This is the anti-seizure mechanism.
    Refractory,
}

/// Configuration for the SR calcium clock — a second pacemaker oscillator.
///
/// The SA node has two coupled oscillators: the membrane clock (HCN leak current)
/// and the calcium clock (sarcoplasmic reticulum Ca²⁺ cycling). The SR accumulates
/// calcium during diastole, and when it reaches the release threshold, Ca²⁺ is
/// released through RyR2 channels. The released calcium drives NCX (Na⁺/Ca²⁺
/// exchanger) which produces an inward depolarizing current — the "calcium clock"
/// contribution to diastolic depolarization.
///
/// Unlike gap junction coupling, calcium clock depolarization CAN push the membrane
/// past threshold and trigger firing. The calcium clock is an integral part of the
/// pacemaker mechanism, not an external influence.
#[derive(Clone, Debug)]
pub struct CalciumClockConfig {
    /// Whether the calcium clock is active for this zone.
    pub enabled: bool,
    /// SR calcium release threshold (0-255). When `sr_load` reaches this value,
    /// Ca²⁺ is released and NCX depolarization occurs.
    pub release_threshold: u8,
    /// Base SR refill rate — calcium units per second of diastolic time.
    /// Tracks the intrinsic SERCA pump rate.
    pub base_refill_rate_per_sec: u32,
    /// NCX depolarization — membrane mV boost when calcium is released.
    /// This CAN push past threshold, unlike gap junction coupling.
    pub ncx_depolarization: i16,
}

impl Default for CalciumClockConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            release_threshold: 180,
            base_refill_rate_per_sec: 400,
            ncx_depolarization: 5,
        }
    }
}

/// Runtime state for the calcium clock oscillator.
pub struct CalciumClock {
    /// Current SR calcium load (0-255). Accumulates during diastole.
    pub sr_load: u8,
    /// Current effective refill rate (after autonomic modulation).
    pub refill_rate_per_sec: u32,
    /// Whether Ca²⁺ has been released this cardiac cycle.
    pub released_this_cycle: bool,
    /// Config parameters
    pub release_threshold: u8,
    pub base_refill_rate_per_sec: u32,
    pub ncx_depolarization: i16,
}

impl CalciumClock {
    fn new(config: &CalciumClockConfig) -> Self {
        Self {
            sr_load: 0,
            refill_rate_per_sec: config.base_refill_rate_per_sec,
            released_this_cycle: false,
            release_threshold: config.release_threshold,
            base_refill_rate_per_sec: config.base_refill_rate_per_sec,
            ncx_depolarization: config.ncx_depolarization,
        }
    }

    /// Reset the calcium clock at the start of a new cardiac cycle (after firing).
    fn reset(&mut self) {
        self.sr_load = 0;
        self.released_this_cycle = false;
    }

    /// Compute SR calcium load from total diastolic elapsed time.
    /// Returns the NCX depolarization to apply if calcium was just released.
    ///
    /// `elapsed_us` is the total time since diastolic phase started — the same
    /// value used by the membrane clock. The SR load is computed from scratch
    /// each call (not accumulated), matching the membrane clock's approach.
    fn compute(&mut self, elapsed_us: u64) -> i16 {
        if self.released_this_cycle {
            return 0;
        }

        let load = (self.refill_rate_per_sec as u64 * elapsed_us) / 1_000_000;
        self.sr_load = load.min(255) as u8;

        if self.sr_load >= self.release_threshold {
            self.released_this_cycle = true;
            self.ncx_depolarization
        } else {
            0
        }
    }
}

/// 256-entry integer sine lookup table (values -127 to +127).
///
/// `sin(2π × i/256) × 127`, rounded. Phase maps to index: `table[phase >> 8]`
/// for u16 phase (0-65535). Full cycle: phase 0 → 65535 maps to 0 → 2π.
#[rustfmt::skip]
const SINE_TABLE: [i8; 256] = [
    0, 3, 6, 9, 12, 16, 19, 22, 25, 28, 31, 34, 37, 40, 43, 46,
    49, 51, 54, 57, 60, 63, 65, 68, 71, 73, 76, 78, 81, 83, 85, 88,
    90, 92, 94, 96, 98, 100, 102, 104, 106, 107, 109, 111, 112, 113, 115, 116,
    117, 118, 120, 121, 122, 122, 123, 124, 125, 125, 126, 126, 126, 127, 127, 127,
    127, 127, 127, 127, 126, 126, 126, 125, 125, 124, 123, 122, 122, 121, 120, 118,
    117, 116, 115, 113, 112, 111, 109, 107, 106, 104, 102, 100, 98, 96, 94, 92,
    90, 88, 85, 83, 81, 78, 76, 73, 71, 68, 65, 63, 60, 57, 54, 51,
    49, 46, 43, 40, 37, 34, 31, 28, 25, 22, 19, 16, 12, 9, 6, 3,
    0, -3, -6, -9, -12, -16, -19, -22, -25, -28, -31, -34, -37, -40, -43, -46,
    -49, -51, -54, -57, -60, -63, -65, -68, -71, -73, -76, -78, -81, -83, -85, -88,
    -90, -92, -94, -96, -98, -100, -102, -104, -106, -107, -109, -111, -112, -113, -115, -116,
    -117, -118, -120, -121, -122, -122, -123, -124, -125, -125, -126, -126, -126, -127, -127, -127,
    -127, -127, -127, -127, -126, -126, -126, -125, -125, -124, -123, -122, -122, -121, -120, -118,
    -117, -116, -115, -113, -112, -111, -109, -107, -106, -104, -102, -100, -98, -96, -94, -92,
    -90, -88, -85, -83, -81, -78, -76, -73, -71, -68, -65, -63, -60, -57, -54, -51,
    -49, -46, -43, -40, -37, -34, -31, -28, -25, -22, -19, -16, -12, -9, -6, -3,
];

/// Integer sine function. Phase 0-65535 maps to full cycle.
/// Returns -127 to +127.
fn integer_sine(phase: u16) -> i8 {
    SINE_TABLE[(phase >> 8) as usize]
}

/// Source of respiratory sinus arrhythmia (RSA) modulation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RsaSource {
    /// Internal sine oscillator at ~0.25 Hz. Produces synthetic RSA without
    /// real respiratory input. This is the default for backward compatibility.
    Internal,
    /// External respiratory signal via `Arc<AtomicU8>`. The lung thread writes
    /// vagal modulation values; the heart reads them and uses them to modulate
    /// the ACh level. Requires coupling to a `RespiratoryPipeline`.
    External,
}

/// Configuration for heart rate variability generation.
///
/// HRV arises from three physiological sources:
/// - **RSA** (respiratory sinus arrhythmia, ~0.25 Hz): vagally mediated
///   beat-to-beat variability synchronized with breathing.
/// - **LF oscillation** (~0.1 Hz): autonomic baroreflex oscillation.
/// - **Intrinsic jitter**: stochastic ion channel noise.
///
/// Variability is implemented as threshold jitter — each cardiac cycle gets
/// a slightly different effective threshold, modulating the diastolic period.
#[derive(Clone, Debug)]
pub struct HrvConfig {
    /// Whether HRV is active for this zone.
    pub enabled: bool,
    /// PRNG seed for deterministic jitter.
    pub seed: u64,
    /// RSA source — internal sine oscillator or external respiratory signal.
    pub rsa_source: RsaSource,
    /// RSA amplitude — max threshold jitter in mV from respiratory oscillation.
    /// Only used when `rsa_source == RsaSource::Internal`.
    pub rsa_amplitude: i16,
    /// RSA frequency — phase increment per beat. ~14000 for 0.25 Hz at 70 BPM.
    /// Only used when `rsa_source == RsaSource::Internal`.
    pub rsa_frequency: u16,
    /// LF amplitude — max threshold jitter in mV from baroreflex oscillation.
    pub lf_amplitude: i16,
    /// LF frequency — phase increment per beat. ~5600 for 0.1 Hz at 70 BPM.
    pub lf_frequency: u16,
    /// Intrinsic jitter — max random threshold jitter in mV.
    pub intrinsic_jitter: i16,
}

impl Default for HrvConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            seed: 0xDEAD_BEEF_CAFE_BABE,
            rsa_source: RsaSource::Internal,
            rsa_amplitude: 3,
            rsa_frequency: 14000,
            lf_amplitude: 2,
            lf_frequency: 5600,
            intrinsic_jitter: 1,
        }
    }
}

/// Runtime state for heart rate variability generation.
pub struct HrvGenerator {
    /// xorshift64 PRNG state.
    rng_state: u64,
    /// RSA source — internal sine oscillator or external respiratory signal.
    rsa_source: RsaSource,
    /// RSA oscillator phase (wraps at u16::MAX). Only used with Internal RSA.
    rsa_phase: u16,
    rsa_frequency: u16,
    rsa_amplitude: i16,
    /// LF oscillator phase.
    lf_phase: u16,
    lf_frequency: u16,
    lf_amplitude: i16,
    /// Max random jitter in mV.
    intrinsic_jitter: i16,
}

impl HrvGenerator {
    fn new(config: &HrvConfig) -> Self {
        Self {
            rng_state: config.seed,
            rsa_source: config.rsa_source.clone(),
            rsa_phase: 0,
            rsa_frequency: config.rsa_frequency,
            rsa_amplitude: config.rsa_amplitude,
            lf_phase: 0,
            lf_frequency: config.lf_frequency,
            lf_amplitude: config.lf_amplitude,
            intrinsic_jitter: config.intrinsic_jitter,
        }
    }

    /// xorshift64 PRNG — returns next pseudo-random u64.
    fn next_random(&mut self) -> u64 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x;
        x
    }

    /// Compute threshold jitter for this cardiac cycle and advance oscillators.
    ///
    /// Called once per cycle at the Refractory → Diastolic transition.
    /// Returns the threshold offset in mV (can be positive or negative).
    fn cycle_jitter(&mut self) -> i16 {
        // RSA component — only from internal sine oscillator.
        // When External, real RSA comes via AtomicU8 ACh modulation,
        // not threshold jitter.
        let rsa = if self.rsa_source == RsaSource::Internal {
            let val = (integer_sine(self.rsa_phase) as i32 * self.rsa_amplitude as i32) / 127;
            self.rsa_phase = self.rsa_phase.wrapping_add(self.rsa_frequency);
            val
        } else {
            0
        };

        // LF component (always active — baroreflex is independent of respiration)
        let lf = (integer_sine(self.lf_phase) as i32 * self.lf_amplitude as i32) / 127;

        // Intrinsic jitter — random in [-jitter, +jitter]
        let noise = if self.intrinsic_jitter > 0 {
            let r = self.next_random();
            let range = (self.intrinsic_jitter as u64) * 2 + 1;
            let val = (r % range) as i16 - self.intrinsic_jitter;
            val
        } else {
            0
        };

        // Advance LF oscillator phase for next cycle
        self.lf_phase = self.lf_phase.wrapping_add(self.lf_frequency);

        (rsa + lf + noise as i32) as i16
    }
}

/// Configuration for a cardiac zone.
///
/// All durations are in microseconds (integer only, BF_003).
/// `leak_rate_per_sec` is the membrane potential units gained per second
/// of diastolic time — the core control knob for heart rate.
#[derive(Clone, Debug)]
pub struct ZoneConfig {
    /// Resting membrane potential (negative, e.g. -70).
    pub resting_potential: i16,
    /// Threshold for firing (e.g. -40). When membrane >= threshold, upstroke.
    pub threshold: i16,
    /// Peak potential during upstroke (e.g. +30).
    pub peak_potential: i16,
    /// Intrinsic HCN leak rate — membrane potential units per second.
    /// Nonzero for all pacemaker zones: SA node (fastest, master), AV node
    /// (subsidiary, ~45 BPM), Purkinje/conduction (escape, ~30 BPM).
    /// Subsidiary pacemakers are normally suppressed by SA node overdrive.
    pub base_leak_rate_per_sec: u32,
    /// Absolute refractory duration in microseconds.
    pub refractory_us: u64,
    /// Conduction delay in microseconds before propagating to next zone.
    /// The AV node has the largest delay to prevent atrial/ventricular overlap.
    pub conduction_delay_us: u64,
    /// NE sensitivity (0-255). How much NE accelerates the leak rate.
    pub ne_sensitivity: u8,
    /// ACh sensitivity (0-255). How much ACh decelerates the leak rate.
    pub ach_sensitivity: u8,
    /// Calcium clock configuration. Only meaningful for pacemaker zones (SA node).
    pub calcium_clock: CalciumClockConfig,
    /// Heart rate variability configuration. Only meaningful for SA node.
    pub hrv: HrvConfig,
}

/// A single cardiac zone (SA node, AV node, conduction system, or myocardium).
///
/// Each zone tracks its membrane potential and phase using wall-clock time.
/// Pacemaker zones (SA node) have intrinsic leak that drives autonomous
/// oscillation. Driven zones fire only when triggered by upstream propagation.
pub struct CardiacZone {
    // Dynamic state
    /// Current membrane potential.
    pub membrane: i16,
    /// Current phase of the ion channel cycle.
    pub phase: CardiacPhase,
    /// When the current phase started (for computing elapsed time).
    pub phase_start: Instant,
    /// Current effective leak rate per second (after autonomic modulation).
    pub leak_rate_per_sec: u32,
    /// Whether this zone has a pending trigger from upstream.
    pub pending_trigger: bool,
    /// When the pending trigger was received (for conduction delay).
    pub trigger_received_at: Option<Instant>,
    /// Calcium clock oscillator (SA node only; None for other zones).
    pub calcium_clock: Option<CalciumClock>,
    /// HRV generator (SA node only; None for other zones).
    pub hrv_generator: Option<HrvGenerator>,
    /// Effective threshold for this cardiac cycle (base threshold + HRV jitter).
    /// Updated at each Refractory → Diastolic transition.
    pub effective_threshold: i16,

    // Physics parameters (from config)
    pub resting_potential: i16,
    pub threshold: i16,
    pub peak_potential: i16,
    pub base_leak_rate_per_sec: u32,
    pub refractory_duration: Duration,
    pub conduction_delay: Duration,
    pub ne_sensitivity: u8,
    pub ach_sensitivity: u8,
}

impl CardiacZone {
    /// Create a new zone from configuration, starting in diastolic phase.
    pub fn new(config: &ZoneConfig, now: Instant) -> Self {
        let calcium_clock = if config.calcium_clock.enabled {
            Some(CalciumClock::new(&config.calcium_clock))
        } else {
            None
        };
        let hrv_generator = if config.hrv.enabled {
            Some(HrvGenerator::new(&config.hrv))
        } else {
            None
        };

        Self {
            membrane: config.resting_potential,
            phase: CardiacPhase::Diastolic,
            phase_start: now,
            leak_rate_per_sec: config.base_leak_rate_per_sec,
            pending_trigger: false,
            trigger_received_at: None,
            calcium_clock,
            hrv_generator,
            effective_threshold: config.threshold,
            resting_potential: config.resting_potential,
            threshold: config.threshold,
            peak_potential: config.peak_potential,
            base_leak_rate_per_sec: config.base_leak_rate_per_sec,
            refractory_duration: Duration::from_micros(config.refractory_us),
            conduction_delay: Duration::from_micros(config.conduction_delay_us),
            ne_sensitivity: config.ne_sensitivity,
            ach_sensitivity: config.ach_sensitivity,
        }
    }

    /// Apply autonomic modulation to the leak rate.
    ///
    /// Only meaningful for zones with nonzero autonomic sensitivity (SA node).
    /// Subsidiary pacemakers (AV node, Purkinje) have intrinsic leak rates
    /// but zero NE/ACh sensitivity — they beat at fixed intrinsic rates
    /// regardless of autonomic input.
    ///
    /// Modulation range: NE at max can ~3x the base rate, ACh at max can
    /// halve it. The leak rate is clamped to `[base/4, base*4]` — the heart
    /// cannot fully stop from modulation, and there's a physiological ceiling.
    ///
    /// Real cardiac physiology: sympathetic activation can increase HR from
    /// 70 to ~200 BPM (~3x), vagal tone can slow it to ~40 BPM (~0.5x).
    pub fn apply_modulation(&mut self, ne: u8, ach: u8, cortisol: u8, metabolism: &MetabolismConfig) {
        // Only zones with autonomic sensitivity AND an intrinsic leak rate
        // respond to modulation. Subsidiary pacemakers (AV, Purkinje) have
        // sensitivity=0 — they beat at fixed intrinsic rates. Zones with
        // leak=0 (e.g. failed SA node) have nothing to modulate.
        if self.base_leak_rate_per_sec == 0
            || (self.ne_sensitivity == 0 && self.ach_sensitivity == 0)
        {
            return;
        }

        let base = self.base_leak_rate_per_sec as u64;

        // Cortisol amplifies NE sensitivity
        let effective_ne_sens = (self.ne_sensitivity as u32)
            .saturating_add(cortisol as u32 / 4)
            .min(255);

        // NE-ACh receptor antagonism: high ACh suppresses NE effectiveness.
        // At ACh=0: full NE boost. At ACh=255 with antagonism=255: NE halved.
        // suppression = 256 - (ach * antagonism / 255 / 2)
        let ne_suppression = 256u64
            - (ach as u64 * metabolism.ach_ne_antagonism as u64) / (255 * 2);
        let ne_boost = (base * 2 * ne as u64 * effective_ne_sens as u64 * ne_suppression)
            / (255 * 255 * 256);

        // ACh-NE receptor antagonism: high NE suppresses ACh effectiveness.
        let ach_suppression = 256u64
            - (ne as u64 * metabolism.ne_ach_antagonism as u64) / (255 * 2);
        let ach_brake = (base * ach as u64 * self.ach_sensitivity as u64 * ach_suppression)
            / (255 * 255 * 2 * 256);

        let modulated = (base as i64) + (ne_boost as i64) - (ach_brake as i64);

        // Clamp: never below base/4 (heart can't stop), never above base*4
        let floor = (base / 4).max(1) as i64;
        let ceiling = (base * 4) as i64;
        self.leak_rate_per_sec = modulated.clamp(floor, ceiling) as u32;

        // Modulate calcium clock refill rate proportionally to the leak rate.
        // If leak rate doubled → calcium refill rate doubles (sympathetic co-activation).
        if let Some(ref mut ca_clock) = self.calcium_clock {
            let ratio_x256 = (self.leak_rate_per_sec as u64 * 256) / base;
            ca_clock.refill_rate_per_sec =
                ((ca_clock.base_refill_rate_per_sec as u64 * ratio_x256) / 256) as u32;
        }
    }

    /// Advance this zone's physics based on current wall-clock time.
    ///
    /// Returns `true` if the zone fired (entered Upstroke) during this update.
    /// Call this continuously from the heart's run loop.
    pub fn update(&mut self, now: Instant) -> bool {
        match self.phase {
            CardiacPhase::Refractory => {
                let elapsed = now.duration_since(self.phase_start);
                if elapsed >= self.refractory_duration {
                    self.phase = CardiacPhase::Diastolic;
                    self.phase_start = now;
                    self.membrane = self.resting_potential;

                    // Apply HRV jitter for this new cardiac cycle
                    if let Some(ref mut hrv) = self.hrv_generator {
                        let jitter = hrv.cycle_jitter();
                        // Clamp effective threshold to safe range
                        let min_thresh = self.resting_potential + 5;
                        let max_thresh = self.peak_potential - 5;
                        self.effective_threshold =
                            (self.threshold as i32 + jitter as i32)
                                .clamp(min_thresh as i32, max_thresh as i32) as i16;
                    } else {
                        self.effective_threshold = self.threshold;
                    }
                }
                false
            }
            CardiacPhase::Diastolic => {
                // Handle pending trigger from upstream (driven zones)
                if self.pending_trigger {
                    if let Some(trigger_time) = self.trigger_received_at {
                        if !self.conduction_delay.is_zero() {
                            // AV node / conduction delay — wait for delay to elapse
                            let since_trigger = now.duration_since(trigger_time);
                            if since_trigger >= self.conduction_delay {
                                self.pending_trigger = false;
                                self.trigger_received_at = None;
                                return self.fire(now);
                            }
                            // Still waiting for delay — fall through to intrinsic
                            // leak so subsidiary pacemakers continue accumulating
                            // during conduction delay (needed for escape rhythms).
                        } else {
                            // No conduction delay — fire immediately
                            self.pending_trigger = false;
                            self.trigger_received_at = None;
                            return self.fire(now);
                        }
                    }
                }

                // Intrinsic leak (all pacemaker zones — SA, AV, Purkinje)
                // During normal conduction, the SA node fires first and triggers
                // downstream zones before their slower intrinsic leak reaches
                // threshold (overdrive suppression). Subsidiary pacemakers only
                // manifest when the SA node fails or slows.
                if self.leak_rate_per_sec > 0 {
                    let elapsed = now.duration_since(self.phase_start);
                    let elapsed_us = elapsed.as_micros() as u64;
                    let leak_accumulated = (self.leak_rate_per_sec as u64 * elapsed_us) / 1_000_000;
                    let new_membrane = (self.resting_potential as i64) + (leak_accumulated as i64);
                    self.membrane = new_membrane.min(i16::MAX as i64) as i16;

                    // Calcium clock contribution — SR calcium release adds NCX
                    // depolarizing current. Unlike gap junctions, this CAN push
                    // past threshold (calcium clock is an integral pacemaker mechanism).
                    if let Some(ref mut ca_clock) = self.calcium_clock {
                        let ncx_mv = ca_clock.compute(elapsed_us);
                        if ncx_mv > 0 {
                            self.membrane = self.membrane.saturating_add(ncx_mv);
                        }
                    }

                    if self.membrane >= self.effective_threshold {
                        // Intrinsic leak reached threshold — fire.
                        // Clear any pending trigger since we're firing intrinsically.
                        self.pending_trigger = false;
                        self.trigger_received_at = None;
                        return self.fire(now);
                    }
                }

                false
            }
            CardiacPhase::Upstroke => {
                // Upstroke is instantaneous — transition to refractory
                self.phase = CardiacPhase::Refractory;
                self.phase_start = now;
                false
            }
        }
    }

    /// Trigger this zone from upstream propagation.
    ///
    /// If the zone is in refractory, the trigger is ignored (absolute refractory).
    pub fn trigger(&mut self, now: Instant) {
        if self.phase != CardiacPhase::Refractory {
            self.pending_trigger = true;
            self.trigger_received_at = Some(now);
        }
    }

    /// Apply electrotonic depolarization from gap junction coupling.
    ///
    /// Gap junctions (connexin-43 channels) allow ionic current to flow between
    /// adjacent cardiac cells. When a neighboring zone fires, the current flow
    /// depolarizes this zone's membrane, accelerating its approach to threshold.
    ///
    /// Critical constraint: gap junction current CANNOT directly fire the zone.
    /// Membrane is clamped to `threshold - 1`. The zone must still reach threshold
    /// through its own intrinsic leak or trigger mechanism. This prevents spurious
    /// extra beats from coupling alone.
    ///
    /// Only affects diastolic zones — refractory zones ignore external current.
    pub fn electrotonic_depolarize(&mut self, amount: i16) {
        if self.phase == CardiacPhase::Diastolic && amount > 0 {
            self.membrane = self.membrane.saturating_add(amount)
                .min(self.threshold - 1);
        }
    }

    /// Fire the zone — enter upstroke.
    fn fire(&mut self, now: Instant) -> bool {
        self.membrane = self.peak_potential;
        self.phase = CardiacPhase::Upstroke;
        self.phase_start = now;
        if let Some(ref mut ca_clock) = self.calcium_clock {
            ca_clock.reset();
        }
        true
    }
}

/// Chemical cross-metabolism configuration.
///
/// Controls interactions between chemicals: cortisol protecting NE from
/// degradation, and mutual NE/ACh receptor antagonism (accrual).
#[derive(Clone, Debug)]
pub struct MetabolismConfig {
    /// How much cortisol extends NE half-life (0=none, 255=doubles it).
    /// Cortisol inhibits COMT, slowing NE degradation.
    pub cortisol_ne_protection: u8,
    /// How much NE suppresses ACh receptor effectiveness (0=none, 255=halves it).
    /// Sympathetic-parasympathetic antagonism at the receptor level.
    pub ne_ach_antagonism: u8,
    /// How much ACh suppresses NE receptor effectiveness (0=none, 255=halves it).
    pub ach_ne_antagonism: u8,
}

impl Default for MetabolismConfig {
    fn default() -> Self {
        Self {
            cortisol_ne_protection: 255, // full cortisol protection
            ne_ach_antagonism: 128,      // moderate mutual antagonism
            ach_ne_antagonism: 128,
        }
    }
}

/// Gap junction (connexin-43) coupling configuration between adjacent zones.
///
/// Gap junctions allow electrotonic current to flow between cardiac cells.
/// When a zone fires, it depolarizes its neighbors by `strength` mV.
/// Retrograde coupling (downstream→upstream) is half strength, modeling
/// the anisotropic nature of cardiac gap junction conductance.
#[derive(Clone, Debug)]
pub struct GapJunctionConfig {
    /// Forward coupling strength in mV. The depolarization applied to the
    /// downstream zone when the upstream zone fires. 0 = no coupling.
    pub strength: i16,
    /// Whether downstream zones apply retrograde coupling to upstream zones.
    /// Retrograde coupling is half the forward strength.
    pub retrograde: bool,
}

impl Default for GapJunctionConfig {
    fn default() -> Self {
        Self {
            strength: 3,
            retrograde: false,
        }
    }
}

/// Complete cardiac pipeline configuration.
///
/// Default values produce a resting heart rate around 70 BPM with
/// vagal tone baseline (ACh=30), subsidiary escape pacemakers in
/// AV node (~45 BPM) and Purkinje (~30 BPM), chemical cross-metabolism,
/// and gap junction coupling between zones.
#[derive(Clone, Debug)]
pub struct CardiacConfig {
    pub sa_node: ZoneConfig,
    pub av_node: ZoneConfig,
    pub conduction: ZoneConfig,
    pub myocardium: ZoneConfig,
    pub metabolism: MetabolismConfig,
    /// SA node → AV node gap junction coupling.
    pub gap_sa_av: GapJunctionConfig,
    /// AV node → conduction system gap junction coupling.
    pub gap_av_cond: GapJunctionConfig,
    /// Conduction system → myocardium gap junction coupling.
    pub gap_cond_myo: GapJunctionConfig,
}

impl Default for CardiacConfig {
    fn default() -> Self {
        Self {
            // SA node: master pacemaker
            // ~70 BPM resting: 860ms cycle
            // Diastolic gap = 30 mV, at 60/sec → diastolic = 500ms
            // Refractory = 350ms (blocks re-excitation for ~40% of cycle)
            sa_node: ZoneConfig {
                resting_potential: -70,
                threshold: -40,
                peak_potential: 30,
                base_leak_rate_per_sec: 60,   // 30mV in 500ms → 60/sec
                refractory_us: 350_000,        // 350ms
                conduction_delay_us: 0,
                ne_sensitivity: 128,
                ach_sensitivity: 128,
                calcium_clock: CalciumClockConfig {
                    enabled: true,
                    release_threshold: 180, // ~70% fill triggers release
                    base_refill_rate_per_sec: 400, // fills to threshold in ~450ms
                    ncx_depolarization: 5,  // ~17% of 30mV gap, conservative
                },
                hrv: HrvConfig {
                    enabled: true,
                    seed: 0xDEAD_BEEF_CAFE_BABE,
                    rsa_source: RsaSource::Internal,
                    rsa_amplitude: 3,    // ±3mV RSA
                    rsa_frequency: 14000, // ~0.25 Hz at 70 BPM
                    lf_amplitude: 2,     // ±2mV LF
                    lf_frequency: 5600,  // ~0.1 Hz at 70 BPM
                    intrinsic_jitter: 1, // ±1mV noise
                },
            },
            // AV node: delay gate + subsidiary pacemaker (~45 BPM escape)
            // Intrinsic rate: 25/sec on 30mV gap → 1200ms diastolic + 250ms
            // refractory = ~1450ms cycle ≈ 41 BPM. Normally suppressed by
            // SA node overdrive (SA fires every ~860ms, resetting AV's cycle).
            av_node: ZoneConfig {
                resting_potential: -70,
                threshold: -40,
                peak_potential: 30,
                base_leak_rate_per_sec: 25,    // subsidiary pacemaker (~45 BPM)
                refractory_us: 250_000,        // 250ms
                conduction_delay_us: 120_000,  // 120ms PR interval
                ne_sensitivity: 0,             // not autonomically modulated
                ach_sensitivity: 0,
                calcium_clock: CalciumClockConfig::default(),
                hrv: HrvConfig::default(),
            },
            // Conduction: bundle of His + Purkinje escape pacemaker (~30 BPM)
            // Intrinsic rate: 12/sec on 30mV gap → 2500ms diastolic + 200ms
            // refractory = ~2700ms cycle ≈ 22 BPM. Last-resort pacemaker.
            conduction: ZoneConfig {
                resting_potential: -70,
                threshold: -40,
                peak_potential: 30,
                base_leak_rate_per_sec: 12,    // escape pacemaker (~30 BPM)
                refractory_us: 200_000,        // 200ms
                conduction_delay_us: 10_000,   // 10ms fast conduction
                ne_sensitivity: 0,             // not autonomically modulated
                ach_sensitivity: 0,
                calcium_clock: CalciumClockConfig::default(),
                hrv: HrvConfig::default(),
            },
            // Myocardium: contractile mass — its firing IS the heartbeat
            myocardium: ZoneConfig {
                resting_potential: -70,
                threshold: -40,
                peak_potential: 30,
                base_leak_rate_per_sec: 0,
                refractory_us: 300_000,        // 300ms ventricular refractory
                conduction_delay_us: 0,
                ne_sensitivity: 0,
                ach_sensitivity: 0,
                calcium_clock: CalciumClockConfig::default(),
                hrv: HrvConfig::default(),
            },
            metabolism: MetabolismConfig::default(),
            // SA→AV: moderate coupling, no retrograde (SA is master pacemaker)
            gap_sa_av: GapJunctionConfig { strength: 3, retrograde: false },
            // AV→Conduction: moderate coupling with retrograde
            gap_av_cond: GapJunctionConfig { strength: 3, retrograde: true },
            // Conduction→Myocardium: stronger coupling (His-Purkinje → muscle)
            gap_cond_myo: GapJunctionConfig { strength: 5, retrograde: true },
        }
    }
}

//! Respiratory cycle physics — central pattern generator, gas exchange, configuration.
//!
//! The respiratory system models the pre-Botzinger complex as an autonomous
//! oscillator, analogous to the SA node in the cardiac system. A drive ramp
//! accumulates during the end-expiratory pause; when it reaches threshold,
//! inspiration begins. CO2 chemoreceptor input lowers the threshold (more
//! CO2 = breathe sooner). NE increases the ramp rate, ACh decreases it.
//!
//! Physics use wall-clock time — `Instant` timestamps and `Duration` for
//! all timing. Integer-only arithmetic. No floats.
//!
//! # Respiratory Cycle
//!
//! ```text
//! Inspiration ──(volume reaches tidal target)──→ EndInspiratory
//!      ↑                                              │
//!      │                                   (pause expires)
//!      │                                              ↓
//! EndExpiratory ←──(volume reaches FRC)── Expiration
//!      │
//!  (CPG drive reaches threshold)
//!      └──→ Inspiration
//! ```

use std::time::Instant;

/// Phase of the respiratory cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum RespiratoryPhase {
    /// Active diaphragm contraction. Lung volume increasing.
    /// Intrathoracic pressure drops, air flows in.
    /// Vagal tone decreases (RSA: HR increases during inspiration).
    Inspiration,
    /// Brief pause at peak volume. Hering-Breuer reflex onset.
    /// Lung stretch receptors maximally active.
    EndInspiratory,
    /// Passive elastic recoil. Lung volume decreasing.
    /// Intrathoracic pressure rises, air flows out.
    /// Vagal tone returns (RSA: HR decreases during expiration).
    Expiration,
    /// Pause at functional residual capacity (FRC). Quiet before next breath.
    /// CO2 continues to accumulate from metabolism.
    /// CPG drive ramp accumulates toward next inspiration threshold.
    EndExpiratory,
}

/// Lung volume and airflow state.
///
/// All volumes in "lung units" (0-255), where 0 = residual volume
/// and 255 = total lung capacity.
///
/// FRC (functional residual capacity) ~75: the resting volume between breaths.
/// Normal tidal volume ~30 units: breathing oscillates between ~75 and ~105.
pub struct LungState {
    /// Current lung volume (0-255 lung units).
    pub volume: u8,
    /// Current instantaneous airflow. Positive = inspiratory, negative = expiratory.
    pub flow: i16,
    /// Current respiratory phase.
    pub phase: RespiratoryPhase,
    /// When the current phase started.
    pub phase_start: Instant,
    /// Effective tidal volume for this breath cycle (autonomic-modulated).
    pub current_tidal_volume: u8,
    /// The volume the current inspiration began from — normally FRC, but LOWER after a voluntary exhale spent the
    /// reserve, so the recovery inspiration refills the deficit (a deeper, faster GASP).
    pub insp_start_volume: u8,
}

impl LungState {
    /// Create a new lung state at rest (end-expiratory, FRC volume).
    pub fn new(config: &RespiratoryConfig, now: Instant) -> Self {
        Self {
            volume: config.frc,
            flow: 0,
            phase: RespiratoryPhase::EndExpiratory,
            phase_start: now,
            current_tidal_volume: config.base_tidal_volume,
            insp_start_volume: config.frc,
        }
    }

    /// Compute phase progress as 0-255 (0 = phase just started, 255 = phase ending).
    pub fn phase_progress_256(&self, now: Instant, config: &RespiratoryConfig) -> u8 {
        let elapsed_us = now.duration_since(self.phase_start).as_micros() as u64;
        let phase_duration_us = match self.phase {
            RespiratoryPhase::Inspiration => config.inspiration_us,
            RespiratoryPhase::EndInspiratory => config.end_inspiratory_pause_us,
            RespiratoryPhase::Expiration => config.expiration_us,
            RespiratoryPhase::EndExpiratory => config.end_expiratory_pause_us,
        };
        if phase_duration_us == 0 {
            return 255;
        }
        let progress = (elapsed_us * 255) / phase_duration_us;
        progress.min(255) as u8
    }

    /// Update lung state: advance phase, compute volume and flow.
    ///
    /// Returns `true` if a full breath cycle completed (EndExpiratory → Inspiration transition).
    pub fn update(
        &mut self,
        now: Instant,
        generator: &mut RespiratoryGenerator,
        config: &RespiratoryConfig,
    ) -> bool {
        let elapsed_us = now.duration_since(self.phase_start).as_micros() as u64;

        match self.phase {
            RespiratoryPhase::Inspiration => {
                // Volume rises from where this breath STARTED (insp_start_volume) toward FRC + tidal. Normally the
                // start is FRC (a quiet breath); after a voluntary exhale spent the reserve it is much lower, so the
                // excursion is large and the flow fast — a GASP that refills the deficit.
                let target_volume = config.frc.saturating_add(self.current_tidal_volume);
                let excursion = target_volume.saturating_sub(self.insp_start_volume) as u64;
                if config.inspiration_us > 0 && excursion > 0 {
                    let progress = (elapsed_us * excursion) / config.inspiration_us;
                    self.volume = self.insp_start_volume.saturating_add(progress.min(excursion) as u8);
                    // Flow = volume change rate (units per second) — deeper refill → faster inspiratory flow (the gasp)
                    let flow = (excursion * 1_000_000) / config.inspiration_us;
                    self.flow = flow.min(i16::MAX as u64) as i16;
                }

                if elapsed_us >= config.inspiration_us {
                    self.volume = target_volume;
                    self.flow = 0;
                    self.phase = RespiratoryPhase::EndInspiratory;
                    self.phase_start = now;
                }
                false
            }
            RespiratoryPhase::EndInspiratory => {
                // Brief pause at peak volume
                self.flow = 0;
                if elapsed_us >= config.end_inspiratory_pause_us {
                    self.phase = RespiratoryPhase::Expiration;
                    self.phase_start = now;
                }
                false
            }
            RespiratoryPhase::Expiration => {
                // Volume falls from FRC + tidal back to FRC
                let peak_volume = config.frc.saturating_add(self.current_tidal_volume);
                let volume_range = self.current_tidal_volume as u64;
                if config.expiration_us > 0 && volume_range > 0 {
                    let progress = (elapsed_us * volume_range) / config.expiration_us;
                    let descent = progress.min(volume_range) as u8;
                    self.volume = peak_volume.saturating_sub(descent);
                    // Negative flow (expiratory)
                    let flow = (volume_range * 1_000_000) / config.expiration_us;
                    self.flow = -(flow.min(i16::MAX as u64) as i16);
                }

                if elapsed_us >= config.expiration_us {
                    self.volume = config.frc;
                    self.flow = 0;
                    self.phase = RespiratoryPhase::EndExpiratory;
                    self.phase_start = now;
                    generator.reset_drive();
                }
                false
            }
            RespiratoryPhase::EndExpiratory => {
                // CPG drive ramps. When threshold reached, start next inspiration.
                self.flow = 0;
                // Clamp DOWN to FRC only — preserve a volume left LOW by a voluntary exhale, so the next inspiration
                // starts from the deficit and gasps to refill it (rather than snapping back up to FRC).
                self.volume = self.volume.min(config.frc);

                // Compute drive from elapsed time (not accumulated)
                let drive = (generator.drive_rate_per_sec as u64 * elapsed_us) / 1_000_000;
                generator.drive = drive.min(255) as u8;

                // Also wait for minimum pause duration
                if generator.drive >= generator.effective_threshold
                    && elapsed_us >= config.end_expiratory_pause_us
                {
                    self.insp_start_volume = self.volume; // the breath begins from here (low = a gasp)
                    self.phase = RespiratoryPhase::Inspiration;
                    self.phase_start = now;
                    generator.reset_drive();
                    return true; // New breath cycle starting
                }
                false
            }
        }
    }
}

/// Central pattern generator for respiratory rhythm.
///
/// The pre-Botzinger complex fires rhythmically. Between breaths,
/// a drive ramp accumulates (like the SA node's diastolic depolarization).
/// When drive reaches threshold, inspiration begins.
pub struct RespiratoryGenerator {
    /// Drive accumulator (0-255). Ramps during EndExpiratory.
    pub drive: u8,
    /// Current effective drive rate per second (after autonomic modulation).
    pub drive_rate_per_sec: u32,
    /// Current effective threshold for inspiration (CO2/O2 modulated).
    pub effective_threshold: u8,
    /// Base drive rate (from config, for modulation reference).
    base_drive_rate_per_sec: u32,
}

impl RespiratoryGenerator {
    /// Create a new generator from config.
    pub fn new(config: &RespiratoryConfig) -> Self {
        Self {
            drive: 0,
            drive_rate_per_sec: config.base_drive_rate_per_sec,
            effective_threshold: config.base_drive_threshold,
            base_drive_rate_per_sec: config.base_drive_rate_per_sec,
        }
    }

    /// Reset drive accumulator at the start of a new breath cycle.
    pub fn reset_drive(&mut self) {
        self.drive = 0;
    }

    /// Apply autonomic and chemoreceptor modulation.
    ///
    /// NE increases drive rate (faster breathing). ACh decreases it.
    /// CO2 above baseline lowers threshold (breathe sooner).
    /// O2 below hypoxic threshold provides additional drive.
    pub fn apply_modulation(
        &mut self,
        ne: u8,
        ach: u8,
        co2: u16,
        o2: u16,
        config: &RespiratoryConfig,
    ) {
        let base = self.base_drive_rate_per_sec as u64;

        // NE: up to 2x drive rate at NE=255 with full sensitivity
        let ne_boost = (base * ne as u64 * config.ne_sensitivity as u64) / (255 * 255);

        // ACh: down to 0.5x drive rate at ACh=255 with full sensitivity
        let ach_brake = (base * ach as u64 * config.ach_sensitivity as u64) / (255 * 255 * 2);

        let modulated = (base as i64) + (ne_boost as i64) - (ach_brake as i64);
        let floor = (base / 4).max(1) as i64; // lungs cannot stop
        let ceiling = (base * 4) as i64;
        self.drive_rate_per_sec = modulated.clamp(floor, ceiling) as u32;

        // CO2 chemoreceptor: lower threshold when CO2 above baseline
        let co2_excess = co2.saturating_sub(config.co2_baseline);
        let co2_drive = (co2_excess as u32 * config.co2_sensitivity as u32) / 255;

        // O2 hypoxic drive: additional threshold reduction when O2 drops below threshold
        let o2_drive = if o2 < config.o2_hypoxic_threshold {
            let deficit = config.o2_hypoxic_threshold.saturating_sub(o2);
            (deficit as u32 * config.o2_sensitivity as u32) / 255
        } else {
            0
        };

        // Effective threshold = base - CO2 drive - O2 drive
        let threshold = config.base_drive_threshold as i32
            - co2_drive as i32
            - o2_drive as i32;
        self.effective_threshold = threshold
            .clamp(config.min_drive_threshold as i32, config.base_drive_threshold as i32)
            as u8;
    }
}

/// Gas exchange environment for the lungs.
///
/// CO2 and O2 are tracked as u16 concentrations at a fine scale (**25 units = 1 mmHg**), so the dynamics run on a
/// realistic timescale (a u8 0-255 range floored the integer rates too coarsely — a breath-hold reached breakpoint
/// in seconds). CO2 baseline ~1000 (40 mmHg); O2 baseline ~5000. CO2 accumulates continuously from metabolism; O2
/// depletes. Ventilation (Inspiration/Expiration phases) clears CO2 and replenishes O2, scaled by tidal volume.
pub struct GasPool {
    /// Arterial CO2 analog (25 units/mmHg; baseline ~1000). Primary respiratory drive signal.
    pub co2: u16,
    /// Arterial O2 analog (baseline ~5000). Secondary drive (hypoxic response).
    pub o2: u16,
    /// Last time gas metabolism was applied.
    last_metabolism: Instant,
}

impl GasPool {
    /// Create a new gas pool at baseline values.
    pub fn new(config: &RespiratoryConfig, now: Instant) -> Self {
        Self {
            co2: config.co2_baseline,
            o2: config.o2_baseline,
            last_metabolism: now,
        }
    }

    /// Metabolize gases and optionally ventilate.
    ///
    /// CO2 production and O2 consumption happen continuously (metabolism never stops).
    /// When `ventilating` is true (Inspiration/Expiration phases), CO2 clearance
    /// and O2 replenishment also occur, scaled by tidal volume.
    ///
    /// Uses 250ms batching like cardiac metabolism.
    pub fn metabolize(
        &mut self,
        now: Instant,
        ventilating: bool,
        tidal_volume: u8,
        config: &RespiratoryConfig,
    ) {
        let elapsed_us = now.duration_since(self.last_metabolism).as_micros() as u64;
        if elapsed_us < 250_000 {
            return;
        }
        self.last_metabolism = now;

        // CO2 production (continuous — metabolism never stops)
        let co2_produced =
            (config.co2_production_rate_per_sec as u64 * elapsed_us) / 1_000_000;
        self.co2 = self.co2.saturating_add(co2_produced.min(u16::MAX as u64) as u16);

        // O2 consumption (continuous)
        let o2_consumed =
            (config.o2_consumption_rate_per_sec as u64 * elapsed_us) / 1_000_000;
        self.o2 = self.o2.saturating_sub(o2_consumed.min(u16::MAX as u64) as u16);

        // Ventilation: clear CO2 and replenish O2 during active breathing
        if ventilating {
            let tidal_ratio_x256 = if config.base_tidal_volume > 0 {
                (tidal_volume as u64 * 256) / config.base_tidal_volume as u64
            } else {
                256
            };

            // CO2 clearance
            let base_clearance =
                (config.co2_clearance_rate_per_sec as u64 * elapsed_us) / 1_000_000;
            let effective_clearance = (base_clearance * tidal_ratio_x256) / 256;
            if self.co2 > config.co2_baseline {
                let distance = (self.co2 - config.co2_baseline) as u64;
                let cleared = effective_clearance.min(distance);
                self.co2 = self.co2.saturating_sub(cleared as u16);
            }

            // O2 replenishment
            let base_replenishment =
                (config.o2_replenishment_rate_per_sec as u64 * elapsed_us) / 1_000_000;
            let effective_replenishment = (base_replenishment * tidal_ratio_x256) / 256;
            if self.o2 < config.o2_baseline {
                let distance = (config.o2_baseline - self.o2) as u64;
                let replenished = effective_replenishment.min(distance);
                self.o2 = self.o2.saturating_add(replenished as u16);
            }
        }
    }
}

/// Compute tidal volume modulated by autonomic state.
///
/// NE deepens breaths. ACh makes them shallower. CO2 excess deepens breaths.
pub fn compute_tidal_volume(ne: u8, ach: u8, co2: u16, config: &RespiratoryConfig) -> u8 {
    let base = config.base_tidal_volume as i32;

    // NE deepens: up to +base at NE=255 with full sensitivity
    let ne_depth = (base * ne as i32 * config.ne_depth_sensitivity as i32) / (255 * 255);

    // ACh shallows: down to -base/2 at ACh=255 with full sensitivity
    let ach_shallow = (base * ach as i32 * config.ach_depth_sensitivity as i32) / (255 * 255 * 2);

    // CO2 deepens: up to +base/2 at max excess
    let co2_excess = co2.saturating_sub(config.co2_baseline) as i32;
    let co2_depth = (base * co2_excess * config.co2_sensitivity as i32) / (255 * 255);

    let modulated = base + ne_depth - ach_shallow + co2_depth;
    modulated.clamp(config.min_tidal_volume as i32, config.max_tidal_volume as i32) as u8
}

/// Complete respiratory system configuration.
///
/// Default values produce resting breathing at ~15 breaths/min with
/// CO2/O2 homeostasis, tidal volume ~30 lung units, and RSA coupling.
#[derive(Clone, Debug)]
pub struct RespiratoryConfig {
    // === Timing ===
    /// Base inspiration duration in microseconds.
    pub inspiration_us: u64,
    /// End-inspiratory pause duration (Hering-Breuer).
    pub end_inspiratory_pause_us: u64,
    /// Base expiration duration in microseconds.
    pub expiration_us: u64,
    /// End-expiratory pause base duration.
    pub end_expiratory_pause_us: u64,

    // === Volume ===
    /// Functional residual capacity — resting lung volume (0-255).
    pub frc: u8,
    /// Resting tidal volume (0-255 lung units).
    pub base_tidal_volume: u8,
    /// Maximum tidal volume (deep breath).
    pub max_tidal_volume: u8,
    /// Minimum tidal volume (shallow breathing).
    pub min_tidal_volume: u8,

    // === CPG Drive ===
    /// Base drive rate per second.
    pub base_drive_rate_per_sec: u32,
    /// Base drive threshold for initiating inspiration.
    pub base_drive_threshold: u8,
    /// Minimum threshold (under high CO2 / low O2).
    pub min_drive_threshold: u8,

    // === Autonomic Sensitivity ===
    /// NE sensitivity for respiratory rate modulation (0-255).
    pub ne_sensitivity: u8,
    /// ACh sensitivity for respiratory rate modulation (0-255).
    pub ach_sensitivity: u8,
    /// NE sensitivity for tidal volume modulation (0-255).
    pub ne_depth_sensitivity: u8,
    /// ACh sensitivity for tidal volume modulation (0-255).
    pub ach_depth_sensitivity: u8,

    // === Chemoreceptor === (CO2/O2 at 25 units/mmHg; see GasPool)
    /// CO2 baseline (homeostatic target), ~1000 = 40 mmHg.
    pub co2_baseline: u16,
    /// O2 baseline, ~5000.
    pub o2_baseline: u16,
    /// CO2 sensitivity for threshold reduction (0-255).
    pub co2_sensitivity: u8,
    /// O2 threshold below which hypoxic drive kicks in.
    pub o2_hypoxic_threshold: u16,
    /// O2 sensitivity for threshold reduction (0-255).
    pub o2_sensitivity: u8,

    // === Gas Exchange ===
    /// Resting CO2 production rate (units/sec).
    pub co2_production_rate_per_sec: u32,
    /// Resting O2 consumption rate (units/sec).
    pub o2_consumption_rate_per_sec: u32,
    /// Base CO2 clearance rate during ventilation (units/sec).
    pub co2_clearance_rate_per_sec: u32,
    /// Base O2 replenishment rate during ventilation (units/sec).
    pub o2_replenishment_rate_per_sec: u32,

    // === RSA Coupling ===
    /// Whether RSA coupling to the heart is active.
    pub rsa_enabled: bool,
    /// Resting vagal tone that RSA modulates around (ACh amount).
    pub rsa_vagal_baseline: u8,
    /// Depth of RSA modulation (0-255). How much vagal tone varies.
    pub rsa_depth: u8,
    /// Extra vagal augmentation during late expiration.
    pub rsa_expiratory_augmentation: u8,
}

impl Default for RespiratoryConfig {
    fn default() -> Self {
        Self {
            // ~4 second cycle = ~15 breaths/min
            inspiration_us: 1_600_000,
            end_inspiratory_pause_us: 100_000,
            expiration_us: 2_200_000,
            end_expiratory_pause_us: 100_000,

            frc: 75,
            base_tidal_volume: 30,
            max_tidal_volume: 120,
            min_tidal_volume: 10,

            base_drive_rate_per_sec: 128, // ~15 breaths/min via the CPG ramp (CO2 now rests near baseline at the
                                          // 25 units/mmHg scale, so it no longer boosts the rate; the ramp carries it)
            base_drive_threshold: 128,
            min_drive_threshold: 40,

            ne_sensitivity: 128,
            ach_sensitivity: 96,
            ne_depth_sensitivity: 100,
            ach_depth_sensitivity: 80,

            // 25 units = 1 mmHg. CO2 baseline 1000 (40 mmHg); O2 baseline 5000.
            co2_baseline: 1000,
            o2_baseline: 5000,
            co2_sensitivity: 60,        // tuned for the 25 units/mmHg scale (was 128 on the u8 scale)
            o2_hypoxic_threshold: 3000,
            o2_sensitivity: 20,         // tuned for the new scale (was 64)

            // At 25 units/mmHg, the 250 ms-batch integer floor (~4 units/s = 0.16 mmHg/s) gives a realistic
            // breath-hold: ~+10-15 mmHg (250-375 units) to the breakpoint over ~40-90 s (Grok-sourced).
            co2_production_rate_per_sec: 4,
            o2_consumption_rate_per_sec: 8,
            // Clearance compensates for production over the ~64% ventilating fraction of the cycle (≥ 4/0.64 ≈ 6.3);
            // use 12 for margin so resting CO2 settles near baseline.
            co2_clearance_rate_per_sec: 12,
            o2_replenishment_rate_per_sec: 20,

            rsa_enabled: true,
            rsa_vagal_baseline: 30,
            rsa_depth: 180,
            rsa_expiratory_augmentation: 15,
        }
    }
}

/// Compute RSA vagal modulation based on breath phase.
///
/// Returns the ACh amount (0-255) to send to the heart via the RSA signal.
/// Replaces the heart's ACh baseline with a breath-phase-modulated signal.
///
/// Inspiration: vagal tone decreases (HR rises).
/// Expiration: vagal tone increases (HR falls).
pub fn compute_vagal_modulation(
    phase: RespiratoryPhase,
    phase_progress_256: u8,
    config: &RespiratoryConfig,
) -> u8 {
    let baseline = config.rsa_vagal_baseline;
    let depth = config.rsa_depth;

    // Maximum vagal reduction during peak inspiration
    let max_reduction = (baseline as u16 * depth as u16) / 255;
    let min_vagal = baseline.saturating_sub(max_reduction as u8);
    let augmented = baseline.saturating_add(config.rsa_expiratory_augmentation);

    match phase {
        RespiratoryPhase::Inspiration => {
            // Vagal withdrawal: ACh decreases as inspiration progresses.
            // Linear ramp from baseline to min_vagal.
            let reduction =
                (max_reduction * phase_progress_256 as u16) / 255;
            baseline.saturating_sub(reduction as u8)
        }
        RespiratoryPhase::EndInspiratory => {
            // Minimal vagal tone (peak stretch).
            min_vagal
        }
        RespiratoryPhase::Expiration => {
            // Vagal return: linear ramp from min_vagal to augmented.
            let range = augmented.saturating_sub(min_vagal) as u16;
            let current =
                min_vagal as u16 + (range * phase_progress_256 as u16) / 255;
            current.min(255) as u8
        }
        RespiratoryPhase::EndExpiratory => {
            // Full vagal tone restored + augmentation.
            augmented
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn phase_transitions_are_sequential() {
        let config = RespiratoryConfig::default();
        let base = Instant::now();
        let mut lung = LungState::new(&config, base);
        let mut gen = RespiratoryGenerator::new(&config);

        // Start in EndExpiratory. Force CPG to fire immediately.
        gen.effective_threshold = 1;
        gen.drive_rate_per_sec = 10000; // Very fast ramp

        let mut phases = vec![lung.phase];

        // Advance through at least one full cycle
        for ms in 1..10000u64 {
            let now = base + Duration::from_millis(ms);
            lung.update(now, &mut gen, &config);
            if *phases.last().unwrap() != lung.phase {
                phases.push(lung.phase);
            }
            if phases.len() >= 6 {
                break;
            }
        }

        // Should see: EndExpiratory → Inspiration → EndInspiratory → Expiration → EndExpiratory → Inspiration
        assert!(phases.len() >= 5, "Got phases: {:?}", phases);
        assert_eq!(phases[0], RespiratoryPhase::EndExpiratory);
        assert_eq!(phases[1], RespiratoryPhase::Inspiration);
        assert_eq!(phases[2], RespiratoryPhase::EndInspiratory);
        assert_eq!(phases[3], RespiratoryPhase::Expiration);
        assert_eq!(phases[4], RespiratoryPhase::EndExpiratory);
    }

    #[test]
    fn volume_rises_during_inspiration() {
        let config = RespiratoryConfig::default();
        let base = Instant::now();
        let mut lung = LungState::new(&config, base);
        let mut gen = RespiratoryGenerator::new(&config);

        // Force into Inspiration
        lung.phase = RespiratoryPhase::Inspiration;
        lung.phase_start = base;
        lung.volume = config.frc;

        // Advance 800ms into a 1600ms inspiration
        let now = base + Duration::from_millis(800);
        lung.update(now, &mut gen, &config);

        assert!(lung.volume > config.frc, "Volume should rise during inspiration");
        assert!(lung.flow > 0, "Flow should be positive (inspiratory)");
    }

    #[test]
    fn volume_falls_during_expiration() {
        let config = RespiratoryConfig::default();
        let base = Instant::now();
        let mut lung = LungState::new(&config, base);
        let mut gen = RespiratoryGenerator::new(&config);

        // Force into Expiration at peak volume
        let peak = config.frc.saturating_add(config.base_tidal_volume);
        lung.phase = RespiratoryPhase::Expiration;
        lung.phase_start = base;
        lung.volume = peak;

        // Advance 1100ms into a 2200ms expiration
        let now = base + Duration::from_millis(1100);
        lung.update(now, &mut gen, &config);

        assert!(lung.volume < peak, "Volume should fall during expiration");
        assert!(lung.flow < 0, "Flow should be negative (expiratory)");
    }

    #[test]
    fn cpg_drive_accumulates_during_pause() {
        let config = RespiratoryConfig::default();
        let base = Instant::now();
        let mut lung = LungState::new(&config, base);
        let mut gen = RespiratoryGenerator::new(&config);

        // In EndExpiratory, drive should ramp up
        assert_eq!(lung.phase, RespiratoryPhase::EndExpiratory);
        assert_eq!(gen.drive, 0);

        // Advance 0.5 s (drive_rate=128 → drive=64, still below the 128 threshold so it accumulates without firing)
        let now = base + Duration::from_millis(500);
        lung.update(now, &mut gen, &config);

        assert!(gen.drive > 0, "CPG drive should accumulate during EndExpiratory");
        // At 128/sec for 0.5 second = 64
        assert_eq!(gen.drive, 64);
    }

    #[test]
    fn co2_lowers_cpg_threshold() {
        let config = RespiratoryConfig::default();
        let mut gen = RespiratoryGenerator::new(&config);

        // At baseline CO2: threshold should be base
        gen.apply_modulation(0, 0, config.co2_baseline, config.o2_baseline, &config);
        assert_eq!(gen.effective_threshold, config.base_drive_threshold);

        // At elevated CO2: threshold should drop
        gen.apply_modulation(0, 0, config.co2_baseline + 50, config.o2_baseline, &config);
        assert!(
            gen.effective_threshold < config.base_drive_threshold,
            "High CO2 should lower threshold: got {}",
            gen.effective_threshold
        );
    }

    #[test]
    fn ne_increases_drive_rate() {
        let config = RespiratoryConfig::default();
        let mut gen = RespiratoryGenerator::new(&config);

        gen.apply_modulation(0, 0, config.co2_baseline, config.o2_baseline, &config);
        let resting_rate = gen.drive_rate_per_sec;

        gen.apply_modulation(200, 0, config.co2_baseline, config.o2_baseline, &config);
        assert!(
            gen.drive_rate_per_sec > resting_rate,
            "NE should increase drive rate"
        );
    }

    #[test]
    fn ach_decreases_drive_rate() {
        let config = RespiratoryConfig::default();
        let mut gen = RespiratoryGenerator::new(&config);

        gen.apply_modulation(0, 0, config.co2_baseline, config.o2_baseline, &config);
        let resting_rate = gen.drive_rate_per_sec;

        gen.apply_modulation(0, 200, config.co2_baseline, config.o2_baseline, &config);
        assert!(
            gen.drive_rate_per_sec < resting_rate,
            "ACh should decrease drive rate"
        );
    }

    #[test]
    fn gas_exchange_clears_co2_during_ventilation() {
        let config = RespiratoryConfig::default();
        let base = Instant::now();
        let mut gas = GasPool::new(&config, base);

        // Elevate CO2 above baseline (1000 at 25 units/mmHg)
        gas.co2 = 1200;

        // Run metabolism + ventilation for 500ms
        let now = base + Duration::from_millis(500);
        gas.metabolize(now, true, config.base_tidal_volume, &config);

        // CO2 should have decreased (clearance > production at elevated levels)
        assert!(
            gas.co2 < 1200,
            "Ventilation should clear CO2: got {}",
            gas.co2
        );
    }

    #[test]
    fn co2_accumulates_during_pause() {
        let config = RespiratoryConfig::default();
        let base = Instant::now();
        let mut gas = GasPool::new(&config, base);

        // Only run metabolism (no ventilation) for 500ms
        let now = base + Duration::from_millis(500);
        gas.metabolize(now, false, 0, &config);

        // CO2 should have risen above baseline
        assert!(
            gas.co2 > config.co2_baseline,
            "CO2 should rise without ventilation: got {} vs baseline {}",
            gas.co2,
            config.co2_baseline
        );
    }
}

//! Respiratory pipeline — autonomous lung engine.
//!
//! The lungs run their own thread, like the heart. No tick(). No external clock.
//! The lungs have their own internal chemical environment with metabolism.
//! External sources inject NE/ACh; the lungs metabolize them back toward
//! resting baselines. If nothing injects, the lungs breathe at intrinsic rate.
//!
//! ```text
//! Brain injects ──→ [mpsc channel] ──→ Lung's chemical pool (internal)
//!                                           │ metabolism (decay)
//!                                           │ gas exchange (CO2/O2)
//!                                           ↓
//! CPG drive → Inspiration → EndInsp → Expiration → EndExp → BreathEvent channel
//!                                                      │
//!                                               RSA → AtomicU8 → Heart
//! ```

use std::sync::atomic::{AtomicBool, AtomicI16, AtomicU16, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::pipeline::decay_toward;
use crate::respiratory::{
    compute_tidal_volume, compute_vagal_modulation, GasPool, LungState,
    RespiratoryConfig, RespiratoryGenerator, RespiratoryPhase,
};
use crate::respiratory_vitals::{BreathEvent, RespiratoryRhythm, RespiratoryVitals};

/// Which chemical to inject into the lungs.
#[derive(Clone, Copy, Debug)]
pub enum RespiratoryChemical {
    /// Norepinephrine — sympathetic, increases rate and depth.
    NE,
    /// Acetylcholine — parasympathetic, decreases rate and depth.
    ACh,
}

/// A chemical injection event sent to the lungs.
#[derive(Clone, Copy, Debug)]
pub struct RespiratoryInjection {
    pub chemical: RespiratoryChemical,
    pub amount: u8,
}

/// The lung's internal autonomic chemical environment.
///
/// Separate from the gas pool. NE and ACh modulate respiratory rate
/// and depth, just as they modulate heart rate in the cardiac system.
struct RespiratoryChemicalPool {
    ne: u8,
    ach: u8,
    ne_baseline: u8,
    ach_baseline: u8,
    ne_halflife_us: u64,
    ach_halflife_us: u64,
    last_metabolism: Instant,
}

impl RespiratoryChemicalPool {
    fn new(now: Instant) -> Self {
        Self {
            ne: 0,
            ach: 20,  // less vagal tone than the heart
            ne_baseline: 0,
            ach_baseline: 20,
            ne_halflife_us: 2_500_000,  // 2.5 seconds (same as heart)
            ach_halflife_us: 500_000,   // 500ms tissue effect
            last_metabolism: now,
        }
    }

    fn inject(&mut self, chemical: RespiratoryChemical, amount: u8) {
        match chemical {
            RespiratoryChemical::NE => self.ne = self.ne.saturating_add(amount),
            RespiratoryChemical::ACh => self.ach = self.ach.saturating_add(amount),
        }
    }

    fn metabolize(&mut self, now: Instant) {
        let elapsed_us = now.duration_since(self.last_metabolism).as_micros() as u64;
        if elapsed_us < 250_000 {
            return;
        }
        self.last_metabolism = now;

        self.ne = decay_toward(self.ne, self.ne_baseline, elapsed_us, self.ne_halflife_us);
        self.ach = decay_toward(self.ach, self.ach_baseline, elapsed_us, self.ach_halflife_us);
    }
}

/// Handle to running lungs. Returned by `RespiratoryPipeline::start()`.
///
/// The lungs run autonomously in their own thread. Interact by:
/// - Injecting chemicals via `inject_ne()`, `inject_ach()`
/// - Receiving `BreathEvent`s from `breaths`
/// - Calling `stop()` to shut down
pub struct LungHandle {
    injector: Sender<RespiratoryInjection>,
    /// Receiver for breath events.
    pub breaths: Receiver<BreathEvent>,
    alive: Arc<AtomicBool>,
    thread: Option<JoinHandle<LungSnapshot>>,
    /// Current arterial O2 level (0-255). Updated each lung loop iteration.
    /// External consumers (autonomic bridge) can read this lock-free to gate
    /// tonic chemical production based on oxygen availability.
    o2_level: Arc<AtomicU16>,
    /// Current instantaneous airflow (signed: + = inspiratory, − = expiratory), in lung-units/iteration.
    /// Updated each loop iteration. A consumer (e.g. a voice) reads this to drive a breath/airflow sound.
    flow: Arc<AtomicI16>,
    /// Current lung volume (0-255 lung units; 0 = residual volume, FRC ~75). Updated each iteration — the live
    /// air budget. Reading it lets a consumer know how much air is left to spend.
    volume: Arc<AtomicU8>,
    /// Current arterial CO2 level (25 units/mmHg; baseline ~1000). Updated each iteration — the air-hunger signal
    /// (rises when ventilation can't keep up; drives the urge to breathe).
    co2_level: Arc<AtomicU16>,
    /// VOLUNTARY BREATH-HOLD (apnea). While true, the lung freezes — no airflow, no gas exchange — so CO2 climbs and
    /// O2 falls (the felt cost of holding the breath). An executive sets this to hold; releasing resumes the CPG,
    /// which breathes hard against the accumulated CO2. (Involuntary break-through at extreme CO2 is a future refinement.)
    hold: Arc<AtomicBool>,
    /// VOLUNTARY expiratory effort (0-255). While > 0, the brain actively drives air OUT — overriding the autonomic
    /// cycle: a strong expiratory airflow (∝ effort, audible as a forceful breath) and the lung volume falls toward
    /// residual (below FRC, into the expiratory reserve), spending the air budget. Released → the CPG resumes (it
    /// inhales to refill). This is the voluntary control that bursts, sustained exhales, and eventually speech ride on.
    exhale_effort: Arc<AtomicU8>,
}

impl LungHandle {
    /// Inject norepinephrine — increases respiratory rate and depth.
    pub fn inject_ne(&self, amount: u8) {
        let _ = self.injector.send(RespiratoryInjection {
            chemical: RespiratoryChemical::NE,
            amount,
        });
    }

    /// Inject acetylcholine — decreases respiratory rate and depth.
    pub fn inject_ach(&self, amount: u8) {
        let _ = self.injector.send(RespiratoryInjection {
            chemical: RespiratoryChemical::ACh,
            amount,
        });
    }

    /// Get the shared O2 level atomic. Reads are lock-free.
    ///
    /// The O2 level reflects arterial oxygen (0-255, baseline ~200).
    /// The bridge reads this to gate tonic chemical production:
    /// high O2 → normal production, low O2 → reduced production.
    pub fn o2_signal(&self) -> Arc<AtomicU16> {
        Arc::clone(&self.o2_level)
    }

    /// Get the shared instantaneous-airflow atomic (signed; + inspiratory, − expiratory). Reads are lock-free.
    /// The magnitude of expiratory flow is what a voice rides to make a breath/airflow sound — no flow, no sound.
    pub fn flow_signal(&self) -> Arc<AtomicI16> {
        Arc::clone(&self.flow)
    }

    /// Get the shared lung-volume atomic (0-255; 0 = residual volume, FRC ~75) — the live air budget. Reads are
    /// lock-free.
    pub fn volume_signal(&self) -> Arc<AtomicU8> {
        Arc::clone(&self.volume)
    }

    /// Get the shared CO2 atomic (25 units/mmHg) — the air-hunger signal (rises when ventilation lags). Lock-free.
    pub fn co2_signal(&self) -> Arc<AtomicU16> {
        Arc::clone(&self.co2_level)
    }

    /// HOLD / RELEASE the breath (voluntary apnea). While held, the lung freezes — no airflow, no gas exchange — so
    /// CO2 climbs and O2 falls (the felt cost). Release resumes normal breathing.
    pub fn set_hold(&self, on: bool) {
        self.hold.store(on, Ordering::Relaxed);
    }

    /// VOLUNTARY expiratory effort (0-255) — the brain pushing air out. 0 = let the autonomic cycle run; higher =
    /// stronger forced exhale (louder airflow, faster air spend). A brief high effort makes a burst; a steady
    /// moderate effort makes a sustained exhale; sustaining it runs the air low → a recovery gasp.
    pub fn set_exhale_effort(&self, effort: u8) {
        self.exhale_effort.store(effort, Ordering::Relaxed);
    }

    /// Check if the lungs are still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Stop the lungs and wait for the thread to finish.
    pub fn stop(mut self) -> LungSnapshot {
        self.alive.store(false, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            handle.join().expect("lung thread panicked")
        } else {
            LungSnapshot {
                breath_count: 0,
                last_breaths_per_minute: 0,
                last_tidal_volume: 0,
                last_rhythm: RespiratoryRhythm::Apnea,
                final_co2: 0,
                final_o2: 0,
                final_ne: 0,
                final_ach: 0,
            }
        }
    }
}

impl Drop for LungHandle {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

/// Snapshot of lung state at shutdown.
#[derive(Clone, Debug)]
pub struct LungSnapshot {
    pub breath_count: u64,
    pub last_breaths_per_minute: u16,
    pub last_tidal_volume: u8,
    pub last_rhythm: RespiratoryRhythm,
    pub final_co2: u16,
    pub final_o2: u16,
    pub final_ne: u8,
    pub final_ach: u8,
}

/// The respiratory pipeline launcher.
pub struct RespiratoryPipeline;

impl RespiratoryPipeline {
    /// Start the lungs with default configuration, no cardiac coupling.
    pub fn start() -> LungHandle {
        Self::start_with_config(RespiratoryConfig::default())
    }

    /// Start the lungs with custom configuration, no cardiac coupling.
    pub fn start_with_config(config: RespiratoryConfig) -> LungHandle {
        Self::launch(config, None)
    }

    /// Start the lungs coupled to an existing heart for RSA.
    ///
    /// The heart must have been started with `RsaSource::External` and
    /// expose an RSA signal via `heart.rsa_signal()`. The lung thread
    /// writes vagal modulation values to this atomic each iteration.
    pub fn start_coupled(
        config: RespiratoryConfig,
        rsa_signal: Arc<AtomicU8>,
    ) -> LungHandle {
        Self::launch(config, Some(rsa_signal))
    }

    fn launch(config: RespiratoryConfig, rsa_signal: Option<Arc<AtomicU8>>) -> LungHandle {
        let alive = Arc::new(AtomicBool::new(true));
        let o2_level = Arc::new(AtomicU16::new(config.o2_baseline));
        let flow = Arc::new(AtomicI16::new(0));
        let volume = Arc::new(AtomicU8::new(config.frc));
        let co2_level = Arc::new(AtomicU16::new(0));
        let hold = Arc::new(AtomicBool::new(false));
        let exhale_effort = Arc::new(AtomicU8::new(0));
        let (inject_tx, inject_rx) = std::sync::mpsc::channel();
        let (breath_tx, breath_rx) = std::sync::mpsc::channel();

        let alive_clone = Arc::clone(&alive);
        let o2_clone = Arc::clone(&o2_level);
        let flow_clone = Arc::clone(&flow);
        let volume_clone = Arc::clone(&volume);
        let co2_clone = Arc::clone(&co2_level);
        let hold_clone = Arc::clone(&hold);
        let exhale_clone = Arc::clone(&exhale_effort);

        let thread = thread::Builder::new()
            .name("respiratory".into())
            .spawn(move || {
                lung_loop(config, inject_rx, alive_clone, breath_tx, rsa_signal, o2_clone, flow_clone, volume_clone, co2_clone, hold_clone, exhale_clone)
            })
            .expect("failed to spawn respiratory thread");

        LungHandle {
            injector: inject_tx,
            breaths: breath_rx,
            alive,
            thread: Some(thread),
            o2_level,
            flow,
            volume,
            co2_level,
            hold,
            exhale_effort,
        }
    }
}

/// The lung's autonomous run loop.
fn lung_loop(
    config: RespiratoryConfig,
    inject_rx: Receiver<RespiratoryInjection>,
    alive: Arc<AtomicBool>,
    breath_tx: Sender<BreathEvent>,
    rsa_signal: Option<Arc<AtomicU8>>,
    o2_signal: Arc<AtomicU16>,
    flow_signal: Arc<AtomicI16>,
    volume_signal: Arc<AtomicU8>,
    co2_signal: Arc<AtomicU16>,
    hold: Arc<AtomicBool>,
    exhale_effort: Arc<AtomicU8>,
) -> LungSnapshot {
    let now = Instant::now();

    let mut lung_state = LungState::new(&config, now);
    let mut gas_pool = GasPool::new(&config, now);
    let mut chem_pool = RespiratoryChemicalPool::new(now);
    let mut generator = RespiratoryGenerator::new(&config);
    let mut vitals = RespiratoryVitals::new();

    // Track peak expiratory flow within each breath cycle
    let mut peak_expiratory_flow: u8 = 0;

    // Voluntary forced-expiration bookkeeping (per-iteration timer + fractional accumulator so slow integer spends
    // don't round to zero).
    let mut last_iter = now;
    let mut exhale_accum: u64 = 0;
    // Effort × elapsed_us accumulated per volume unit expelled (tuned so max effort empties ~FRC→RV in ~1.5 s).
    const EXHALE_SPEND_DIV: u64 = 5_000_000;

    let sleep_duration = Duration::from_millis(1);

    while alive.load(Ordering::Relaxed) {
        let now = Instant::now();

        // Drain all pending injections
        while let Ok(injection) = inject_rx.try_recv() {
            chem_pool.inject(injection.chemical, injection.amount);
        }

        // Metabolize autonomic chemicals
        chem_pool.metabolize(now);

        // Compute tidal volume modulation (before gas exchange needs it)
        lung_state.current_tidal_volume =
            compute_tidal_volume(chem_pool.ne, chem_pool.ach, gas_pool.co2, &config);

        // VOLUNTARY HOLD: while held, the breath is frozen — no gas exchange (CO2 climbs, O2 falls) and no airflow.
        let held = hold.load(Ordering::Relaxed);

        // Metabolize gases — CO2 production always, ventilation only during active phases AND when not holding
        let ventilating = !held
            && (lung_state.phase == RespiratoryPhase::Inspiration
                || lung_state.phase == RespiratoryPhase::Expiration);
        gas_pool.metabolize(now, ventilating, lung_state.current_tidal_volume, &config);

        // Publish O2 level for external consumers (autonomic bridge)
        o2_signal.store(gas_pool.o2, Ordering::Relaxed);

        // Apply autonomic + chemoreceptor modulation to generator
        generator.apply_modulation(
            chem_pool.ne,
            chem_pool.ach,
            gas_pool.co2,
            gas_pool.o2,
            &config,
        );

        // Read voluntary expiratory effort + per-iteration timing (the override below uses both).
        let iter_us = now.duration_since(last_iter).as_micros() as u64;
        last_iter = now;
        let effort = exhale_effort.load(Ordering::Relaxed);

        // Update respiratory phase — UNLESS the breath is held (frozen), or the brain is voluntarily driving air out.
        // In the voluntary case the CPG is SUPPRESSED so its phase-based volume doesn't overwrite the forced exhale
        // below FRC (the bug otherwise: update() resets volume to its phase value every iteration).
        let new_cycle = if held {
            lung_state.flow = 0;
            false
        } else if effort > 0 {
            false // CPG suppressed; the voluntary block below drives volume + flow
        } else {
            lung_state.update(now, &mut generator, &config)
        };

        // VOLUNTARY forced expiration — the brain driving air OUT: a strong expiratory airflow (∝ effort) and lung
        // volume falling toward residual (below FRC, into the expiratory reserve), spending the air budget. Released
        // → the CPG resumes (it inhales to refill — a recovery gasp if the air ran low).
        if effort > 0 && !held && lung_state.volume > 0 {
            exhale_accum += effort as u64 * iter_us;
            let spend = (exhale_accum / EXHALE_SPEND_DIV) as u64;
            if spend > 0 {
                exhale_accum -= spend * EXHALE_SPEND_DIV;
                lung_state.volume = lung_state.volume.saturating_sub(spend.min(255) as u8);
            }
            lung_state.flow = -(effort as i16); // strong, audible expiratory push (the louder the harder you blow)
        } else {
            exhale_accum = 0;
        }

        // Publish the live airflow / volume / CO2 for external consumers (a voice rides the airflow; volume is the
        // air budget; CO2 is the air-hunger signal).
        flow_signal.store(lung_state.flow, Ordering::Relaxed);
        volume_signal.store(lung_state.volume, Ordering::Relaxed);
        co2_signal.store(gas_pool.co2, Ordering::Relaxed);

        // Track peak expiratory flow
        if lung_state.flow < 0 {
            let abs_flow = (-lung_state.flow).min(255) as u8;
            if abs_flow > peak_expiratory_flow {
                peak_expiratory_flow = abs_flow;
            }
        }

        // RSA coupling: send vagal modulation to heart
        if config.rsa_enabled {
            if let Some(ref signal) = rsa_signal {
                let phase_progress =
                    lung_state.phase_progress_256(now, &config);
                let vagal = compute_vagal_modulation(
                    lung_state.phase,
                    phase_progress,
                    &config,
                );
                signal.store(vagal, Ordering::Relaxed);
            }
        }

        // Emit breath event if a new cycle started
        if new_cycle {
            let breath = vitals.record_breath(
                now,
                lung_state.current_tidal_volume,
                peak_expiratory_flow,
            );
            let _ = breath_tx.send(breath);
            peak_expiratory_flow = 0;
        }

        thread::sleep(sleep_duration);
    }

    let now = Instant::now();
    LungSnapshot {
        breath_count: vitals.breath_count,
        last_breaths_per_minute: vitals.breaths_per_minute(),
        last_tidal_volume: vitals.last_tidal_volume,
        last_rhythm: vitals.classify(now),
        final_co2: gas_pool.co2,
        final_o2: gas_pool.o2,
        final_ne: chem_pool.ne,
        final_ach: chem_pool.ach,
    }
}

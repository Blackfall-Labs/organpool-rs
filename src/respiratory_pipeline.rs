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

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
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
    pub final_co2: u8,
    pub final_o2: u8,
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
        let (inject_tx, inject_rx) = std::sync::mpsc::channel();
        let (breath_tx, breath_rx) = std::sync::mpsc::channel();

        let alive_clone = Arc::clone(&alive);

        let thread = thread::Builder::new()
            .name("respiratory".into())
            .spawn(move || lung_loop(config, inject_rx, alive_clone, breath_tx, rsa_signal))
            .expect("failed to spawn respiratory thread");

        LungHandle {
            injector: inject_tx,
            breaths: breath_rx,
            alive,
            thread: Some(thread),
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
) -> LungSnapshot {
    let now = Instant::now();

    let mut lung_state = LungState::new(&config, now);
    let mut gas_pool = GasPool::new(&config, now);
    let mut chem_pool = RespiratoryChemicalPool::new(now);
    let mut generator = RespiratoryGenerator::new(&config);
    let mut vitals = RespiratoryVitals::new();

    // Track peak expiratory flow within each breath cycle
    let mut peak_expiratory_flow: u8 = 0;

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

        // Metabolize gases — CO2 production always, ventilation only during active phases
        let ventilating = lung_state.phase == RespiratoryPhase::Inspiration
            || lung_state.phase == RespiratoryPhase::Expiration;
        gas_pool.metabolize(now, ventilating, lung_state.current_tidal_volume, &config);

        // Apply autonomic + chemoreceptor modulation to generator
        generator.apply_modulation(
            chem_pool.ne,
            chem_pool.ach,
            gas_pool.co2,
            gas_pool.o2,
            &config,
        );

        // Update respiratory phase
        let new_cycle = lung_state.update(now, &mut generator, &config);

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

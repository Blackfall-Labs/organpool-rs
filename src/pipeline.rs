//! Cardiac conduction pipeline — autonomous heart engine.
//!
//! The heart runs its own thread. No tick(). No external clock.
//! The heart has its own internal chemical environment with metabolism.
//! External sources (brain, bloodstream) inject chemicals into the heart's
//! environment. Enzymes metabolize them back toward resting baselines.
//! If nothing injects, the heart beats at its intrinsic rate.
//!
//! ```text
//! Brain injects ──→ [mpsc channel] ──→ Heart's chemical pool (internal)
//!                                           │ metabolism (decay)
//!                                           ↓
//! SA Node → AV Node → Conduction → Myocardium → BeatEvent channel
//! ```
//!
//! A denervated heart (no injections) beats at intrinsic rate with resting
//! vagal tone — exactly like a transplanted human heart.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::cardiac::{CardiacConfig, CardiacZone, RsaSource};
use crate::vitals::{BeatEvent, CardiacRhythm, CardiacVitals};

/// Which chemical to inject.
#[derive(Clone, Copy, Debug)]
pub enum Chemical {
    /// Norepinephrine — sympathetic, accelerates.
    NE,
    /// Acetylcholine — parasympathetic, decelerates.
    ACh,
    /// Cortisol — hormonal, amplifies NE sensitivity.
    Cortisol,
}

/// A chemical injection event sent to the heart.
#[derive(Clone, Copy, Debug)]
pub struct ChemicalInjection {
    pub chemical: Chemical,
    /// Amount to add to the local pool (saturating at 255).
    pub amount: u8,
}

/// The heart's internal chemical environment.
///
/// This is entirely owned by the heart thread. Nothing external reads or
/// writes it directly. External sources inject via channel; the heart
/// metabolizes internally.
///
/// Resting baselines reflect real physiology:
/// - NE baseline ~0: sympathetic tone is low at rest (heart is vagally dominated)
/// - ACh baseline ~30: vagal tone is real — the resting heart is actively
///   slowed by parasympathetic activity. Denervated hearts beat faster
///   than innervated ones (~100 BPM vs ~70 BPM) because vagal brake is removed.
/// - Cortisol baseline ~10: low circadian cortisol at rest
pub struct ChemicalPool {
    /// Current local NE concentration (0-255).
    pub ne: u8,
    /// Current local ACh concentration (0-255).
    pub ach: u8,
    /// Current local cortisol concentration (0-255).
    pub cortisol: u8,

    /// Resting NE baseline — metabolism decays toward this.
    pub ne_baseline: u8,
    /// Resting ACh baseline (vagal tone).
    pub ach_baseline: u8,
    /// Resting cortisol baseline.
    pub cortisol_baseline: u8,

    /// NE half-life in microseconds. ~2-3 seconds for synaptic NE (MAO/COMT).
    pub ne_halflife_us: u64,
    /// ACh half-life in microseconds. ~1ms for synaptic ACh (acetylcholinesterase
    /// is extremely fast), but we model the tissue-level effect which lingers ~500ms.
    pub ach_halflife_us: u64,
    /// Cortisol half-life in microseconds. ~60-90 minutes in blood, but we
    /// model the local tissue effect — ~10 seconds for cardiac sensitivity.
    pub cortisol_halflife_us: u64,

    /// Last time metabolism was applied.
    last_metabolism: Instant,
}

impl ChemicalPool {
    fn new(now: Instant) -> Self {
        Self {
            ne: 0,
            ach: 30,       // vagal tone at rest
            cortisol: 10,  // low circadian cortisol

            ne_baseline: 0,
            ach_baseline: 30,
            cortisol_baseline: 10,

            ne_halflife_us: 2_500_000,     // 2.5 seconds
            ach_halflife_us: 500_000,      // 500ms tissue effect
            cortisol_halflife_us: 10_000_000, // 10 seconds local effect

            last_metabolism: now,
        }
    }

    /// Inject a chemical into the pool. Saturates at 255.
    fn inject(&mut self, chemical: Chemical, amount: u8) {
        match chemical {
            Chemical::NE => self.ne = self.ne.saturating_add(amount),
            Chemical::ACh => self.ach = self.ach.saturating_add(amount),
            Chemical::Cortisol => self.cortisol = self.cortisol.saturating_add(amount),
        }
    }

    /// Metabolize: decay each chemical toward its baseline.
    ///
    /// Uses exponential decay: each half-life, the distance from baseline halves.
    /// Only applies when enough time has accumulated (10ms minimum) so integer
    /// arithmetic produces meaningful fractions.
    ///
    /// Cross-metabolism: cortisol inhibits COMT, extending NE half-life.
    /// `cortisol_ne_protection` controls the strength (0=none, 255=doubles halflife).
    fn metabolize(&mut self, now: Instant, cortisol_ne_protection: u8) {
        let elapsed_us = now.duration_since(self.last_metabolism).as_micros() as u64;
        // Metabolize in ≥250ms chunks so fractional decay produces meaningful
        // integer results for u8 distances. At 250ms with a 2.5s half-life,
        // decay is ~6.9% of distance — well above truncation threshold for
        // any distance ≥ 2. Shorter intervals (e.g. 10ms) truncate to 0 for
        // most u8 distances, making different half-lives indistinguishable.
        if elapsed_us < 250_000 {
            return;
        }
        self.last_metabolism = now;

        // Cortisol extends NE half-life by inhibiting COMT.
        // At protection=255 and cortisol=255: half-life doubled.
        // effective = base × (256 + cortisol × protection / 255) / 256
        let ne_halflife = {
            let extension = (self.cortisol as u64 * cortisol_ne_protection as u64) / 255;
            (self.ne_halflife_us * (256 + extension)) / 256
        };

        self.ne = decay_toward(self.ne, self.ne_baseline, elapsed_us, ne_halflife);
        self.ach = decay_toward(self.ach, self.ach_baseline, elapsed_us, self.ach_halflife_us);
        self.cortisol = decay_toward(
            self.cortisol,
            self.cortisol_baseline,
            elapsed_us,
            self.cortisol_halflife_us,
        );
    }
}

/// Exponential decay toward a baseline, integer-only.
///
/// `current` decays toward `baseline` with the given half-life.
/// After `halflife_us` microseconds, the distance from baseline halves.
///
/// For full half-life periods: shift right (halve distance).
/// For fractional remainder: linear approximation using ln(2) ≈ 693/1000.
///
/// Metabolism is batched in ≥250ms chunks so that the fractional decay
/// computation produces meaningful integer results for u8 distances.
/// Shorter intervals would truncate to 0 for most distances, creating
/// a half-life-independent decay rate from the `max(1)` floor.
pub(crate) fn decay_toward(current: u8, baseline: u8, elapsed_us: u64, halflife_us: u64) -> u8 {
    if current == baseline || halflife_us == 0 {
        return current;
    }

    let distance = if current > baseline {
        (current - baseline) as u64
    } else {
        (baseline - current) as u64
    };

    // Full half-lives: each halves the distance
    let full_halflives = elapsed_us / halflife_us;
    let remainder_us = elapsed_us % halflife_us;

    let mut remaining_distance = if full_halflives >= 8 {
        0 // < 0.4% remains — snap to baseline
    } else {
        distance >> full_halflives
    };

    // Fractional remainder: linear approximation of exponential decay
    // decay_amount = distance * (1 - e^(-dt*ln2/halflife))
    //             ≈ distance * dt * ln2 / halflife  (for small dt)
    //             = distance * dt * 693 / (halflife * 1000)
    if remaining_distance > 0 && remainder_us > 0 {
        let decay_amount = (remaining_distance * remainder_us * 693) / (halflife_us * 1000);
        if decay_amount > 0 {
            remaining_distance = remaining_distance.saturating_sub(decay_amount);
        }
    }

    if current > baseline {
        baseline.saturating_add(remaining_distance.min(255) as u8)
    } else {
        baseline.saturating_sub(remaining_distance.min(255) as u8)
    }
}

/// Handle to a running heart. Returned by `CardiacPipeline::start()`.
///
/// The heart runs autonomously in its own thread. You interact with it by:
/// - Injecting chemicals via `inject_ne()`, `inject_ach()`, `inject_cortisol()`
/// - Receiving `BeatEvent`s from `beats`
/// - Calling `stop()` to shut it down
///
/// If you stop injecting, chemicals decay to baseline and the heart returns
/// to its intrinsic rhythm. A denervated heart (no injections ever) beats
/// at ~100 BPM because vagal tone provides the only resting brake and the
/// ACh baseline (30) applies a modest deceleration.
pub struct HeartHandle {
    /// Channel for injecting chemicals into the heart's environment.
    injector: Sender<ChemicalInjection>,
    /// Receiver for beat events. Each myocardium contraction sends one.
    pub beats: Receiver<BeatEvent>,
    /// Signal to stop the heart thread.
    alive: Arc<AtomicBool>,
    /// Join handle for the heart thread.
    thread: Option<JoinHandle<HeartSnapshot>>,
    /// Shared RSA signal for coupling to a respiratory system.
    /// The lung thread writes vagal modulation values here;
    /// the heart thread reads them to modulate ACh.
    /// None if the heart was started without external RSA.
    rsa_signal: Option<Arc<AtomicU8>>,
}

impl HeartHandle {
    /// Inject norepinephrine into the heart's chemical environment.
    /// The amount is added to the current local concentration (saturating).
    /// NE will decay back toward baseline (~0) with a half-life of ~2.5s.
    pub fn inject_ne(&self, amount: u8) {
        let _ = self.injector.send(ChemicalInjection {
            chemical: Chemical::NE,
            amount,
        });
    }

    /// Inject acetylcholine into the heart's chemical environment.
    /// ACh decays quickly (half-life ~500ms) back toward vagal tone baseline (~30).
    pub fn inject_ach(&self, amount: u8) {
        let _ = self.injector.send(ChemicalInjection {
            chemical: Chemical::ACh,
            amount,
        });
    }

    /// Inject cortisol into the heart's chemical environment.
    /// Cortisol decays slowly (half-life ~10s) back toward baseline (~10).
    pub fn inject_cortisol(&self, amount: u8) {
        let _ = self.injector.send(ChemicalInjection {
            chemical: Chemical::Cortisol,
            amount,
        });
    }

    /// Get the RSA signal atomic for coupling to a respiratory system.
    ///
    /// Returns `Some(Arc<AtomicU8>)` if the heart was started with
    /// `RsaSource::External`. The respiratory pipeline writes vagal
    /// modulation values here, and the heart reads them to set ACh level.
    ///
    /// Returns `None` if the heart uses internal RSA (sine oscillator).
    pub fn rsa_signal(&self) -> Option<Arc<AtomicU8>> {
        self.rsa_signal.clone()
    }

    /// Check if the heart is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Stop the heart and wait for the thread to finish.
    /// Returns a snapshot of the heart's final state.
    pub fn stop(mut self) -> HeartSnapshot {
        self.alive.store(false, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            handle.join().expect("heart thread panicked")
        } else {
            HeartSnapshot {
                beat_count: 0,
                last_bpm: 0,
                last_rhythm: CardiacRhythm::Asystole,
                last_ibi_us: 0,
                final_ne: 0,
                final_ach: 0,
                final_cortisol: 0,
            }
        }
    }
}

impl Drop for HeartHandle {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
    }
}

/// Snapshot of heart state at shutdown.
#[derive(Clone, Debug)]
pub struct HeartSnapshot {
    pub beat_count: u64,
    pub last_bpm: u16,
    pub last_rhythm: CardiacRhythm,
    pub last_ibi_us: u64,
    /// Final NE concentration in the heart's pool.
    pub final_ne: u8,
    /// Final ACh concentration.
    pub final_ach: u8,
    /// Final cortisol concentration.
    pub final_cortisol: u8,
}

/// The cardiac conduction pipeline.
///
/// This is a launcher — call `start()` to spawn the heart thread.
/// The heart then runs autonomously until `stop()` is called on the handle.
pub struct CardiacPipeline;

impl CardiacPipeline {
    /// Start the heart with default configuration.
    pub fn start() -> HeartHandle {
        Self::start_with_config(CardiacConfig::default())
    }

    /// Start the heart with custom configuration.
    pub fn start_with_config(config: CardiacConfig) -> HeartHandle {
        let alive = Arc::new(AtomicBool::new(true));
        let (inject_tx, inject_rx) = std::sync::mpsc::channel();
        let (beat_tx, beat_rx) = std::sync::mpsc::channel();

        // Create RSA signal atomic if external RSA is requested.
        // Initialize to the ACh baseline (30) so that before any lung
        // writes, the heart operates at normal vagal tone.
        let rsa_signal = if config.sa_node.hrv.rsa_source == RsaSource::External {
            Some(Arc::new(AtomicU8::new(30)))
        } else {
            None
        };

        let alive_clone = Arc::clone(&alive);
        let rsa_clone = rsa_signal.clone();

        let thread = thread::Builder::new()
            .name("cardiac".into())
            .spawn(move || heart_loop(config, inject_rx, alive_clone, beat_tx, rsa_clone))
            .expect("failed to spawn cardiac thread");

        HeartHandle {
            injector: inject_tx,
            beats: beat_rx,
            alive,
            thread: Some(thread),
            rsa_signal,
        }
    }
}

/// The heart's autonomous run loop.
///
/// This is the core physics engine. It owns its chemical environment,
/// metabolizes chemicals internally, and runs zone physics continuously.
/// External injections arrive via channel and are drained each iteration.
fn heart_loop(
    config: CardiacConfig,
    inject_rx: Receiver<ChemicalInjection>,
    alive: Arc<AtomicBool>,
    beat_tx: Sender<BeatEvent>,
    rsa_signal: Option<Arc<AtomicU8>>,
) -> HeartSnapshot {
    let now = Instant::now();

    let mut sa_node = CardiacZone::new(&config.sa_node, now);
    let mut av_node = CardiacZone::new(&config.av_node, now);
    let mut conduction = CardiacZone::new(&config.conduction, now);
    let mut myocardium = CardiacZone::new(&config.myocardium, now);
    let mut vitals = CardiacVitals::new();
    let mut pool = ChemicalPool::new(now);

    let sleep_duration = Duration::from_micros(100);

    while alive.load(Ordering::Relaxed) {
        let now = Instant::now();

        // Drain all pending injections from the channel
        while let Ok(injection) = inject_rx.try_recv() {
            pool.inject(injection.chemical, injection.amount);
        }

        // External RSA: read vagal modulation from the lung thread and
        // set it as the ACh baseline. Brain ACh injections are additive
        // on top — they spike ACh above the respiratory-driven floor,
        // then metabolism decays the spike back. The respiratory
        // oscillation persists underneath.
        if let Some(ref signal) = rsa_signal {
            pool.ach_baseline = signal.load(Ordering::Relaxed);
        }

        // Metabolize: decay chemicals toward baselines (with cross-metabolism)
        pool.metabolize(now, config.metabolism.cortisol_ne_protection);

        // Apply current chemical environment to SA node (with receptor antagonism)
        sa_node.apply_modulation(pool.ne, pool.ach, pool.cortisol, &config.metabolism);

        // Advance zone physics with gap junction coupling
        let sa_fired = sa_node.update(now);
        if sa_fired {
            av_node.trigger(now);
            // SA→AV gap junction: depolarize AV node
            av_node.electrotonic_depolarize(config.gap_sa_av.strength);
        }

        let av_fired = av_node.update(now);
        if av_fired {
            conduction.trigger(now);
            // AV→Conduction gap junction: depolarize conduction system
            conduction.electrotonic_depolarize(config.gap_av_cond.strength);
            // Retrograde: AV→SA (half strength)
            if config.gap_av_cond.retrograde {
                sa_node.electrotonic_depolarize(config.gap_av_cond.strength / 2);
            }
        }

        let cond_fired = conduction.update(now);
        if cond_fired {
            myocardium.trigger(now);
            // Conduction→Myocardium gap junction: depolarize myocardium
            myocardium.electrotonic_depolarize(config.gap_cond_myo.strength);
            // Retrograde: Conduction→AV (half strength)
            if config.gap_cond_myo.retrograde {
                av_node.electrotonic_depolarize(config.gap_cond_myo.strength / 2);
            }
        }

        let myo_fired = myocardium.update(now);
        if myo_fired {
            let mut beat = vitals.record_beat(now);
            // POSITIVE INOTROPY — contractile force rises with sympathetic drive: NE above resting tone makes the
            // myocardium contract HARDER (a forceful, pounding beat). At rest it sits moderate, leaving headroom so
            // the pounding heart of stress is genuinely felt as a rising PRESSURE — not the flat constant it was.
            const RESTING_STROKE: u16 = 110;
            // half-gain so even high sympathetic drive leaves headroom below the 255 ceiling — a pounding heart still
            // reads clearly above rest without pinning to max (so a fresh surge is always felt as a further rise).
            let inotropy = pool.ne.saturating_sub(pool.ne_baseline) as u16 / 2;
            beat.stroke_force = (RESTING_STROKE + inotropy).min(255) as u8;
            let _ = beat_tx.send(beat);
            // Retrograde: Myocardium→Conduction (half strength)
            if config.gap_cond_myo.retrograde {
                conduction.electrotonic_depolarize(config.gap_cond_myo.strength / 2);
            }
        }

        thread::sleep(sleep_duration);
    }

    let now = Instant::now();
    HeartSnapshot {
        beat_count: vitals.beat_count,
        last_bpm: vitals.bpm(),
        last_rhythm: vitals.classify(now),
        last_ibi_us: vitals.ibi_mean_us(),
        final_ne: pool.ne,
        final_ach: pool.ach,
        final_cortisol: pool.cortisol,
    }
}

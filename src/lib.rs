//! Organ physics substrate — cardiac and respiratory simulation.
//!
//! `organpool` simulates autonomous organ systems with integer-only physics,
//! wall-clock timing, and chemical modulation by the autonomic nervous system.
//!
//! # Organs
//!
//! **Heart** — 4-zone cardiac conduction (SA → AV → Conduction → Myocardium)
//! with ion channel cycling, escape rhythms, gap junctions, calcium clock,
//! and HRV generation. Modulated by NE (sympathetic), ACh (parasympathetic),
//! and cortisol.
//!
//! **Lungs** — Respiratory cycle (Inspiration → EndInspiratory → Expiration →
//! EndExpiratory) with central pattern generator, CO2/O2 gas exchange, and
//! autonomic modulation. Couples to the heart via respiratory sinus arrhythmia.
//!
//! Each organ runs its own thread with its own chemical environment.
//! External sources inject chemicals; the organ metabolizes them internally.
//! If nothing injects, chemicals decay to resting baselines and the organ
//! operates at its intrinsic rate. Denervated organs still function.
//!
//! # Architecture
//!
//! ```text
//! Brain injects ──→ Heart (thread) ──→ BeatEvent channel
//!                        ↑ RSA (AtomicU8)
//! Brain injects ──→ Lungs (thread) ──→ BreathEvent channel
//! ```
//!
//! # Usage
//!
//! ```no_run
//! use organpool::CardiacPipeline;
//! use std::time::Duration;
//!
//! // Start the heart — it begins beating immediately at intrinsic rate
//! let heart = CardiacPipeline::start();
//!
//! // Inject NE (sympathetic burst — decays over ~2.5s half-life)
//! heart.inject_ne(200);
//!
//! // Observe beats
//! while let Ok(beat) = heart.beats.recv_timeout(Duration::from_secs(2)) {
//!     println!("Beat #{} IBI={}μs", beat.beat_number, beat.ibi_us);
//! }
//!
//! // Stop the heart
//! let snapshot = heart.stop();
//! println!("Final BPM: {}", snapshot.last_bpm);
//! ```

pub mod cardiac;
pub mod pipeline;
pub mod respiratory;
pub mod respiratory_pipeline;
pub mod respiratory_vitals;
pub mod vitals;

#[cfg(feature = "fibertract")]
pub mod bridge;

// Re-export cardiac types
pub use cardiac::{CalciumClockConfig, CardiacConfig, CardiacPhase, CardiacZone, GapJunctionConfig, HrvConfig, MetabolismConfig, RsaSource, ZoneConfig};
pub use pipeline::{CardiacPipeline, Chemical, ChemicalInjection, HeartHandle, HeartSnapshot};
pub use vitals::{BeatEvent, CardiacRhythm, CardiacVitals};

// Re-export respiratory types
pub use respiratory::{RespiratoryConfig, RespiratoryPhase};
pub use respiratory_pipeline::{LungHandle, LungSnapshot, RespiratoryChemical, RespiratoryInjection, RespiratoryPipeline};
pub use respiratory_vitals::{BreathEvent, RespiratoryRhythm, RespiratoryVitals};

#[cfg(feature = "fibertract")]
pub use bridge::AutonomicBridge;

//! Organ physics substrate — cardiac pacemaker simulation.
//!
//! `organpool` simulates the cardiac conduction system: an autonomous
//! oscillator driven by ion channel cycling (HCN → Ca²⁺ → K⁺ → refractory),
//! modulated by the autonomic nervous system (NE accelerates, ACh decelerates).
//!
//! The heart runs its own thread with its own chemical environment.
//! External sources inject chemicals; the heart metabolizes them internally.
//! If nothing injects, chemicals decay to resting baselines and the heart
//! beats at its intrinsic rate. A denervated heart still beats — like a
//! transplanted human heart.
//!
//! # Architecture
//!
//! ```text
//! Brain injects ──→ [mpsc channel] ──→ Heart's chemical pool (internal)
//!                                           │ metabolism (enzymatic decay)
//!                                           ↓
//! SA Node → AV Node → Conduction → Myocardium → BeatEvent channel
//! ```
//!
//! - **SA node**: Master pacemaker with intrinsic HCN leak current
//! - **AV node**: Delay gate preventing atrial/ventricular overlap
//! - **Conduction**: Bundle of His + Purkinje fibers
//! - **Myocardium**: Contractile mass — its firing IS the heartbeat
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
pub mod vitals;

#[cfg(feature = "fibertract")]
pub mod bridge;

// Re-export primary types at crate root
pub use cardiac::{CalciumClockConfig, CardiacConfig, CardiacPhase, CardiacZone, GapJunctionConfig, HrvConfig, MetabolismConfig, ZoneConfig};
pub use pipeline::{CardiacPipeline, Chemical, ChemicalInjection, HeartHandle, HeartSnapshot};
pub use vitals::{BeatEvent, CardiacRhythm, CardiacVitals};

#[cfg(feature = "fibertract")]
pub use bridge::AutonomicBridge;

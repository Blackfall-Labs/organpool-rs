//! Autonomic bridge — couples fibertract nerve bundles to the cardiac pipeline.
//!
//! The bridge reads chemical concentrations from autonomic efferent tracts
//! (NE from sympathetic chain, ACh from vagus nerve) and injects them into
//! the heart's chemical pool. After reading beat events from the heart's
//! channel, it writes interoceptive signals back to the cardiac afferent tract.
//!
//! The heart owns its own chemical environment. The bridge is the nerve
//! terminal — it releases neurotransmitter into the heart's tissue, and
//! the heart metabolizes it on its own.
//!
//! ```text
//! Brain (NE/ACh) → Autonomic Tracts → Bridge → Heart pool (injection)
//! Heart (beats)  → Bridge → Interoceptive Tract → Brain (awareness)
//! ```

use fibertract::{FiberBundle, FiberTractKind};

use crate::pipeline::HeartHandle;
use crate::vitals::BeatEvent;

/// Autonomic bridge coupling nerve bundles to a cardiac pipeline.
///
/// Reads NE and ACh concentrations from fibertract autonomic efferent tracts
/// and injects them into the heart's chemical pool. Reads beat events from
/// the heart's channel and writes interoceptive feedback to the vagus nerve.
pub struct AutonomicBridge {
    /// Sympathetic cardiac bundle — carries NE efferent.
    pub sympathetic: FiberBundle,
    /// Vagus nerve bundle — carries ACh efferent + cardiac afferent.
    pub vagus: FiberBundle,
}

impl AutonomicBridge {
    /// Create a bridge from pre-built bundles.
    pub fn new(sympathetic: FiberBundle, vagus: FiberBundle) -> Self {
        Self {
            sympathetic,
            vagus,
        }
    }

    /// Create a bridge using the default autonomic profiles.
    pub fn from_profiles() -> Self {
        use fibertract::LimbProfile;
        Self {
            sympathetic: LimbProfile::sympathetic_cardiac().build(),
            vagus: LimbProfile::vagus_nerve().build(),
        }
    }

    /// Read tract concentrations and inject them into the heart's pool.
    ///
    /// Call this on the brain's update cycle. The bridge reads what the
    /// autonomic tracts are carrying and injects that amount into the heart.
    /// The heart will metabolize it on its own — you need to keep calling
    /// this to maintain elevated levels (just like real nerve firing).
    pub fn push_chemicals(&self, heart: &HeartHandle) {
        let ne = self
            .sympathetic
            .tract(FiberTractKind::AutonomicEfferent)
            .map(|t| t.autonomic_concentration(0))
            .unwrap_or(0);

        let ach = self
            .vagus
            .tract(FiberTractKind::AutonomicEfferent)
            .map(|t| t.autonomic_concentration(0))
            .unwrap_or(0);

        if ne > 0 {
            heart.inject_ne(ne);
        }
        if ach > 0 {
            heart.inject_ach(ach);
        }
    }

    /// Inject cortisol into the heart. Cortisol is hormonal (bloodstream),
    /// not delivered through autonomic fibers.
    pub fn push_cortisol(&self, heart: &HeartHandle, cortisol: u8) {
        if cortisol > 0 {
            heart.inject_cortisol(cortisol);
        }
    }

    /// Write interoceptive feedback from a beat event to the vagal afferent tract.
    pub fn write_interoception(&mut self, beat: &BeatEvent) {
        if let Some(afferent) = self.vagus.tract_mut(FiberTractKind::Interoceptive) {
            let ibi_signal = beat.ibi_us.min(i32::MAX as u64) as i32;
            let pressure_signal = beat.stroke_force as i32 * 25; // stroke_force is now 0..1000 (was 0..255 × 100)
            let input = vec![ibi_signal, pressure_signal];
            afferent.transmit_sensory(&input, beat.beat_number);
        }
    }

    /// Write silence (no beat) to the vagal afferent tract.
    pub fn write_silence(&mut self) {
        if let Some(afferent) = self.vagus.tract_mut(FiberTractKind::Interoceptive) {
            let input = vec![0i32, 0i32];
            afferent.transmit_sensory(&input, 0);
        }
    }

    /// Drive sympathetic efferent: brain writes NE concentration to sympathetic tract.
    pub fn set_sympathetic_drive(&mut self, ne_concentration: u8) {
        if let Some(tract) = self.sympathetic.tract_mut(FiberTractKind::AutonomicEfferent) {
            tract.transmit_autonomic(&[ne_concentration]);
        }
    }

    /// Drive vagal efferent: brain writes ACh concentration to vagus tract.
    pub fn set_vagal_drive(&mut self, ach_concentration: u8) {
        if let Some(tract) = self.vagus.tract_mut(FiberTractKind::AutonomicEfferent) {
            tract.transmit_autonomic(&[ach_concentration]);
        }
    }

    /// Read the interoceptive signal from the cardiac afferent tract.
    pub fn read_interoception(&self) -> (i32, i32) {
        self.vagus
            .tract(FiberTractKind::Interoceptive)
            .map(|t| {
                let beat = t.sensory_signals.first().copied().unwrap_or(0);
                let pressure = t.sensory_signals.get(1).copied().unwrap_or(0);
                (beat, pressure)
            })
            .unwrap_or((0, 0))
    }

    /// Current NE concentration on the sympathetic tract.
    pub fn delivered_ne(&self) -> u8 {
        self.sympathetic
            .tract(FiberTractKind::AutonomicEfferent)
            .map(|t| t.autonomic_concentration(0))
            .unwrap_or(0)
    }

    /// Current ACh concentration on the vagus tract.
    pub fn delivered_ach(&self) -> u8 {
        self.vagus
            .tract(FiberTractKind::AutonomicEfferent)
            .map(|t| t.autonomic_concentration(0))
            .unwrap_or(0)
    }
}

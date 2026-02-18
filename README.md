# organpool

Organ physics substrate for the Blackfall Labs neuromorphic ecosystem. Simulates autonomous cardiac and respiratory systems as coupled oscillators driven by ion channel cycling and central pattern generation, modulated by the autonomic nervous system.

Each organ runs its own thread with its own chemical environment. External sources inject chemicals; each organ metabolizes them internally. If nothing injects, chemicals decay to resting baselines and the organs operate at their intrinsic rates. Denervated organs still function — like a transplanted human heart.

**Integer-only.** No floats anywhere. All physics are computed with integer arithmetic on `u8`/`i16`/`u32`/`u64` substrates. Wall-clock time via `std::time::Instant` — no tick counters, no simulation clock.

## Architecture

```
Brain (NE/ACh) ──→ Heart (thread) ──→ BeatEvent channel
                        ↑ RSA coupling (AtomicU8)
Brain (NE/ACh) ──→ Lungs (thread) ──→ BreathEvent channel
```

The heart and lungs are coupled via respiratory sinus arrhythmia (RSA). The lung thread computes vagal modulation from breath phase and writes it to a shared `Arc<AtomicU8>`. The heart reads this value each iteration and uses it to modulate ACh level — inspiration withdraws vagal tone (HR rises), expiration restores it (HR falls).

---

## Heart

### Conduction Zones

| Zone | Role | Intrinsic Rate | Refractory |
|------|------|----------------|------------|
| **SA Node** | Master pacemaker. HCN leak + calcium clock + HRV. | ~70 BPM | 350ms |
| **AV Node** | Delay gate (120ms PR interval) + subsidiary pacemaker. | ~41 BPM | 250ms |
| **Conduction** | Bundle of His + Purkinje fibers. Last-resort escape. | ~22 BPM | 200ms |
| **Myocardium** | Contractile mass. Its firing IS the heartbeat. | None (driven only) | 300ms |

Each zone simulates the ion channel cycle:

```
Diastolic ──(leak reaches threshold)──→ Upstroke (Ca²⁺ depolarization)
    ↑                                       │
    │                                       ↓
    └───(refractory expires)─── Refractory (K⁺ repolarization)
```

**Overdrive suppression:** The SA node fires fastest, resetting slower subsidiary pacemakers before they reach threshold. AV and Purkinje escape rhythms only manifest when the SA node fails or slows below their intrinsic rates.

### Cardiac Chemical Environment

The heart owns an internal `ChemicalPool` with three chemicals:

| Chemical | Role | Baseline | Half-life | Effect |
|----------|------|----------|-----------|--------|
| **NE** (norepinephrine) | Sympathetic — accelerates | 0 | 2.5s | Up to ~3x leak rate |
| **ACh** (acetylcholine) | Parasympathetic — decelerates | 30 (vagal tone) | 500ms | Down to ~0.5x leak rate |
| **Cortisol** | Hormonal — amplifies NE sensitivity | 10 | 10s | Extends NE half-life, boosts receptor gain |

Resting baselines reflect real physiology. The ACh baseline of 30 represents tonic vagal activity — the resting heart is actively slowed by parasympathetic input. Denervated hearts beat ~100 BPM (no vagal brake) vs ~70 BPM innervated.

**Enzymatic metabolism** decays chemicals toward baseline using integer exponential decay, batched in 250ms chunks to preserve half-life fidelity across the u8 distance range.

### Chemical Cross-Metabolism

Chemicals don't decay independently:

- **Cortisol protects NE:** Cortisol inhibits COMT, extending NE's effective half-life. At max cortisol + max protection, NE half-life doubles. Sustained stress keeps NE elevated longer.
- **NE/ACh receptor antagonism:** Sympathetic and parasympathetic branches partially cancel at the receptor level. High NE suppresses ACh effectiveness (up to 50%), and vice versa. Co-injection of both produces less effect than either alone.

### Gap Junction Coupling

Connexin-43 channels allow electrotonic current between adjacent zones. When a zone fires, it depolarizes its neighbors:

| Junction | Strength | Retrograde |
|----------|----------|------------|
| SA → AV | 3 mV | No (SA is master) |
| AV → Conduction | 3 mV | Yes (half strength) |
| Conduction → Myocardium | 5 mV | Yes (half strength) |

Gap junction depolarization is **clamped at threshold - 1** — coupling accelerates the approach to threshold but cannot directly fire a zone. This prevents spurious extra beats from coupling alone.

### Calcium Clock

The SA node has two coupled oscillators:

1. **Membrane clock** — HCN leak current (the primary ramp)
2. **Calcium clock** — sarcoplasmic reticulum Ca²⁺ cycling

The SR accumulates calcium during diastole. When load reaches the release threshold (~70% fill), Ca²⁺ is released through RyR2 channels, driving the NCX (Na⁺/Ca²⁺ exchanger) to produce an inward depolarizing current. Unlike gap junctions, calcium clock depolarization **can** push past threshold and trigger firing — it's an integral part of the pacemaker, not an external influence.

The calcium clock refill rate is co-modulated with the membrane clock: sympathetic activation accelerates both proportionally.

### Heart Rate Variability

Beat-to-beat variability from three physiological sources, implemented as threshold jitter applied at each Refractory → Diastolic transition:

| Source | Frequency | Amplitude | Mechanism |
|--------|-----------|-----------|-----------|
| **RSA** (respiratory sinus arrhythmia) | ~0.25 Hz | ±3 mV | Vagal modulation synchronized with breathing |
| **LF oscillation** (baroreflex) | ~0.1 Hz | ±2 mV | Autonomic baroreflex feedback loop |
| **Intrinsic jitter** | Stochastic | ±1 mV | Ion channel noise (xorshift64 PRNG) |

RSA supports two modes via `RsaSource`:
- **Internal** (default) — Fixed-frequency sine oscillator producing synthetic RSA without real respiratory input. Backward compatible with v0.1.0.
- **External** — Real respiratory signal from a coupled lung thread via `Arc<AtomicU8>`. The lung continuously modulates the heart's ACh baseline through breath-phase-synchronized vagal gating.

### Cardiac Vital Signs

Real-time diagnostics from wall-clock beat timing:

- **BPM** — beats per minute from IBI mean
- **IBI** — inter-beat interval statistics (mean, CV, RMSSD)
- **Rhythm classification** — Normal Sinus, Sinus Tachycardia, Sinus Bradycardia, Arrhythmia, Fibrillation, Asystole

RMSSD (root mean square of successive differences) is a standard short-term HRV metric reflecting vagal tone.

---

## Lungs

### Respiratory Cycle

The lungs model the pre-Botzinger complex as an autonomous oscillator with four phases:

```
Inspiration ──(volume reaches tidal target)──→ EndInspiratory
     ↑                                              │
     │                                   (pause expires)
     │                                              ↓
EndExpiratory ←──(volume reaches FRC)── Expiration
     │
 (CPG drive reaches threshold)
     └──→ Inspiration
```

| Phase | Duration | Description |
|-------|----------|-------------|
| **Inspiration** | ~1.6s | Active diaphragm contraction. Volume rises. Vagal withdrawal (HR rises). |
| **EndInspiratory** | ~100ms | Brief pause at peak volume. Hering-Breuer reflex onset. |
| **Expiration** | ~2.2s | Passive elastic recoil. Volume falls. Vagal tone returns (HR falls). |
| **EndExpiratory** | ~2s (CPG-driven) | Pause at FRC. CO2 accumulates. CPG drive ramps toward threshold. |

Default configuration produces ~10-15 breaths/min at rest.

### Central Pattern Generator

The CPG (pre-Botzinger analog) works like the SA node's HCN leak: a drive ramp accumulates during the end-expiratory pause. When drive reaches threshold, inspiration begins.

- **NE** increases drive ramp rate (breathe faster)
- **ACh** decreases drive ramp rate (breathe slower)
- **CO2 excess** lowers threshold (breathe sooner) — chemoreceptor feedback
- **O2 deficit** provides additional drive below the hypoxic threshold

### Gas Exchange

The lungs maintain a `GasPool` with CO2/O2 homeostasis:

| Gas | Baseline | Production/Consumption | Clearance/Replenishment |
|-----|----------|----------------------|------------------------|
| **CO2** | 40 | 20/sec (continuous — metabolism never stops) | 35/sec (during ventilation only) |
| **O2** | 200 | 15/sec consumption (continuous) | 28/sec (during ventilation only) |

CO2 clearance and O2 replenishment only occur during active breathing phases (Inspiration + Expiration). During pauses, CO2 accumulates from metabolism, creating a negative feedback loop: CO2 rises → CPG threshold drops → breathe sooner → CO2 clears.

Clearance scales with tidal volume — deeper breaths clear more CO2 per cycle.

### Respiratory Chemical Environment

The lungs have their own autonomic chemical pool (separate from gas exchange):

| Chemical | Baseline | Half-life | Effect |
|----------|----------|-----------|--------|
| **NE** | 0 | 2.5s | Increases rate and depth |
| **ACh** | 20 (less vagal tone than heart) | 500ms | Decreases rate and depth |

### Tidal Volume Modulation

Tidal volume (breath depth) is dynamically modulated:

- **NE** deepens breaths (sympathetic activation = prepare for exertion)
- **ACh** makes breaths shallower (parasympathetic = relaxation)
- **CO2 excess** deepens breaths (compensatory response)

### Respiratory Vital Signs

- **Breaths per minute** from cycle time ring buffer
- **Tidal volume** and **peak expiratory flow** per breath
- **Rhythm classification** — Eupnea (normal), Tachypnea (fast), Bradypnea (slow), Apnea (absent)

---

## RSA Coupling

When heart and lungs are coupled, the lung thread writes vagal modulation values to a shared `Arc<AtomicU8>`. The heart thread reads this each iteration and sets it as the ACh baseline:

| Breath Phase | Vagal ACh | Heart Rate Effect |
|--------------|-----------|-------------------|
| Inspiration | Decreasing → minimum | HR rises |
| EndInspiratory | Minimum | HR at peak |
| Expiration | Increasing → augmented | HR falls |
| EndExpiratory | Baseline + slight augmentation | HR at lowest |

Brain ACh injections are **additive** on top of the respiratory-driven floor. Metabolism decays the spike back to the respiratory baseline. The respiratory oscillation persists underneath.

---

## Autonomic Bridge

With the `fibertract` feature enabled, `AutonomicBridge` couples fibertract nerve bundles to the heart:

```
Brain (NE/ACh) → Autonomic Tracts → Bridge → Heart pool (injection)
Heart (beats)  → Bridge → Interoceptive Tract → Brain (awareness)
```

The bridge reads chemical concentrations from sympathetic (NE) and vagal (ACh) efferent tracts, injects them into the heart, and writes interoceptive feedback (IBI timing, stroke force) back to the vagal afferent tract.

---

## Usage

### Standalone Heart

```rust
use organpool::CardiacPipeline;
use std::time::Duration;

// Start the heart — begins beating immediately at intrinsic rate
let heart = CardiacPipeline::start();

// Inject NE (sympathetic burst — decays over ~2.5s half-life)
heart.inject_ne(200);

// Observe beats
while let Ok(beat) = heart.beats.recv_timeout(Duration::from_secs(2)) {
    println!("Beat #{} IBI={}us", beat.beat_number, beat.ibi_us);
}

// Stop the heart
let snapshot = heart.stop();
println!("Final BPM: {}", snapshot.last_bpm);
```

### Standalone Lungs

```rust
use organpool::RespiratoryPipeline;
use std::time::Duration;

// Start the lungs — begins breathing immediately at intrinsic rate
let lungs = RespiratoryPipeline::start();

// Inject NE (sympathetic — breathe faster and deeper)
lungs.inject_ne(150);

// Observe breaths
while let Ok(breath) = lungs.breaths.recv_timeout(Duration::from_secs(8)) {
    println!("Breath #{} cycle={}us tidal={}",
        breath.breath_number, breath.cycle_us, breath.tidal_volume);
}

// Stop the lungs
let snapshot = lungs.stop();
println!("Final BPM: {}, CO2: {}", snapshot.last_breaths_per_minute, snapshot.final_co2);
```

### Coupled Cardiopulmonary System

```rust
use organpool::{CardiacConfig, CardiacPipeline, RespiratoryConfig, RespiratoryPipeline, RsaSource};
use std::time::Duration;

// Start heart with external RSA (real respiratory coupling)
let mut config = CardiacConfig::default();
config.sa_node.hrv.rsa_source = RsaSource::External;
let heart = CardiacPipeline::start_with_config(config);

// Get the RSA signal and couple lungs to it
let rsa_signal = heart.rsa_signal().expect("External RSA provides signal");
let lungs = RespiratoryPipeline::start_coupled(RespiratoryConfig::default(), rsa_signal);

// Both organs now run coupled — breathing modulates heart rate
// Stress both organs simultaneously
heart.inject_ne(200);
lungs.inject_ne(200);

// Observe
while let Ok(beat) = heart.beats.recv_timeout(Duration::from_secs(2)) {
    println!("Beat #{} IBI={}us", beat.beat_number, beat.ibi_us);
}

lungs.stop();
heart.stop();
```

### Custom Configuration

```rust
use organpool::{CardiacPipeline, CardiacConfig};

let mut config = CardiacConfig::default();

// Disable escape rhythms (AV and Purkinje leak = 0)
config.av_node.base_leak_rate_per_sec = 0;
config.conduction.base_leak_rate_per_sec = 0;

// Stronger gap junction coupling
config.gap_cond_myo.strength = 8;

// Disable HRV for deterministic testing
config.sa_node.hrv.enabled = false;

// Disable calcium clock
config.sa_node.calcium_clock.enabled = false;

// No chemical cross-metabolism
config.metabolism.cortisol_ne_protection = 0;
config.metabolism.ne_ach_antagonism = 0;
config.metabolism.ach_ne_antagonism = 0;

let heart = CardiacPipeline::start_with_config(config);
```

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `serde` | Yes | Serialization for rhythm/phase enums and config types |
| `fibertract` | No | Autonomic bridge to fibertract nerve bundles |

## Design Principles

**Wall-clock time.** Organs track real `Instant` timestamps. Dynamics are computed from elapsed `Duration`. There is no simulation tick — each organ runs in real time on its own thread.

**Owned chemical environment.** Each organ thread owns all state. External code cannot read or write the chemical pool directly — only inject via channel. Each organ metabolizes internally. This mirrors real physiology: organs have their own tissue chemistry.

**Integer-only physics.** All arithmetic is integer. Exponential decay uses bit-shifting for full half-lives and a linear ln(2) approximation for fractional remainders. Metabolism is batched in 250ms chunks so u8 distance arithmetic doesn't lose half-life fidelity to truncation.

**Denervated operation.** If nothing ever injects chemicals, organs still function. The heart beats at intrinsic rate with vagal tone. The lungs breathe at intrinsic rate with CO2/O2 homeostasis. No organ needs a brain to operate.

**Lock-free coupling.** Inter-organ communication uses `Arc<AtomicU8>` — zero allocation, zero contention, always-latest value. No mutexes, no channels, no blocking between organ threads.

## Crate Structure

```
src/
├── lib.rs                  Root module, re-exports
├── cardiac.rs              Zone physics, ion channel cycle, calcium clock,
│                           HRV generation, gap junctions, RsaSource, configuration
├── pipeline.rs             Heart thread, chemical pool, enzymatic metabolism,
│                           conduction pipeline, RSA coupling, decay_toward()
├── vitals.rs               Beat events, IBI statistics, BPM, rhythm classification
├── respiratory.rs          Respiratory cycle physics, CPG, gas exchange,
│                           vagal modulation, tidal volume, configuration
├── respiratory_pipeline.rs Lung thread, chemical pool, RSA signal output,
│                           LungHandle, RespiratoryPipeline
├── respiratory_vitals.rs   Breath events, respiratory rate, rhythm classification
└── bridge.rs               Autonomic bridge (fibertract feature)
```

## Test Suite

96 tests across four test files:

- **cardiac_tests.rs** (37) — Zone physics, chemical modulation, escape rhythms, cross-metabolism, gap junctions, calcium clock, HRV generation
- **adversarial_tests.rs** (21) — Extreme configurations, saturation, edge cases, recovery from stress, rhythm classification under load
- **respiratory_tests.rs** (14) — Standalone lung operation, NE/ACh rate modulation, tidal volume, gas exchange homeostasis, recovery, extreme conditions
- **cardiopulmonary_tests.rs** (6) — Coupled heart-lung operation, RSA variability, cross-organ NE, graceful degradation, stress recovery

Plus 17 unit tests in source and 1 doc-test.

```bash
cargo test -p organpool
```

# organpool

Organ physics substrate for the Blackfall Labs neuromorphic ecosystem. Simulates the cardiac conduction system as an autonomous oscillator driven by ion channel cycling, modulated by the autonomic nervous system.

The heart runs its own thread with its own chemical environment. External sources inject chemicals; the heart metabolizes them internally. If nothing injects, chemicals decay to resting baselines and the heart beats at its intrinsic rate. A denervated heart still beats — like a transplanted human heart.

**Integer-only.** No floats anywhere. All physics are computed with integer arithmetic on `u8`/`i16`/`u32`/`u64` substrates. Wall-clock time via `std::time::Instant` — no tick counters, no simulation clock.

## Architecture

```
Brain (NE/ACh) ──→ [mpsc channel] ──→ Heart's chemical pool (internal)
                                            │ enzymatic metabolism (exponential decay)
                                            ↓
SA Node ──→ AV Node ──→ Conduction ──→ Myocardium ──→ BeatEvent channel
    ↑           ↑            ↑              ↑
    └───────────┴────────────┴──────────────┘
              gap junction coupling (bidirectional)
```

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

### Chemical Environment

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

Maximum combined jitter: ±6 mV on a 30 mV threshold gap = ±20% IBI worst case. Typical variation is well under 10%.

### Vital Signs

Real-time diagnostics from wall-clock beat timing:

- **BPM** — beats per minute from IBI mean
- **IBI** — inter-beat interval statistics (mean, CV, RMSSD)
- **Rhythm classification** — Normal Sinus, Sinus Tachycardia, Sinus Bradycardia, Arrhythmia, Fibrillation, Asystole

RMSSD (root mean square of successive differences) is a standard short-term HRV metric reflecting vagal tone.

### Autonomic Bridge

With the `fibertract` feature enabled, `AutonomicBridge` couples fibertract nerve bundles to the heart:

```
Brain (NE/ACh) → Autonomic Tracts → Bridge → Heart pool (injection)
Heart (beats)  → Bridge → Interoceptive Tract → Brain (awareness)
```

The bridge reads chemical concentrations from sympathetic (NE) and vagal (ACh) efferent tracts, injects them into the heart, and writes interoceptive feedback (IBI timing, stroke force) back to the vagal afferent tract.

## Usage

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

### Autonomic Bridge (with fibertract)

```rust
use organpool::{CardiacPipeline, AutonomicBridge};

let heart = CardiacPipeline::start();
let mut bridge = AutonomicBridge::from_profiles();

// Brain drives sympathetic output
bridge.set_sympathetic_drive(150);

// Push chemicals from tracts into heart
bridge.push_chemicals(&heart);

// Read interoceptive feedback
let (ibi_signal, pressure_signal) = bridge.read_interoception();
```

## Features

| Feature | Default | Description |
|---------|---------|-------------|
| `serde` | Yes | Serialization for rhythm classification |
| `fibertract` | Yes | Autonomic bridge to fibertract nerve bundles |

## Design Principles

**Wall-clock time.** Zones track real `Instant` timestamps. Membrane dynamics are computed from elapsed `Duration`. There is no simulation tick — the heart runs in real time on its own thread.

**Owned chemical environment.** The heart thread owns all state. External code cannot read or write the chemical pool directly — only inject via channel. The heart metabolizes internally. This mirrors real cardiac physiology: the heart has its own tissue chemistry.

**Integer-only physics.** All arithmetic is integer. Exponential decay uses bit-shifting for full half-lives and a linear ln(2) approximation for fractional remainders. Metabolism is batched in 250ms chunks so u8 distance arithmetic doesn't lose half-life fidelity to truncation.

**Denervated operation.** If nothing ever injects chemicals, the heart still beats. Resting vagal tone (ACh baseline = 30) provides the only brake. The heart is autonomous — it doesn't need a brain to function.

## Crate Structure

```
src/
├── lib.rs          Root module, re-exports
├── cardiac.rs      Zone physics, ion channel cycle, calcium clock,
│                   HRV generation, gap junctions, configuration
├── pipeline.rs     Heart thread, chemical pool, enzymatic metabolism,
│                   conduction pipeline, decay_toward()
├── vitals.rs       Beat events, IBI statistics, BPM, rhythm classification
└── bridge.rs       Autonomic bridge (fibertract feature)
```

## Test Suite

67 tests across two test files:

- **cardiac_tests.rs** — Zone physics, chemical modulation, escape rhythms, cross-metabolism, gap junctions, calcium clock, HRV generation
- **adversarial_tests.rs** — Extreme configurations, saturation, edge cases, recovery from stress, rhythm classification under load

```bash
cargo test -p organpool
```

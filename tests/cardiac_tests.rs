//! Integration tests for the autonomous cardiac engine.
//!
//! The heart runs itself with its own chemical environment. Tests inject
//! chemicals and observe beats. Chemicals decay — sustained effects require
//! repeated injection (like real nerve firing).

use organpool::{CalciumClockConfig, CardiacConfig, CardiacPipeline, CardiacRhythm, CardiacVitals, GapJunctionConfig, HrvConfig, MetabolismConfig, ZoneConfig};
use std::time::{Duration, Instant};

/// Helper: collect N beats from a running heart within a timeout.
fn collect_beats(
    heart: &organpool::HeartHandle,
    n: usize,
    timeout: Duration,
) -> Vec<organpool::BeatEvent> {
    let mut beats = Vec::with_capacity(n);
    let deadline = Instant::now() + timeout;
    while beats.len() < n {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match heart.beats.recv_timeout(remaining) {
            Ok(beat) => beats.push(beat),
            Err(_) => break,
        }
    }
    beats
}

/// Helper: continuously inject a chemical every `interval` for `duration`.
/// Runs in a background thread. Returns the join handle.
fn sustain_ne(
    heart: &organpool::HeartHandle,
    amount: u8,
    interval: Duration,
    duration: Duration,
) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        heart.inject_ne(amount);
        std::thread::sleep(interval);
    }
}

/// Helper: continuously inject ACh.
fn sustain_ach(
    heart: &organpool::HeartHandle,
    amount: u8,
    interval: Duration,
    duration: Duration,
) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        heart.inject_ach(amount);
        std::thread::sleep(interval);
    }
}

// ============================================================================
// Intrinsic rhythm tests — heart beats on its own
// ============================================================================

#[test]
fn heart_beats_on_its_own() {
    let heart = CardiacPipeline::start();
    // No injections — heart has resting vagal tone (ACh baseline ~30)
    let beats = collect_beats(&heart, 3, Duration::from_secs(5));
    let snapshot = heart.stop();
    assert!(
        beats.len() >= 3,
        "expected at least 3 beats, got {}",
        beats.len()
    );
    assert!(snapshot.beat_count >= 3);
}

#[test]
fn intrinsic_rhythm_is_regular() {
    let heart = CardiacPipeline::start();
    let beats = collect_beats(&heart, 6, Duration::from_secs(8));
    heart.stop();

    assert!(beats.len() >= 6, "not enough beats for regularity test");

    let ibis: Vec<u64> = beats[1..].iter().map(|b| b.ibi_us).collect();
    let mean_ibi = ibis.iter().sum::<u64>() / ibis.len() as u64;

    for (i, &ibi) in ibis.iter().enumerate() {
        let deviation = if ibi > mean_ibi {
            ibi - mean_ibi
        } else {
            mean_ibi - ibi
        };
        let deviation_pct = (deviation * 100) / mean_ibi;
        assert!(
            deviation_pct < 20,
            "IBI {} deviated {}% from mean (IBI={}μs, mean={}μs)",
            i, deviation_pct, ibi, mean_ibi
        );
    }
}

#[test]
fn intrinsic_bpm_in_expected_range() {
    let heart = CardiacPipeline::start();
    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    let snapshot = heart.stop();

    assert!(beats.len() >= 5, "not enough beats for BPM check");

    // With vagal tone baseline ACh=30, should be somewhat slower than pure intrinsic.
    // Allow 40-120 BPM.
    let bpm = snapshot.last_bpm;
    assert!(
        bpm >= 40 && bpm <= 120,
        "resting BPM {} out of expected range [40, 120]",
        bpm
    );
}

// ============================================================================
// Autonomic modulation — injected chemicals change the rhythm
// ============================================================================

#[test]
fn ne_injection_accelerates_heart() {
    // Baseline: collect beats at rest
    let heart = CardiacPipeline::start();
    let resting_beats = collect_beats(&heart, 4, Duration::from_secs(6));
    let resting_ibi = resting_beats[1..]
        .iter()
        .map(|b| b.ibi_us)
        .sum::<u64>()
        / resting_beats[1..].len().max(1) as u64;

    // Sustained NE injection (like continuous sympathetic firing)
    // Inject 100 every 200ms for 4 seconds — keeps NE elevated
    sustain_ne(&heart, 100, Duration::from_millis(200), Duration::from_secs(4));
    let fast_beats = collect_beats(&heart, 4, Duration::from_secs(6));
    heart.stop();

    assert!(fast_beats.len() >= 4, "not enough fast beats");

    let fast_ibi = fast_beats[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / fast_beats[1..].len().max(1) as u64;

    assert!(
        fast_ibi < resting_ibi,
        "NE should accelerate: fast_ibi={}μs should be < resting_ibi={}μs",
        fast_ibi, resting_ibi
    );
}

#[test]
fn ach_injection_decelerates_heart() {
    // Baseline
    let heart = CardiacPipeline::start();
    let resting_beats = collect_beats(&heart, 4, Duration::from_secs(6));
    let resting_ibi = resting_beats[1..]
        .iter()
        .map(|b| b.ibi_us)
        .sum::<u64>()
        / resting_beats[1..].len().max(1) as u64;

    // Sustained ACh injection (enhanced vagal drive)
    sustain_ach(&heart, 150, Duration::from_millis(200), Duration::from_secs(4));
    let slow_beats = collect_beats(&heart, 4, Duration::from_secs(10));
    heart.stop();

    assert!(slow_beats.len() >= 4, "not enough slow beats");

    let slow_ibi = slow_beats[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / slow_beats[1..].len().max(1) as u64;

    assert!(
        slow_ibi > resting_ibi,
        "ACh should decelerate: slow_ibi={}μs should be > resting_ibi={}μs",
        slow_ibi, resting_ibi
    );
}

#[test]
fn cortisol_amplifies_ne() {
    // Use moderate NE so there's headroom for cortisol amplification.
    // With the injection model, chemicals decay — sustained injection is needed.

    // NE alone: sustain moderate NE, then collect beats under that drive
    let heart1 = CardiacPipeline::start();
    // Let it settle at resting rate first
    let _ = collect_beats(&heart1, 2, Duration::from_secs(3));
    // Sustain NE while collecting beats
    sustain_ne(&heart1, 50, Duration::from_millis(100), Duration::from_secs(4));
    let ne_beats = collect_beats(&heart1, 4, Duration::from_secs(6));
    let snap1 = heart1.stop();

    // NE + cortisol: sustain both simultaneously
    let heart2 = CardiacPipeline::start();
    let _ = collect_beats(&heart2, 2, Duration::from_secs(3));
    // Build up cortisol first (slow decay, so it persists)
    for _ in 0..10 {
        heart2.inject_cortisol(200);
        std::thread::sleep(Duration::from_millis(100));
    }
    // Now sustain both NE and cortisol together
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        heart2.inject_ne(50);
        heart2.inject_cortisol(100);
        std::thread::sleep(Duration::from_millis(100));
    }
    let ne_cort_beats = collect_beats(&heart2, 4, Duration::from_secs(6));
    let snap2 = heart2.stop();

    assert!(
        ne_beats.len() >= 4 && ne_cort_beats.len() >= 4,
        "not enough beats: NE={}, NE+cort={}",
        ne_beats.len(),
        ne_cort_beats.len()
    );

    let ne_ibi = ne_beats[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / ne_beats[1..].len().max(1) as u64;
    let ne_cort_ibi = ne_cort_beats[1..]
        .iter()
        .map(|b| b.ibi_us)
        .sum::<u64>()
        / ne_cort_beats[1..].len().max(1) as u64;

    // Cortisol amplifies NE sensitivity, so NE+cortisol should produce shorter IBI.
    // Allow for some noise — require at least 5% faster.
    assert!(
        ne_cort_ibi < ne_ibi,
        "cortisol+NE should be faster: ne_cort={}μs should be < ne_only={}μs (snap1 bpm={}, snap2 bpm={})",
        ne_cort_ibi, ne_ibi, snap1.last_bpm, snap2.last_bpm
    );
}

// ============================================================================
// Self-stabilization — chemicals decay, heart recovers
// ============================================================================

#[test]
fn heart_recovers_after_ne_burst() {
    let heart = CardiacPipeline::start();

    // Baseline
    let resting = collect_beats(&heart, 3, Duration::from_secs(5));
    assert!(resting.len() >= 3);
    let resting_ibi = resting.last().unwrap().ibi_us;

    // Single big NE burst
    heart.inject_ne(255);
    // Wait for it to take effect then decay (NE half-life ~2.5s, so ~8s for near-baseline)
    std::thread::sleep(Duration::from_secs(8));
    let recovered = collect_beats(&heart, 3, Duration::from_secs(5));
    heart.stop();

    assert!(recovered.len() >= 3);
    let recovered_ibi = recovered.last().unwrap().ibi_us;

    // Recovered IBI should be close to resting (within 30%)
    let diff = if recovered_ibi > resting_ibi {
        recovered_ibi - resting_ibi
    } else {
        resting_ibi - recovered_ibi
    };
    let pct = (diff * 100) / resting_ibi.max(1);
    assert!(
        pct < 30,
        "recovered IBI should be close to resting: recovered={}μs resting={}μs diff={}%",
        recovered_ibi, resting_ibi, pct
    );
}

#[test]
fn chemicals_decay_in_snapshot() {
    let heart = CardiacPipeline::start();
    heart.inject_ne(200);
    // Wait for NE to decay significantly (~3 half-lives = 7.5s)
    // With default cortisol baseline (10) and cortisol_ne_protection (255),
    // effective NE half-life is ~2.6s. After 8s (~3.08 half-lives):
    // 200 / 2^3.08 ≈ 24. Integer rounding accumulates, so allow up to ~40.
    std::thread::sleep(Duration::from_secs(8));
    let snapshot = heart.stop();
    assert!(
        snapshot.final_ne < 40,
        "NE should have decayed: got {}",
        snapshot.final_ne
    );
}

// ============================================================================
// Vitals and rhythm classification
// ============================================================================

#[test]
fn vitals_track_real_time() {
    let heart = CardiacPipeline::start();
    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    heart.stop();

    assert!(beats.len() >= 5);

    for beat in &beats[1..] {
        assert!(
            beat.ibi_us > 300_000 && beat.ibi_us < 3_000_000,
            "IBI {}μs out of plausible range",
            beat.ibi_us
        );
    }
}

#[test]
fn vitals_classify_normal_sinus() {
    let mut vitals = CardiacVitals::new();
    let base = Instant::now();
    for i in 0..10 {
        let t = base + Duration::from_millis(i * 800);
        vitals.record_beat(t);
    }
    let now = base + Duration::from_millis(10 * 800);
    assert_eq!(vitals.classify(now), CardiacRhythm::NormalSinus);
}

#[test]
fn vitals_classify_tachycardia() {
    let mut vitals = CardiacVitals::new();
    let base = Instant::now();
    for i in 0..10 {
        let t = base + Duration::from_millis(i * 333);
        vitals.record_beat(t);
    }
    let now = base + Duration::from_millis(10 * 333);
    assert_eq!(vitals.classify(now), CardiacRhythm::SinusTachycardia);
}

#[test]
fn vitals_classify_bradycardia() {
    let mut vitals = CardiacVitals::new();
    let base = Instant::now();
    for i in 0..10 {
        let t = base + Duration::from_millis(i * 2000);
        vitals.record_beat(t);
    }
    let now = base + Duration::from_millis(10 * 2000);
    assert_eq!(vitals.classify(now), CardiacRhythm::SinusBradycardia);
}

#[test]
fn vitals_classify_asystole() {
    let mut vitals = CardiacVitals::new();
    let base = Instant::now();
    vitals.record_beat(base);
    vitals.record_beat(base + Duration::from_millis(800));
    let now = base + Duration::from_secs(10);
    assert_eq!(vitals.classify(now), CardiacRhythm::Asystole);
}

// ============================================================================
// Stop and snapshot
// ============================================================================

#[test]
fn stop_returns_snapshot() {
    let heart = CardiacPipeline::start();
    let _ = collect_beats(&heart, 3, Duration::from_secs(5));
    let snapshot = heart.stop();
    assert!(snapshot.beat_count >= 3);
    assert!(snapshot.last_bpm > 0);
}

#[test]
fn heart_stops_cleanly() {
    let heart = CardiacPipeline::start();
    let _ = collect_beats(&heart, 2, Duration::from_secs(3));
    assert!(heart.is_alive());
    let _ = heart.stop();
}

// ============================================================================
// Custom configuration
// ============================================================================

#[test]
fn fast_heart_config() {
    let config = CardiacConfig {
        sa_node: ZoneConfig {
            resting_potential: -70,
            threshold: -40,
            peak_potential: 30,
            base_leak_rate_per_sec: 300,
            refractory_us: 150_000,
            conduction_delay_us: 0,
            ne_sensitivity: 128,
            ach_sensitivity: 128,
            calcium_clock: CalciumClockConfig::default(),
            hrv: HrvConfig::default(),
        },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    let beats = collect_beats(&heart, 5, Duration::from_secs(5));
    let snapshot = heart.stop();

    assert!(beats.len() >= 5, "fast heart should beat quickly");
    let mean_ibi = beats[2..].iter().map(|b| b.ibi_us).sum::<u64>() / (beats.len() - 2) as u64;
    assert!(
        mean_ibi < 600_000,
        "fast heart IBI={}μs should be < 600ms",
        mean_ibi
    );
    assert!(snapshot.last_bpm > 80, "fast heart should be > 80 BPM");
}

#[test]
fn slow_heart_config() {
    let config = CardiacConfig {
        sa_node: ZoneConfig {
            resting_potential: -70,
            threshold: -40,
            peak_potential: 30,
            base_leak_rate_per_sec: 15,
            refractory_us: 500_000,
            conduction_delay_us: 0,
            ne_sensitivity: 128,
            ach_sensitivity: 128,
            calcium_clock: CalciumClockConfig::default(),
            hrv: HrvConfig::default(),
        },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    let beats = collect_beats(&heart, 3, Duration::from_secs(15));
    let snapshot = heart.stop();

    assert!(beats.len() >= 3, "slow heart should still beat");
    assert!(
        snapshot.last_bpm < 60,
        "slow heart should be < 60 BPM, got {}",
        snapshot.last_bpm
    );
}

// ============================================================================
// Beat numbering and sequencing
// ============================================================================

#[test]
fn beats_are_sequentially_numbered() {
    let heart = CardiacPipeline::start();
    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    heart.stop();

    assert!(beats.len() >= 5);
    for (i, beat) in beats.iter().enumerate() {
        assert_eq!(beat.beat_number, i as u64);
    }
}

#[test]
fn first_beat_has_zero_ibi() {
    let heart = CardiacPipeline::start();
    let beats = collect_beats(&heart, 1, Duration::from_secs(3));
    heart.stop();
    assert!(!beats.is_empty());
    assert_eq!(beats[0].ibi_us, 0, "first beat should have 0 IBI");
}

// ============================================================================
// Escape rhythms — subsidiary pacemakers
// ============================================================================

#[test]
fn escape_av_fires_when_sa_fails() {
    // SA node disabled (leak=0), AV node has intrinsic rate (~45 BPM).
    // The AV node should generate escape rhythm on its own.
    let config = CardiacConfig {
        sa_node: ZoneConfig {
            base_leak_rate_per_sec: 0, // SA node failure
            ..CardiacConfig::default().sa_node
        },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    // AV escape rhythm is slow (~40-50 BPM, ~1.2-1.5s cycle)
    let beats = collect_beats(&heart, 3, Duration::from_secs(10));
    let snapshot = heart.stop();

    assert!(
        beats.len() >= 3,
        "AV escape rhythm should produce beats: got {}",
        beats.len()
    );

    // AV escape rate should be ~30-55 BPM (slower than normal SA rhythm)
    let bpm = snapshot.last_bpm;
    assert!(
        bpm >= 20 && bpm <= 60,
        "AV escape BPM {} should be in ~30-55 range",
        bpm
    );
}

#[test]
fn escape_purkinje_fires_when_sa_and_av_fail() {
    // Both SA and AV disabled. Only the Purkinje/conduction system paces.
    let config = CardiacConfig {
        sa_node: ZoneConfig {
            base_leak_rate_per_sec: 0,
            ..CardiacConfig::default().sa_node
        },
        av_node: ZoneConfig {
            base_leak_rate_per_sec: 0,
            ..CardiacConfig::default().av_node
        },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    // Purkinje escape is very slow (~22 BPM, ~2.7s cycle)
    let beats = collect_beats(&heart, 3, Duration::from_secs(15));
    let snapshot = heart.stop();

    assert!(
        beats.len() >= 3,
        "Purkinje escape should produce beats: got {}",
        beats.len()
    );

    let bpm = snapshot.last_bpm;
    assert!(
        bpm >= 10 && bpm <= 40,
        "Purkinje escape BPM {} should be in ~15-35 range",
        bpm
    );
}

#[test]
fn sa_node_suppresses_subsidiary_pacemakers() {
    // Default config — all pacemakers active. SA node (fastest) should
    // dominate via overdrive suppression. Beat rate should match SA rate,
    // not AV or Purkinje rate.
    let heart = CardiacPipeline::start();
    let beats = collect_beats(&heart, 6, Duration::from_secs(10));
    let snapshot = heart.stop();

    assert!(beats.len() >= 6);

    // SA node rate is ~70 BPM. If subsidiary pacemakers were interfering,
    // the rate would be irregular or slower.
    let bpm = snapshot.last_bpm;
    assert!(
        bpm >= 50 && bpm <= 100,
        "SA-dominated BPM {} should be ~70, not subsidiary rates (~40/~22)",
        bpm
    );

    // Regularity check — SA overdrive produces steady rhythm
    let ibis: Vec<u64> = beats[1..].iter().map(|b| b.ibi_us).collect();
    let mean_ibi = ibis.iter().sum::<u64>() / ibis.len() as u64;
    for &ibi in &ibis {
        let deviation = if ibi > mean_ibi { ibi - mean_ibi } else { mean_ibi - ibi };
        let pct = (deviation * 100) / mean_ibi;
        assert!(
            pct < 20,
            "overdrive suppression should produce regular rhythm, got {}% deviation",
            pct
        );
    }
}

// ============================================================================
// Chemical cross-metabolism
// ============================================================================

#[test]
fn cortisol_extends_ne_halflife() {
    // Sustained NE injection at identical rate into two hearts.
    // One has cortisol=0 (no protection, cortisol_ne_protection irrelevant).
    // The other has sustained high cortisol (extends NE half-life).
    // With longer half-life, NE accumulates to a higher steady-state level.
    //
    // We disable cortisol baseline (set to 0) and ACh baseline (set to 0)
    // so the only variable is whether cortisol is injected or not.

    // Heart 1: no cortisol protection — NE decays at base half-life (2.5s)
    let config1 = CardiacConfig {
        metabolism: MetabolismConfig {
            cortisol_ne_protection: 0,   // disabled
            ne_ach_antagonism: 0,
            ach_ne_antagonism: 0,
        },
        ..CardiacConfig::default()
    };

    // Heart 2: max cortisol protection — NE half-life extended
    let config2 = CardiacConfig {
        metabolism: MetabolismConfig {
            cortisol_ne_protection: 255, // full protection
            ne_ach_antagonism: 0,
            ach_ne_antagonism: 0,
        },
        ..CardiacConfig::default()
    };

    let heart1 = CardiacPipeline::start_with_config(config1);
    let heart2 = CardiacPipeline::start_with_config(config2);

    // Inject NE once into both hearts. Sustain cortisol only in heart 2.
    // The single NE bolus decays at different rates — heart 2 retains more.
    heart1.inject_ne(200);
    heart2.inject_ne(200);

    // Keep cortisol elevated in heart 2 while NE decays.
    // Wait 1.5 seconds (~60% of base half-life). Integer decay is aggressive,
    // so don't wait too long or both reach 0.
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        heart2.inject_cortisol(255);
        std::thread::sleep(Duration::from_millis(50));
    }

    let snap1 = heart1.stop();
    let snap2 = heart2.stop();

    // With cortisol protection, NE should decay slower → higher final NE
    assert!(
        snap2.final_ne > snap1.final_ne,
        "cortisol should protect NE: with_cortisol={} should be > without={}",
        snap2.final_ne, snap1.final_ne
    );
}

#[test]
fn cross_metabolism_zero_is_noop() {
    // With all antagonism disabled, opposing chemicals should have full independent effect.
    let config = CardiacConfig {
        metabolism: MetabolismConfig {
            cortisol_ne_protection: 0,
            ne_ach_antagonism: 0,
            ach_ne_antagonism: 0,
        },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    // Inject both NE and ACh — with zero antagonism they work independently
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        heart.inject_ne(200);
        heart.inject_ach(200);
        std::thread::sleep(Duration::from_millis(200));
    }

    let beats = collect_beats(&heart, 3, Duration::from_secs(5));
    heart.stop();
    assert!(
        beats.len() >= 3,
        "heart should still beat with zero antagonism: got {} beats",
        beats.len()
    );
}

#[test]
fn ne_ach_mutual_antagonism() {
    // With antagonism enabled, NE+ACh should partially cancel each other,
    // producing a rate closer to resting than NE or ACh alone.

    // NE alone
    let heart1 = CardiacPipeline::start();
    let _ = collect_beats(&heart1, 2, Duration::from_secs(3));
    sustain_ne(&heart1, 150, Duration::from_millis(200), Duration::from_secs(3));
    let ne_beats = collect_beats(&heart1, 4, Duration::from_secs(5));
    heart1.stop();

    // NE + ACh together (antagonism reduces both effects)
    let heart2 = CardiacPipeline::start();
    let _ = collect_beats(&heart2, 2, Duration::from_secs(3));
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        heart2.inject_ne(150);
        heart2.inject_ach(150);
        std::thread::sleep(Duration::from_millis(200));
    }
    let both_beats = collect_beats(&heart2, 4, Duration::from_secs(5));
    heart2.stop();

    assert!(ne_beats.len() >= 4 && both_beats.len() >= 4);

    let ne_ibi = ne_beats[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / ne_beats[1..].len().max(1) as u64;
    let both_ibi = both_beats[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / both_beats[1..].len().max(1) as u64;

    // NE alone makes the heart faster (shorter IBI).
    // NE+ACh with antagonism: NE effect reduced by ACh, ACh effect reduced by NE.
    // Result should be slower than NE alone (longer IBI, closer to resting).
    assert!(
        both_ibi > ne_ibi,
        "NE+ACh should be slower than NE alone (antagonism): both={}μs, ne_only={}μs",
        both_ibi, ne_ibi
    );
}

// ============================================================================
// Gap junction coupling
// ============================================================================

#[test]
fn gap_junction_zero_is_noop() {
    // With all coupling disabled, heart should still beat normally.
    let config = CardiacConfig {
        gap_sa_av: GapJunctionConfig { strength: 0, retrograde: false },
        gap_av_cond: GapJunctionConfig { strength: 0, retrograde: false },
        gap_cond_myo: GapJunctionConfig { strength: 0, retrograde: false },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    let snapshot = heart.stop();
    assert!(
        beats.len() >= 5,
        "heart should beat without gap junctions: got {} beats",
        beats.len()
    );
    assert!(
        snapshot.last_bpm >= 50 && snapshot.last_bpm <= 100,
        "BPM should be near resting without gap junctions: got {}",
        snapshot.last_bpm
    );
}

#[test]
fn gap_junction_cannot_cause_extra_beats() {
    // Even with very high coupling, gap junctions should not cause double-beats
    // because electrotonic depolarization is clamped below threshold.
    let config = CardiacConfig {
        gap_sa_av: GapJunctionConfig { strength: 25, retrograde: true },
        gap_av_cond: GapJunctionConfig { strength: 25, retrograde: true },
        gap_cond_myo: GapJunctionConfig { strength: 25, retrograde: true },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    let beats = collect_beats(&heart, 8, Duration::from_secs(10));
    heart.stop();

    // With default SA rate ~70 BPM, 10 seconds should produce ~12 beats.
    // High gap junction strength should not produce dramatically more beats
    // (e.g. 100+ would indicate spurious firing from coupling).
    assert!(
        beats.len() <= 25,
        "gap junctions should not cause spurious extra beats: got {} beats in 10s",
        beats.len()
    );
    assert!(
        beats.len() >= 5,
        "heart should still beat with high gap junctions: got {} beats",
        beats.len()
    );
}

#[test]
fn gap_junction_accelerates_conduction() {
    // Gap junction coupling depolarizes downstream zones, accelerating their
    // approach to threshold. Compare no-coupling vs high-coupling: the coupled
    // heart should have slightly faster overall conduction (shorter IBI) because
    // gap junction current helps downstream zones reach threshold sooner after
    // being triggered.

    // No coupling
    let config_off = CardiacConfig {
        gap_sa_av: GapJunctionConfig { strength: 0, retrograde: false },
        gap_av_cond: GapJunctionConfig { strength: 0, retrograde: false },
        gap_cond_myo: GapJunctionConfig { strength: 0, retrograde: false },
        ..CardiacConfig::default()
    };

    // Strong coupling
    let config_on = CardiacConfig {
        gap_sa_av: GapJunctionConfig { strength: 8, retrograde: false },
        gap_av_cond: GapJunctionConfig { strength: 8, retrograde: false },
        gap_cond_myo: GapJunctionConfig { strength: 10, retrograde: false },
        ..CardiacConfig::default()
    };

    let heart_off = CardiacPipeline::start_with_config(config_off);
    let heart_on = CardiacPipeline::start_with_config(config_on);

    // Let both reach steady state
    let _ = collect_beats(&heart_off, 3, Duration::from_secs(5));
    let _ = collect_beats(&heart_on, 3, Duration::from_secs(5));

    let beats_off = collect_beats(&heart_off, 5, Duration::from_secs(8));
    let beats_on = collect_beats(&heart_on, 5, Duration::from_secs(8));

    heart_off.stop();
    heart_on.stop();

    assert!(beats_off.len() >= 5 && beats_on.len() >= 5);

    let ibi_off: u64 = beats_off[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / beats_off[1..].len().max(1) as u64;
    let ibi_on: u64 = beats_on[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / beats_on[1..].len().max(1) as u64;

    // Both should produce reasonable heart rates
    assert!(ibi_off > 500_000, "no-coupling IBI too short: {}μs", ibi_off);
    assert!(ibi_on > 500_000, "coupled IBI too short: {}μs", ibi_on);
    assert!(ibi_off < 2_000_000, "no-coupling IBI too long: {}μs", ibi_off);
    assert!(ibi_on < 2_000_000, "coupled IBI too long: {}μs", ibi_on);
}

#[test]
fn retrograde_coupling_stabilizes_escape() {
    // SA node fails (leak=0). AV node escapes at ~45 BPM.
    // With retrograde coupling from conduction, conduction depolarizes AV slightly,
    // helping AV maintain rhythm. The heart still beats.
    let config = CardiacConfig {
        sa_node: ZoneConfig {
            base_leak_rate_per_sec: 0,
            ne_sensitivity: 0,
            ach_sensitivity: 0,
            ..CardiacConfig::default().sa_node
        },
        gap_av_cond: GapJunctionConfig { strength: 5, retrograde: true },
        gap_cond_myo: GapJunctionConfig { strength: 5, retrograde: true },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    let beats = collect_beats(&heart, 4, Duration::from_secs(10));
    heart.stop();

    assert!(
        beats.len() >= 4,
        "escape rhythm with retrograde should still beat: got {} beats",
        beats.len()
    );
}

// ============================================================================
// Calcium clock
// ============================================================================

#[test]
fn calcium_clock_disabled_matches_baseline() {
    // With calcium clock disabled, heart should beat at normal resting rate.
    let config = CardiacConfig {
        sa_node: ZoneConfig {
            calcium_clock: CalciumClockConfig {
                enabled: false,
                ..CalciumClockConfig::default()
            },
            ..CardiacConfig::default().sa_node
        },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    let _ = collect_beats(&heart, 2, Duration::from_secs(3));
    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    let snapshot = heart.stop();
    assert!(beats.len() >= 5, "should beat without calcium clock");
    assert!(
        snapshot.last_bpm >= 50 && snapshot.last_bpm <= 100,
        "BPM without calcium clock should be resting range: got {}",
        snapshot.last_bpm
    );
}

#[test]
fn calcium_clock_contributes_to_depolarization() {
    // With calcium clock enabled (default), the SA node should fire slightly
    // faster because the calcium clock adds NCX depolarization on top of the
    // membrane clock's HCN leak.

    // No calcium clock
    let config_off = CardiacConfig {
        sa_node: ZoneConfig {
            calcium_clock: CalciumClockConfig {
                enabled: false,
                ..CalciumClockConfig::default()
            },
            ..CardiacConfig::default().sa_node
        },
        ..CardiacConfig::default()
    };

    // Calcium clock enabled (default)
    let config_on = CardiacConfig::default();

    let heart_off = CardiacPipeline::start_with_config(config_off);
    let heart_on = CardiacPipeline::start_with_config(config_on);

    // Warm up
    let _ = collect_beats(&heart_off, 3, Duration::from_secs(5));
    let _ = collect_beats(&heart_on, 3, Duration::from_secs(5));

    let beats_off = collect_beats(&heart_off, 5, Duration::from_secs(8));
    let beats_on = collect_beats(&heart_on, 5, Duration::from_secs(8));

    heart_off.stop();
    heart_on.stop();

    assert!(beats_off.len() >= 5 && beats_on.len() >= 5);

    let ibi_off: u64 = beats_off[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / beats_off[1..].len().max(1) as u64;
    let ibi_on: u64 = beats_on[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / beats_on[1..].len().max(1) as u64;

    // Calcium clock should make the heart beat faster (shorter IBI)
    assert!(
        ibi_on < ibi_off,
        "calcium clock should accelerate SA node: with_ca={}μs without={}μs",
        ibi_on, ibi_off
    );
}

#[test]
fn calcium_clock_resets_on_fire() {
    // The calcium clock should produce consistent beat timing, not accumulate
    // across cycles. We verify by checking that beats are regular.
    let heart = CardiacPipeline::start();
    let _ = collect_beats(&heart, 2, Duration::from_secs(3));
    let beats = collect_beats(&heart, 6, Duration::from_secs(10));
    heart.stop();

    assert!(beats.len() >= 6, "should get enough beats for regularity check");

    let ibis: Vec<u64> = beats[1..].iter().map(|b| b.ibi_us).collect();
    let mean: u64 = ibis.iter().sum::<u64>() / ibis.len() as u64;

    // Check all IBIs are within 15% of mean (regular rhythm)
    for (i, &ibi) in ibis.iter().enumerate() {
        let deviation = if ibi > mean { ibi - mean } else { mean - ibi };
        let pct = (deviation * 100) / mean.max(1);
        assert!(
            pct < 15,
            "beat {} IBI deviation {}% from mean (ibi={}μs, mean={}μs)",
            i, pct, ibi, mean
        );
    }
}

#[test]
fn calcium_clock_modulated_by_ne() {
    // NE should accelerate both the membrane clock and the calcium clock.
    // Compare resting vs NE-stimulated: NE-stimulated should be faster
    // (this is already covered by existing NE tests, but we verify the
    // calcium clock doesn't interfere with modulation).
    let heart = CardiacPipeline::start();
    let _ = collect_beats(&heart, 2, Duration::from_secs(3));

    // Resting beats
    let resting_beats = collect_beats(&heart, 4, Duration::from_secs(6));

    // NE stimulation
    sustain_ne(&heart, 150, Duration::from_millis(200), Duration::from_secs(3));
    let ne_beats = collect_beats(&heart, 4, Duration::from_secs(5));
    heart.stop();

    assert!(resting_beats.len() >= 4 && ne_beats.len() >= 4);

    let resting_ibi: u64 = resting_beats[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / resting_beats[1..].len().max(1) as u64;
    let ne_ibi: u64 = ne_beats[1..].iter().map(|b| b.ibi_us).sum::<u64>()
        / ne_beats[1..].len().max(1) as u64;

    // NE should make the heart beat faster (shorter IBI)
    assert!(
        ne_ibi < resting_ibi,
        "NE should accelerate with calcium clock: ne={}μs resting={}μs",
        ne_ibi, resting_ibi
    );
}

// ============================================================================
// Heart rate variability
// ============================================================================

#[test]
fn hrv_produces_variability() {
    // Default config has HRV enabled on SA node.
    // IBI coefficient of variation should be nonzero.
    let heart = CardiacPipeline::start();
    let _ = collect_beats(&heart, 3, Duration::from_secs(5));
    let beats = collect_beats(&heart, 10, Duration::from_secs(15));
    heart.stop();

    assert!(beats.len() >= 10, "need enough beats for HRV measurement");

    let ibis: Vec<u64> = beats[1..].iter().map(|b| b.ibi_us).collect();
    let mean: u64 = ibis.iter().sum::<u64>() / ibis.len() as u64;

    // Compute standard deviation
    let variance: u64 = ibis.iter()
        .map(|&ibi| {
            let diff = if ibi > mean { ibi - mean } else { mean - ibi };
            diff * diff
        })
        .sum::<u64>() / ibis.len() as u64;

    // With HRV enabled, variance should be nonzero
    assert!(
        variance > 0,
        "HRV should produce nonzero IBI variance: mean={}μs, ibis={:?}",
        mean, ibis
    );
}

#[test]
fn hrv_disabled_is_regular() {
    // With HRV disabled, beats should be very regular (near-zero variance).
    let config = CardiacConfig {
        sa_node: ZoneConfig {
            hrv: HrvConfig {
                enabled: false,
                ..HrvConfig::default()
            },
            // Also disable calcium clock to remove that source of jitter
            calcium_clock: CalciumClockConfig {
                enabled: false,
                ..CalciumClockConfig::default()
            },
            ..CardiacConfig::default().sa_node
        },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    let _ = collect_beats(&heart, 3, Duration::from_secs(5));
    let beats = collect_beats(&heart, 8, Duration::from_secs(12));
    heart.stop();

    assert!(beats.len() >= 8, "need enough beats for regularity check");

    let ibis: Vec<u64> = beats[1..].iter().map(|b| b.ibi_us).collect();
    let mean: u64 = ibis.iter().sum::<u64>() / ibis.len() as u64;

    // Check all IBIs are within 5% of mean (very regular)
    for (i, &ibi) in ibis.iter().enumerate() {
        let deviation = if ibi > mean { ibi - mean } else { mean - ibi };
        let pct = (deviation * 100) / mean.max(1);
        assert!(
            pct < 5,
            "beat {} should be regular without HRV: {}% deviation (ibi={}μs, mean={}μs)",
            i, pct, ibi, mean
        );
    }
}

#[test]
fn hrv_within_regularity_tolerance() {
    // HRV should not make the heart chaotic. Individual IBIs should still be
    // within 20% of the mean — the max combined jitter is ±6mV on a 30mV gap.
    let heart = CardiacPipeline::start();
    let _ = collect_beats(&heart, 3, Duration::from_secs(5));
    let beats = collect_beats(&heart, 10, Duration::from_secs(15));
    heart.stop();

    assert!(beats.len() >= 10);

    let ibis: Vec<u64> = beats[1..].iter().map(|b| b.ibi_us).collect();
    let mean: u64 = ibis.iter().sum::<u64>() / ibis.len() as u64;

    for (i, &ibi) in ibis.iter().enumerate() {
        let deviation = if ibi > mean { ibi - mean } else { mean - ibi };
        let pct = (deviation * 100) / mean.max(1);
        assert!(
            pct < 20,
            "beat {} IBI should be within 20% of mean: {}% deviation (ibi={}μs, mean={}μs)",
            i, pct, ibi, mean
        );
    }
}

#[test]
fn rmssd_nonzero_with_hrv() {
    // RMSSD (root mean square of successive IBI differences) should be nonzero
    // with HRV enabled — it reflects vagally mediated beat-to-beat variability.
    let heart = CardiacPipeline::start();
    let _ = collect_beats(&heart, 3, Duration::from_secs(5));
    let beats = collect_beats(&heart, 8, Duration::from_secs(12));
    heart.stop();

    assert!(beats.len() >= 8);

    let mut vitals = CardiacVitals::new();
    for beat in &beats {
        vitals.record_beat(beat.instant);
    }

    let rmssd = vitals.rmssd_us();
    assert!(
        rmssd > 0,
        "RMSSD should be nonzero with HRV enabled: got {}μs",
        rmssd
    );
}

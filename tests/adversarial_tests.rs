//! Adversarial, stress, and load tests for the autonomous cardiac system.
//!
//! The heart has its own chemical environment. Tests inject chemicals and
//! observe emergent behavior. Chemicals decay — sustained effects require
//! repeated injection (like real autonomic nerve firing).

use organpool::{CalciumClockConfig, CardiacConfig, CardiacPipeline, CardiacRhythm, CardiacVitals, HrvConfig, ZoneConfig};
use std::time::{Duration, Instant};

#[cfg(feature = "fibertract")]
use organpool::AutonomicBridge;

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

/// Helper: drain all beats within a time window.
fn drain_beats(heart: &organpool::HeartHandle, window: Duration) -> Vec<organpool::BeatEvent> {
    let mut beats = Vec::new();
    let deadline = Instant::now() + window;
    loop {
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

/// Helper: compute mean IBI from beats (skipping first with ibi=0).
fn mean_ibi(beats: &[organpool::BeatEvent]) -> u64 {
    let ibis: Vec<u64> = beats.iter().filter(|b| b.ibi_us > 0).map(|b| b.ibi_us).collect();
    if ibis.is_empty() { return 0; }
    ibis.iter().sum::<u64>() / ibis.len() as u64
}

/// Helper: continuously inject NE for a duration.
fn sustain_ne(heart: &organpool::HeartHandle, amount: u8, interval: Duration, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        heart.inject_ne(amount);
        std::thread::sleep(interval);
    }
}

/// Helper: continuously inject ACh for a duration.
fn sustain_ach(heart: &organpool::HeartHandle, amount: u8, interval: Duration, duration: Duration) {
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        heart.inject_ach(amount);
        std::thread::sleep(interval);
    }
}

// ═══════════════════════════════════════════════════════════════════════
// STRESS TESTS — extreme chemical injection
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn max_sympathetic_stress_still_beats() {
    // Sustained maximum NE + cortisol injection
    let heart = CardiacPipeline::start();

    // Continuously inject max NE + cortisol every 100ms for 6 seconds
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        heart.inject_ne(255);
        heart.inject_cortisol(255);
        std::thread::sleep(Duration::from_millis(100));
    }

    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    let snapshot = heart.stop();

    assert!(
        beats.len() >= 5,
        "heart must still beat under max stress: got {} beats",
        beats.len()
    );
    assert!(snapshot.last_bpm > 0, "BPM should be calculable");
}

#[test]
fn max_parasympathetic_still_beats() {
    // Sustained maximum ACh injection
    let heart = CardiacPipeline::start();

    // Continuously inject max ACh every 200ms for 15 seconds
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        heart.inject_ach(255);
        std::thread::sleep(Duration::from_millis(200));
    }

    // Even at max ACh, floor clamp on leak rate keeps heart beating
    let beats = collect_beats(&heart, 2, Duration::from_secs(10));
    heart.stop();

    assert!(
        !beats.is_empty(),
        "heart must still beat at max ACh"
    );
}

#[test]
fn opposing_chemicals_still_beats() {
    let heart = CardiacPipeline::start();

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        heart.inject_ne(200);
        heart.inject_ach(200);
        std::thread::sleep(Duration::from_millis(200));
    }

    let beats = collect_beats(&heart, 3, Duration::from_secs(8));
    heart.stop();

    assert!(
        beats.len() >= 3,
        "heart must beat with opposing chemicals: got {} beats",
        beats.len()
    );
}

#[test]
fn all_chemicals_maxed() {
    let heart = CardiacPipeline::start();

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        heart.inject_ne(255);
        heart.inject_ach(255);
        heart.inject_cortisol(255);
        std::thread::sleep(Duration::from_millis(200));
    }

    let beats = collect_beats(&heart, 3, Duration::from_secs(8));
    heart.stop();

    assert!(
        beats.len() >= 3,
        "heart must beat with all chemicals maxed: got {}",
        beats.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// RAPID OSCILLATION — injection patterns change rapidly
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn rapid_ne_oscillation() {
    let heart = CardiacPipeline::start();

    // Alternate between NE bursts and silence every 200ms
    for cycle in 0..20 {
        if cycle % 2 == 0 {
            heart.inject_ne(200);
        }
        // On odd cycles, don't inject — NE decays naturally
        std::thread::sleep(Duration::from_millis(200));
    }

    let beats = collect_beats(&heart, 2, Duration::from_secs(3));
    let snapshot = heart.stop();

    assert!(snapshot.beat_count >= 3, "heart should survive rapid NE oscillation");
    assert!(beats.len() >= 2);
}

#[test]
fn rapid_ach_oscillation() {
    let heart = CardiacPipeline::start();

    for cycle in 0..20 {
        if cycle % 2 == 0 {
            heart.inject_ach(200);
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    let beats = collect_beats(&heart, 2, Duration::from_secs(3));
    let snapshot = heart.stop();

    assert!(snapshot.beat_count >= 3, "heart should survive rapid ACh oscillation");
    assert!(beats.len() >= 2);
}

#[test]
fn alternating_stress_and_calm() {
    let heart = CardiacPipeline::start();

    for cycle in 0..10 {
        if cycle % 2 == 0 {
            // Stress burst
            heart.inject_ne(200);
            heart.inject_cortisol(200);
        } else {
            // Calm burst (extra vagal)
            heart.inject_ach(200);
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    let beats = collect_beats(&heart, 2, Duration::from_secs(3));
    heart.stop();
    assert!(beats.len() >= 2);
}

// ═══════════════════════════════════════════════════════════════════════
// SUSTAINED STRESS — continuous injection over time
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn sustained_high_ne() {
    let heart = CardiacPipeline::start();

    // Sustained NE injection for 6 seconds
    sustain_ne(&heart, 150, Duration::from_millis(200), Duration::from_secs(6));
    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    heart.stop();

    assert!(beats.len() >= 5, "should beat continuously under sustained NE");

    // IBI should be consistent (not drifting)
    let ibis: Vec<u64> = beats[1..].iter().map(|b| b.ibi_us).collect();
    if ibis.len() >= 4 {
        let first_half = ibis[0..ibis.len() / 2].iter().sum::<u64>() / (ibis.len() / 2) as u64;
        let second_half = ibis[ibis.len() / 2..].iter().sum::<u64>()
            / (ibis.len() - ibis.len() / 2) as u64;
        let drift_pct = if first_half > second_half {
            ((first_half - second_half) * 100) / first_half
        } else {
            ((second_half - first_half) * 100) / second_half.max(1)
        };
        assert!(
            drift_pct < 30,
            "IBI should not drift: first={}μs second={}μs drift={}%",
            first_half, second_half, drift_pct
        );
    }
}

#[test]
fn sustained_high_ach() {
    let heart = CardiacPipeline::start();

    sustain_ach(&heart, 150, Duration::from_millis(200), Duration::from_secs(6));
    let beats = collect_beats(&heart, 3, Duration::from_secs(8));
    heart.stop();

    assert!(
        beats.len() >= 3,
        "should still beat under sustained ACh: got {} beats",
        beats.len()
    );
}

// ═══════════════════════════════════════════════════════════════════════
// DECAY AND RECOVERY — chemicals fade, heart returns to baseline
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn ne_decays_after_single_burst() {
    let heart = CardiacPipeline::start();

    // Single big NE burst
    heart.inject_ne(255);

    // Wait for multiple half-lives (~8s = ~3 half-lives)
    std::thread::sleep(Duration::from_secs(8));

    let snapshot = heart.stop();
    // NE should have decayed to near baseline (0)
    assert!(
        snapshot.final_ne < 40,
        "NE should decay after single burst: got {}",
        snapshot.final_ne
    );
}

#[test]
fn stress_then_natural_recovery() {
    let heart = CardiacPipeline::start();

    // Baseline
    let resting = collect_beats(&heart, 3, Duration::from_secs(5));
    let resting_ibi = mean_ibi(&resting);

    // Stress phase: sustained NE
    sustain_ne(&heart, 200, Duration::from_millis(200), Duration::from_secs(3));
    let _stressed = collect_beats(&heart, 3, Duration::from_secs(5));

    // Recovery: stop injecting, wait for decay
    std::thread::sleep(Duration::from_secs(8));
    let recovered = collect_beats(&heart, 3, Duration::from_secs(5));
    let recovered_ibi = mean_ibi(&recovered);
    heart.stop();

    assert!(resting.len() >= 3 && recovered.len() >= 3);

    // Recovered IBI should be close to resting
    let diff = if recovered_ibi > resting_ibi {
        recovered_ibi - resting_ibi
    } else {
        resting_ibi - recovered_ibi
    };
    let pct = (diff * 100) / resting_ibi.max(1);
    assert!(
        pct < 35,
        "recovered IBI should return to baseline: recovered={}μs resting={}μs diff={}%",
        recovered_ibi, resting_ibi, pct
    );
}

// ═══════════════════════════════════════════════════════════════════════
// REFRACTORY / PHYSICS CEILING TESTS
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn refractory_prevents_unbounded_rate() {
    let heart = CardiacPipeline::start();

    // Sustained max NE + cortisol
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
        heart.inject_ne(255);
        heart.inject_cortisol(255);
        std::thread::sleep(Duration::from_millis(100));
    }

    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    let snapshot = heart.stop();

    assert!(beats.len() >= 5);

    for beat in &beats[2..] {
        assert!(
            beat.ibi_us >= 100_000,
            "IBI={}μs should not drop below physics floor",
            beat.ibi_us
        );
    }

    assert!(
        snapshot.last_bpm < 500,
        "BPM {} should be bounded by refractory",
        snapshot.last_bpm
    );
}

#[test]
fn extremely_fast_config_still_bounded() {
    let config = CardiacConfig {
        sa_node: ZoneConfig {
            resting_potential: -70,
            threshold: -40,
            peak_potential: 30,
            base_leak_rate_per_sec: 10_000,
            refractory_us: 50_000,
            conduction_delay_us: 0,
            ne_sensitivity: 255,
            ach_sensitivity: 0,
            calcium_clock: CalciumClockConfig::default(),
            hrv: HrvConfig::default(),
        },
        av_node: ZoneConfig {
            refractory_us: 50_000,
            conduction_delay_us: 10_000,
            ..CardiacConfig::default().av_node
        },
        conduction: ZoneConfig {
            refractory_us: 50_000,
            conduction_delay_us: 5_000,
            ..CardiacConfig::default().conduction
        },
        myocardium: ZoneConfig {
            refractory_us: 50_000,
            conduction_delay_us: 0,
            ..CardiacConfig::default().myocardium
        },
        ..CardiacConfig::default()
    };

    let heart = CardiacPipeline::start_with_config(config);
    // Sustained NE
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        heart.inject_ne(255);
        std::thread::sleep(Duration::from_millis(100));
    }

    let beats = collect_beats(&heart, 20, Duration::from_secs(10));
    heart.stop();

    assert!(beats.len() >= 20);
    for beat in &beats[2..] {
        assert!(
            beat.ibi_us >= 30_000,
            "IBI={}μs below physical minimum",
            beat.ibi_us
        );
    }
}

// ═══════════════════════════════════════════════════════════════════════
// SELF-OBSERVATION
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn self_observation_stress_classification() {
    let heart = CardiacPipeline::start();

    let _ = collect_beats(&heart, 4, Duration::from_secs(5));

    // Sustained stress
    sustain_ne(&heart, 200, Duration::from_millis(200), Duration::from_secs(3));
    let stressed_beats = collect_beats(&heart, 5, Duration::from_secs(5));

    let mut vitals = CardiacVitals::new();
    for beat in &stressed_beats {
        vitals.record_beat(beat.instant);
    }

    let rhythm = vitals.classify(Instant::now());
    assert!(
        rhythm != CardiacRhythm::Arrhythmia
            && rhythm != CardiacRhythm::Fibrillation
            && rhythm != CardiacRhythm::Asystole,
        "stress rhythm should be regular fast, got {:?}",
        rhythm
    );

    heart.stop();
}

#[test]
fn self_observation_recovery_classification() {
    let heart = CardiacPipeline::start();

    sustain_ne(&heart, 200, Duration::from_millis(200), Duration::from_secs(2));
    let _ = collect_beats(&heart, 3, Duration::from_secs(3));

    // Wait for NE to decay and heart to stabilize at resting rate
    std::thread::sleep(Duration::from_secs(8));

    // Drain stale beats from the channel (accumulated during recovery sleep)
    while heart.beats.try_recv().is_ok() {}

    // Collect fresh beats after recovery — these reflect current resting rhythm
    let recovered_beats = collect_beats(&heart, 5, Duration::from_secs(8));

    let mut vitals = CardiacVitals::new();
    for beat in &recovered_beats {
        vitals.record_beat(beat.instant);
    }

    let rhythm = vitals.classify(Instant::now());
    assert!(
        rhythm != CardiacRhythm::Fibrillation && rhythm != CardiacRhythm::Asystole,
        "recovered rhythm should be regular, got {:?}",
        rhythm
    );

    heart.stop();
}

// ═══════════════════════════════════════════════════════════════════════
// BRIDGE TESTS (fibertract feature)
// ═══════════════════════════════════════════════════════════════════════

#[cfg(feature = "fibertract")]
#[test]
fn bridge_delivers_ne_to_heart() {
    let heart = CardiacPipeline::start();
    let mut bridge = AutonomicBridge::from_profiles();

    // Sustained sympathetic drive via bridge
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        bridge.set_sympathetic_drive(200);
        bridge.push_chemicals(&heart);
        std::thread::sleep(Duration::from_millis(200));
    }

    let beats = collect_beats(&heart, 3, Duration::from_secs(5));
    heart.stop();
    assert!(beats.len() >= 3, "bridge should deliver NE to accelerate heart");
}

#[cfg(feature = "fibertract")]
#[test]
fn bridge_delivers_ach_to_heart() {
    let heart = CardiacPipeline::start();
    let mut bridge = AutonomicBridge::from_profiles();

    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        bridge.set_vagal_drive(200);
        bridge.push_chemicals(&heart);
        std::thread::sleep(Duration::from_millis(200));
    }

    let beats = collect_beats(&heart, 3, Duration::from_secs(10));
    heart.stop();
    assert!(beats.len() >= 3, "bridge should deliver ACh");
}

#[cfg(feature = "fibertract")]
#[test]
fn bridge_writes_interoception() {
    let heart = CardiacPipeline::start();
    let mut bridge = AutonomicBridge::from_profiles();

    let beats = collect_beats(&heart, 1, Duration::from_secs(3));
    heart.stop();
    assert!(!beats.is_empty());

    bridge.write_interoception(&beats[0]);

    let (_timing, pressure) = bridge.read_interoception();
    assert!(
        pressure > 0,
        "interoceptive pressure should be nonzero after beat"
    );
}

#[cfg(feature = "fibertract")]
#[test]
fn bridge_ne_accelerates_vs_baseline() {
    // Baseline
    let heart1 = CardiacPipeline::start();
    let baseline = collect_beats(&heart1, 4, Duration::from_secs(6));
    let baseline_ibi = mean_ibi(&baseline);
    heart1.stop();

    // With bridge NE — sustained injection
    let heart2 = CardiacPipeline::start();
    let mut bridge = AutonomicBridge::from_profiles();
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        bridge.set_sympathetic_drive(200);
        bridge.push_chemicals(&heart2);
        std::thread::sleep(Duration::from_millis(200));
    }
    let ne_beats = collect_beats(&heart2, 4, Duration::from_secs(6));
    let ne_ibi = mean_ibi(&ne_beats);
    heart2.stop();

    assert!(baseline.len() >= 4 && ne_beats.len() >= 4);
    assert!(
        ne_ibi < baseline_ibi,
        "bridge NE should accelerate: ne_ibi={}μs < baseline={}μs",
        ne_ibi, baseline_ibi
    );
}

// ═══════════════════════════════════════════════════════════════════════
// EDGE CASES
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn immediate_stop() {
    let heart = CardiacPipeline::start();
    let snapshot = heart.stop();
    assert!(snapshot.beat_count < 100);
}

#[test]
fn drop_without_stop() {
    {
        let heart = CardiacPipeline::start();
        let _ = collect_beats(&heart, 1, Duration::from_secs(2));
    }
    std::thread::sleep(Duration::from_millis(200));
}

#[test]
fn receiver_dropped_heart_keeps_beating() {
    let heart = CardiacPipeline::start();
    std::thread::sleep(Duration::from_secs(2));
    assert!(heart.is_alive(), "heart should still be alive");
    let _snapshot = heart.stop();
}

#[test]
fn no_injections_produces_intrinsic_rhythm() {
    // No injections at all — heart has resting baselines
    let heart = CardiacPipeline::start();
    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    heart.stop();
    assert!(beats.len() >= 5, "intrinsic rhythm should produce beats");
}

#[test]
fn multiple_hearts_run_independently() {
    let heart1 = CardiacPipeline::start();
    let heart2 = CardiacPipeline::start();

    // Sustained NE on heart1 (fast)
    sustain_ne(&heart1, 200, Duration::from_millis(200), Duration::from_secs(3));
    // Sustained ACh on heart2 (slow)
    sustain_ach(&heart2, 200, Duration::from_millis(200), Duration::from_secs(3));

    let _fast_beats = drain_beats(&heart1, Duration::from_secs(3));
    let _slow_beats = drain_beats(&heart2, Duration::from_secs(5));

    let snap1 = heart1.stop();
    let snap2 = heart2.stop();

    assert!(
        snap1.beat_count > snap2.beat_count,
        "fast heart ({}) should beat more than slow heart ({})",
        snap1.beat_count, snap2.beat_count
    );
}

#[test]
fn denervated_heart_still_beats() {
    // A denervated heart = no external injections whatsoever.
    // The resting chemical baselines (ACh=30 vagal tone) provide
    // the only modulation. Heart should beat stably at intrinsic rate.
    let heart = CardiacPipeline::start();

    // Wait 5 seconds with zero injections
    let beats = collect_beats(&heart, 5, Duration::from_secs(8));
    let snapshot = heart.stop();

    assert!(beats.len() >= 5, "denervated heart must beat");
    assert!(
        snapshot.last_bpm >= 40 && snapshot.last_bpm <= 120,
        "denervated heart BPM {} should be in physiological range",
        snapshot.last_bpm
    );

    // Chemical state should be at baselines
    assert!(
        snapshot.final_ach >= 25 && snapshot.final_ach <= 35,
        "resting ACh should be near baseline 30, got {}",
        snapshot.final_ach
    );
}

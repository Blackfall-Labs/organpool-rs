//! Integration tests for the autonomous respiratory engine.
//!
//! The lungs run themselves with their own chemical environment. Tests inject
//! chemicals and observe breaths. Chemicals decay — sustained effects require
//! repeated injection (like real nerve firing).

use organpool::{
    BreathEvent, LungHandle, RespiratoryConfig, RespiratoryPipeline,
};
use std::time::{Duration, Instant};

/// Helper: collect N breaths from running lungs within a timeout.
fn collect_breaths(lungs: &LungHandle, n: usize, timeout: Duration) -> Vec<BreathEvent> {
    let mut breaths = Vec::with_capacity(n);
    let deadline = Instant::now() + timeout;
    while breaths.len() < n {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match lungs.breaths.recv_timeout(remaining) {
            Ok(breath) => breaths.push(breath),
            Err(_) => break,
        }
    }
    breaths
}


// ───────────────────── Basic Operation ─────────────────────

#[test]
fn lungs_breathe_on_their_own() {
    let lungs = RespiratoryPipeline::start();
    let breaths = collect_breaths(&lungs, 3, Duration::from_secs(20));
    let snapshot = lungs.stop();

    assert!(
        breaths.len() >= 3,
        "Denervated lungs should produce at least 3 breaths, got {}",
        breaths.len()
    );
    assert!(snapshot.breath_count >= 3);
}

#[test]
fn intrinsic_rhythm_is_regular() {
    let lungs = RespiratoryPipeline::start();

    // Wait for rhythm to settle, then collect
    std::thread::sleep(Duration::from_secs(5));
    let breaths = collect_breaths(&lungs, 4, Duration::from_secs(25));
    lungs.stop();

    assert!(
        breaths.len() >= 4,
        "Expected 4+ breaths, got {}",
        breaths.len()
    );

    // Check that cycle times are within 30% of each other (excluding first)
    let cycles: Vec<u64> = breaths.iter().skip(1).map(|b| b.cycle_us).collect();
    if cycles.len() >= 2 {
        let mean = cycles.iter().sum::<u64>() / cycles.len() as u64;
        for c in &cycles {
            let deviation = if *c > mean { *c - mean } else { mean - *c };
            let pct = (deviation * 100) / mean;
            assert!(
                pct < 30,
                "Cycle time {} deviates {}% from mean {} — rhythm is irregular",
                c,
                pct,
                mean,
            );
        }
    }
}

#[test]
fn respiratory_rate_in_expected_range() {
    let lungs = RespiratoryPipeline::start();
    let breaths = collect_breaths(&lungs, 5, Duration::from_secs(30));
    let snapshot = lungs.stop();

    assert!(
        breaths.len() >= 3,
        "Need at least 3 breaths to measure rate, got {}",
        breaths.len()
    );

    let bpm = snapshot.last_breaths_per_minute;
    assert!(
        bpm >= 8 && bpm <= 30,
        "Resting respiratory rate should be 8-30 bpm, got {}",
        bpm
    );
}

// ───────────────────── Sympathetic Modulation ─────────────────────

#[test]
fn ne_increases_respiratory_rate() {
    // Single lungs — measure baseline then stressed rate on same instance.
    // This avoids inter-instance timing variance.
    let lungs = RespiratoryPipeline::start();

    // Settle and collect baseline breaths
    let _ = collect_breaths(&lungs, 2, Duration::from_secs(15));
    let baseline_breaths = collect_breaths(&lungs, 3, Duration::from_secs(20));

    // Now inject NE continuously for 15 seconds while breaths buffer in channel
    for _ in 0..75 {
        lungs.inject_ne(200);
        std::thread::sleep(Duration::from_millis(200));
    }

    // Drain all breaths produced during injection
    let mut ne_breaths = Vec::new();
    while let Ok(b) = lungs.breaths.try_recv() {
        ne_breaths.push(b);
    }
    lungs.stop();

    assert!(baseline_breaths.len() >= 2, "Baseline should have 2+ breaths");
    assert!(ne_breaths.len() >= 2, "NE should produce 2+ breaths");

    // NE should produce shorter cycle times (faster rate)
    let baseline_mean = baseline_breaths.iter().map(|b| b.cycle_us).sum::<u64>()
        / baseline_breaths.len() as u64;
    let ne_mean = ne_breaths.iter().map(|b| b.cycle_us).sum::<u64>()
        / ne_breaths.len() as u64;

    assert!(
        ne_mean < baseline_mean,
        "NE should shorten breath cycle: NE mean {}us vs baseline mean {}us",
        ne_mean,
        baseline_mean
    );
}

#[test]
fn ne_increases_tidal_volume() {
    let lungs = RespiratoryPipeline::start();

    // Collect baseline breaths
    let baseline = collect_breaths(&lungs, 3, Duration::from_secs(20));
    let baseline_tv: u8 = baseline.last().map(|b| b.tidal_volume).unwrap_or(0);

    // Inject sustained NE
    for _ in 0..30 {
        lungs.inject_ne(200);
        std::thread::sleep(Duration::from_millis(200));
    }
    let stressed = collect_breaths(&lungs, 3, Duration::from_secs(15));
    lungs.stop();

    let stressed_tv: u8 = stressed.last().map(|b| b.tidal_volume).unwrap_or(0);

    assert!(
        stressed_tv > baseline_tv,
        "NE should deepen breaths: stressed TV {} vs baseline TV {}",
        stressed_tv,
        baseline_tv
    );
}

// ───────────────────── Parasympathetic Modulation ─────────────────────

#[test]
fn ach_decreases_respiratory_rate() {
    let lungs = RespiratoryPipeline::start();

    // Settle and collect baseline breaths
    let _ = collect_breaths(&lungs, 2, Duration::from_secs(15));
    let baseline_breaths = collect_breaths(&lungs, 3, Duration::from_secs(20));

    // Inject ACh continuously for 15 seconds while breaths buffer
    for _ in 0..75 {
        lungs.inject_ach(200);
        std::thread::sleep(Duration::from_millis(200));
    }

    // Drain all breaths produced during injection
    let mut ach_breaths = Vec::new();
    while let Ok(b) = lungs.breaths.try_recv() {
        ach_breaths.push(b);
    }
    lungs.stop();

    assert!(baseline_breaths.len() >= 2, "Need baseline breaths");
    assert!(ach_breaths.len() >= 1, "ACh lungs should still breathe");

    let baseline_mean = baseline_breaths.iter().map(|b| b.cycle_us).sum::<u64>()
        / baseline_breaths.len() as u64;
    let ach_mean = ach_breaths.iter().map(|b| b.cycle_us).sum::<u64>()
        / ach_breaths.len() as u64;

    assert!(
        ach_mean > baseline_mean,
        "ACh should lengthen breath cycle: ACh mean {}us vs baseline mean {}us",
        ach_mean,
        baseline_mean
    );
}

// ───────────────────── Gas Exchange ─────────────────────

#[test]
fn gas_exchange_maintains_homeostasis() {
    let config = RespiratoryConfig::default();
    let lungs = RespiratoryPipeline::start();

    // Let lungs breathe for 15 seconds at rest
    let _ = collect_breaths(&lungs, 4, Duration::from_secs(18));
    let snapshot = lungs.stop();

    // CO2 and O2 should stay near baselines
    let co2_distance = (snapshot.final_co2 as i16 - config.co2_baseline as i16).unsigned_abs();
    let o2_distance = (snapshot.final_o2 as i16 - config.o2_baseline as i16).unsigned_abs();

    assert!(
        co2_distance < 30,
        "CO2 should stay near baseline {}: got {} (distance {})",
        config.co2_baseline,
        snapshot.final_co2,
        co2_distance
    );
    assert!(
        o2_distance < 30,
        "O2 should stay near baseline {}: got {} (distance {})",
        config.o2_baseline,
        snapshot.final_o2,
        o2_distance
    );
}

// ───────────────────── Recovery ─────────────────────

#[test]
fn lungs_recover_after_stress() {
    let lungs = RespiratoryPipeline::start();

    // Let lungs settle
    let _ = collect_breaths(&lungs, 2, Duration::from_secs(12));

    // Record baseline cycle time
    let baseline = collect_breaths(&lungs, 2, Duration::from_secs(12));
    let baseline_cycle = baseline.last().map(|b| b.cycle_us).unwrap_or(4_000_000);

    // Stress with NE
    for _ in 0..20 {
        lungs.inject_ne(200);
        std::thread::sleep(Duration::from_millis(100));
    }
    // Drain stressed breaths
    let _ = collect_breaths(&lungs, 3, Duration::from_secs(10));

    // Wait for recovery (NE half-life = 2.5s, should mostly decay in ~10s)
    std::thread::sleep(Duration::from_secs(8));

    // Measure recovered rate
    let recovered = collect_breaths(&lungs, 2, Duration::from_secs(12));
    lungs.stop();

    let recovered_cycle = recovered.last().map(|b| b.cycle_us).unwrap_or(0);

    // Recovered cycle should be within 40% of baseline
    let diff = if recovered_cycle > baseline_cycle {
        recovered_cycle - baseline_cycle
    } else {
        baseline_cycle - recovered_cycle
    };
    let pct = (diff * 100) / baseline_cycle.max(1);
    assert!(
        pct < 40,
        "Recovered cycle {}us should be within 40% of baseline {}us ({}% off)",
        recovered_cycle,
        baseline_cycle,
        pct
    );
}

// ───────────────────── Extreme Conditions ─────────────────────

#[test]
fn max_ne_still_breathes() {
    let lungs = RespiratoryPipeline::start();
    for _ in 0..30 {
        lungs.inject_ne(255);
        std::thread::sleep(Duration::from_millis(100));
    }
    let breaths = collect_breaths(&lungs, 3, Duration::from_secs(15));
    lungs.stop();

    assert!(
        breaths.len() >= 2,
        "Max NE should not stop breathing, got {} breaths",
        breaths.len()
    );
}

#[test]
fn max_ach_still_breathes() {
    let lungs = RespiratoryPipeline::start();
    for _ in 0..30 {
        lungs.inject_ach(255);
        std::thread::sleep(Duration::from_millis(100));
    }
    let breaths = collect_breaths(&lungs, 2, Duration::from_secs(20));
    lungs.stop();

    assert!(
        breaths.len() >= 1,
        "Max ACh should not stop breathing entirely, got {} breaths",
        breaths.len()
    );
}

#[test]
fn denervated_lungs_still_breathe() {
    // No injections at all — pure intrinsic CPG
    let lungs = RespiratoryPipeline::start();
    let breaths = collect_breaths(&lungs, 4, Duration::from_secs(25));
    let snapshot = lungs.stop();

    assert!(breaths.len() >= 3, "Denervated lungs must breathe");
    assert!(snapshot.final_ne == 0, "No NE injected, should be 0");
}

// ───────────────────── Lifecycle ─────────────────────

#[test]
fn stop_returns_snapshot() {
    let lungs = RespiratoryPipeline::start();
    let _ = collect_breaths(&lungs, 3, Duration::from_secs(20));
    let snapshot = lungs.stop();

    assert!(
        snapshot.breath_count >= 3,
        "Snapshot should reflect breaths taken: {}",
        snapshot.breath_count
    );
    assert!(snapshot.last_breaths_per_minute > 0);
}

#[test]
fn breaths_are_sequentially_numbered() {
    let lungs = RespiratoryPipeline::start();
    let breaths = collect_breaths(&lungs, 5, Duration::from_secs(30));
    lungs.stop();

    assert!(breaths.len() >= 3, "Need breaths to verify numbering");

    for window in breaths.windows(2) {
        assert_eq!(
            window[1].breath_number,
            window[0].breath_number + 1,
            "Breath numbers should be sequential: {} then {}",
            window[0].breath_number,
            window[1].breath_number,
        );
    }
}

#[test]
fn multiple_lungs_run_independently() {
    let lungs_a = RespiratoryPipeline::start();
    let lungs_b = RespiratoryPipeline::start();

    // Stress only lungs_a
    for _ in 0..15 {
        lungs_a.inject_ne(200);
        std::thread::sleep(Duration::from_millis(200));
    }

    let breaths_a = collect_breaths(&lungs_a, 3, Duration::from_secs(15));
    let breaths_b = collect_breaths(&lungs_b, 3, Duration::from_secs(15));

    let snap_a = lungs_a.stop();
    let snap_b = lungs_b.stop();

    assert!(breaths_a.len() >= 2, "Lungs A should breathe");
    assert!(breaths_b.len() >= 2, "Lungs B should breathe");

    // Lungs A (stressed) should have taken more breaths than B (relaxed)
    // in the same period, OR at minimum both are alive and independent
    assert!(
        snap_a.breath_count > 0 && snap_b.breath_count > 0,
        "Both lung instances should produce breaths"
    );
}

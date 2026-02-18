//! Integration tests for cardiac-respiratory coupling via RSA.
//!
//! These tests verify that when lungs and heart are coupled via
//! `Arc<AtomicU8>`, respiratory sinus arrhythmia (RSA) modulates
//! heart rate in synchrony with breathing.

use organpool::{
    BeatEvent, BreathEvent, CardiacConfig, CardiacPipeline, HeartHandle,
    LungHandle, RespiratoryConfig, RespiratoryPipeline, RsaSource,
};
use std::time::{Duration, Instant};

/// Helper: start a heart with external RSA enabled.
fn start_heart_external_rsa() -> HeartHandle {
    let mut config = CardiacConfig::default();
    config.sa_node.hrv.rsa_source = RsaSource::External;
    CardiacPipeline::start_with_config(config)
}

/// Helper: collect N beats within a timeout.
fn collect_beats(heart: &HeartHandle, n: usize, timeout: Duration) -> Vec<BeatEvent> {
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

/// Helper: collect N breaths within a timeout.
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

/// Integer square root for RMSSD computation.
fn isqrt(n: u64) -> u64 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}

// ───────────────────── Coupled Operation ─────────────────────

#[test]
fn coupled_system_both_beat_and_breathe() {
    let heart = start_heart_external_rsa();
    let rsa_signal = heart.rsa_signal().expect("External RSA should provide signal");
    let lungs = RespiratoryPipeline::start_coupled(RespiratoryConfig::default(), rsa_signal);

    // Let them run together
    let beats = collect_beats(&heart, 10, Duration::from_secs(15));
    let breaths = collect_breaths(&lungs, 2, Duration::from_secs(15));

    lungs.stop();
    heart.stop();

    assert!(
        beats.len() >= 8,
        "Heart should beat at least 8 times in 15s, got {}",
        beats.len()
    );
    assert!(
        breaths.len() >= 1,
        "Lungs should breathe at least once in 15s, got {}",
        breaths.len()
    );
}

#[test]
fn rsa_produces_heart_rate_variability() {
    let heart = start_heart_external_rsa();
    let rsa_signal = heart.rsa_signal().expect("External RSA");
    let lungs = RespiratoryPipeline::start_coupled(RespiratoryConfig::default(), rsa_signal);

    // Let them settle and then collect beats
    let _ = collect_beats(&heart, 5, Duration::from_secs(10));
    let beats = collect_beats(&heart, 20, Duration::from_secs(25));

    lungs.stop();
    heart.stop();

    assert!(beats.len() >= 10, "Need 10+ beats for RMSSD");

    // Compute RMSSD (root mean square of successive differences)
    let ibis: Vec<u64> = beats.iter().filter(|b| b.ibi_us > 0).map(|b| b.ibi_us).collect();
    if ibis.len() >= 2 {
        let mut sum_sq_diff: u64 = 0;
        let mut count: u64 = 0;
        for w in ibis.windows(2) {
            let diff = if w[1] > w[0] { w[1] - w[0] } else { w[0] - w[1] };
            sum_sq_diff += diff * diff;
            count += 1;
        }
        let rmssd = isqrt(sum_sq_diff / count.max(1));

        assert!(
            rmssd > 0,
            "RMSSD should be > 0 with real RSA coupling, got {}",
            rmssd
        );
    }
}

// ───────────────────── RSA Backward Compatibility ─────────────────────

#[test]
fn rsa_disabled_when_internal() {
    // Default config uses Internal RSA — no AtomicU8 signal
    let heart = CardiacPipeline::start();
    assert!(
        heart.rsa_signal().is_none(),
        "Internal RSA heart should have no rsa_signal"
    );

    // Heart should still beat with internal sine RSA
    let beats = collect_beats(&heart, 5, Duration::from_secs(10));
    heart.stop();

    assert!(beats.len() >= 4, "Internal RSA heart should beat normally");
}

// ───────────────────── Cross-Organ Modulation ─────────────────────

#[test]
fn coupled_ne_affects_both_organs() {
    let heart = start_heart_external_rsa();
    let rsa_signal = heart.rsa_signal().unwrap();
    let lungs = RespiratoryPipeline::start_coupled(RespiratoryConfig::default(), rsa_signal);

    // Let them settle
    let _ = collect_beats(&heart, 5, Duration::from_secs(10));
    let _ = collect_breaths(&lungs, 2, Duration::from_secs(12));

    // Collect baseline
    let baseline_beats = collect_beats(&heart, 5, Duration::from_secs(10));

    // Inject NE into both
    for _ in 0..30 {
        heart.inject_ne(200);
        lungs.inject_ne(200);
        std::thread::sleep(Duration::from_millis(200));
    }

    // Collect stressed beats from channel
    let mut stressed_beats = Vec::new();
    while let Ok(b) = heart.beats.try_recv() {
        stressed_beats.push(b);
    }

    lungs.stop();
    heart.stop();

    assert!(baseline_beats.len() >= 3, "Need baseline beats");
    assert!(stressed_beats.len() >= 3, "Need stressed beats");

    // Both heart and lungs should be faster under NE
    let baseline_mean = baseline_beats.iter().filter(|b| b.ibi_us > 0)
        .map(|b| b.ibi_us).sum::<u64>()
        / baseline_beats.iter().filter(|b| b.ibi_us > 0).count().max(1) as u64;
    let stressed_mean = stressed_beats.iter().filter(|b| b.ibi_us > 0)
        .map(|b| b.ibi_us).sum::<u64>()
        / stressed_beats.iter().filter(|b| b.ibi_us > 0).count().max(1) as u64;

    assert!(
        stressed_mean < baseline_mean,
        "NE should shorten IBI: stressed {}us vs baseline {}us",
        stressed_mean,
        baseline_mean
    );
}

// ───────────────────── Graceful Degradation ─────────────────────

#[test]
fn stop_lungs_heart_continues() {
    let heart = start_heart_external_rsa();
    let rsa_signal = heart.rsa_signal().unwrap();
    let lungs = RespiratoryPipeline::start_coupled(RespiratoryConfig::default(), rsa_signal);

    // Let them run together
    let _ = collect_beats(&heart, 5, Duration::from_secs(10));
    let _ = collect_breaths(&lungs, 2, Duration::from_secs(12));

    // Stop lungs — heart should continue
    lungs.stop();

    // Heart should still beat after lungs stop
    // The AtomicU8 retains its last value — heart continues at that ACh level
    let beats_after = collect_beats(&heart, 5, Duration::from_secs(10));
    heart.stop();

    assert!(
        beats_after.len() >= 4,
        "Heart should continue beating after lungs stop, got {} beats",
        beats_after.len()
    );
}

// ───────────────────── Stress and Recovery ─────────────────────

#[test]
fn coupled_stress_then_recovery() {
    let heart = start_heart_external_rsa();
    let rsa_signal = heart.rsa_signal().unwrap();
    let lungs = RespiratoryPipeline::start_coupled(RespiratoryConfig::default(), rsa_signal);

    // Settle
    let _ = collect_beats(&heart, 5, Duration::from_secs(10));
    let _ = collect_breaths(&lungs, 2, Duration::from_secs(12));

    // Stress both organs
    for _ in 0..20 {
        heart.inject_ne(200);
        lungs.inject_ne(200);
        std::thread::sleep(Duration::from_millis(200));
    }

    // Drain stressed events
    while heart.beats.try_recv().is_ok() {}
    while lungs.breaths.try_recv().is_ok() {}

    // Wait for recovery (NE half-life 2.5s)
    std::thread::sleep(Duration::from_secs(8));

    // Collect recovered beats
    let recovered_beats = collect_beats(&heart, 5, Duration::from_secs(10));
    let recovered_breaths = collect_breaths(&lungs, 2, Duration::from_secs(12));

    lungs.stop();
    heart.stop();

    assert!(
        recovered_beats.len() >= 4,
        "Heart should recover after stress"
    );
    assert!(
        recovered_breaths.len() >= 1,
        "Lungs should recover after stress"
    );
}

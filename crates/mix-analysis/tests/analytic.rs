//! Features vs analytic signals (sines, white/pink noise, click trains, EBU Tech 3341/3342 cases).

mod common;

use common::*;
use mix_analysis::signals::{click_train, pink, sine, white};
use mix_core::DB_FLOOR;

#[test]
fn ebu_3341_stereo_sine_minus_23_dbfs_reads_minus_23_lufs() {
    // EBU Tech 3341 case 1: 1 kHz sine, −23 dBFS peak in both channels → M = S = I = −23.0 LUFS ± 0.1.
    let x = sine(1000.0, db_amp(-23.0), secs(10.0), SR, 0.0);
    let a = offline_stereo(&x, &x);
    let l = &a.session.loudness;
    close(l.integrated_lufs.unwrap(), -23.0, 0.1, "integrated");
    close(l.momentary_lufs, -23.0, 0.1, "momentary");
    close(l.short_term_lufs, -23.0, 0.1, "short-term");
    close(l.true_peak_dbtp, -23.0, 0.1, "true peak");
    close(l.sample_peak_dbfs, -23.0, 0.05, "sample peak");
    close(l.rms_dbfs, -26.02, 0.05, "rms (mean square)");
    close(l.crest_db, 3.01, 0.1, "crest");
    close(l.plr_db.unwrap(), 0.0, 0.15, "plr");
    close(l.psr_db, 0.0, 0.15, "psr");
}

#[test]
fn sine_minus_20_dbfs_stereo_and_single_channel() {
    // Both channels at −20 dBFS: −20.0 LUFS. Only one channel (the other silent): −23.0 LUFS
    // (BS.1770 sums channel energies, so dropping one channel loses 3.01 dB).
    let x = sine(1000.0, db_amp(-20.0), secs(6.0), SR, 0.0);
    let silent = vec![0.0; x.len()];
    close(
        offline_stereo(&x, &x)
            .session
            .loudness
            .integrated_lufs
            .unwrap(),
        -20.0,
        0.1,
        "stereo",
    );
    close(
        offline_stereo(&x, &silent)
            .session
            .loudness
            .integrated_lufs
            .unwrap(),
        -23.0,
        0.1,
        "left only",
    );
    // A mono tap carries the signal once (G = 1): also −23.0.
    let m = offline_mono(&x);
    close(
        m.session.loudness.integrated_lufs.unwrap(),
        -23.0,
        0.1,
        "mono tap",
    );
    assert!(m.session.stereo.is_none() && m.session.spectrum.erb_side_rel_db.is_empty());
}

#[test]
fn ebu_3342_case1_lra_is_10_lu() {
    // EBU Tech 3342 case 1: 20 s at −20 dBFS then 20 s at −30 dBFS → LRA 10 ± 1 LU.
    let mut x = sine(1000.0, db_amp(-20.0), secs(20.0), SR, 0.0);
    x.extend(sine(1000.0, db_amp(-30.0), secs(20.0), SR, 0.0));
    let a = offline_stereo(&x, &x);
    close(a.session.loudness.lra_lu.unwrap(), 10.0, 1.0, "LRA");
}

#[test]
fn true_peak_catches_inter_sample_peak() {
    // fs/4 sine at 45° phase: samples sit at ±0.707·A, the true peak is A (−6.02 dBFS for A = 0.5).
    let x = sine(
        SR as f32 / 4.0,
        0.5,
        secs(4.0),
        SR,
        std::f32::consts::FRAC_PI_4,
    );
    let l = offline_stereo(&x, &x).session.loudness;
    close(l.sample_peak_dbfs, -9.03, 0.05, "sample peak");
    close(l.true_peak_dbtp, -6.02, 0.5, "true peak");
}

#[test]
fn sine_lands_in_its_bands() {
    let x = sine(1000.0, db_amp(-20.0), secs(5.0), SR, 0.0);
    let a = offline_stereo(&x, &x);
    let s = &a.session.spectrum;
    // Mean-square power of a −20 dBFS sine is −23.01 dB, all in the 1 kHz third octave (index 17).
    close(s.third_oct_mid_db[17], -23.01, 0.1, "1 kHz third octave");
    assert!(
        s.third_oct_mid_db[15] < -65.0 && s.third_oct_mid_db[19] < -65.0,
        "{:?}",
        s.third_oct_mid_db
    );
    // rel_db: −23.01 dB band power vs −20 LUFS window loudness → ≈ −3 in the 1 kHz ERB band.
    let peak = s.erb_mid_rel_db.iter().cloned().fold(f32::MIN, f32::max);
    close(peak, -3.0, 0.2, "peak ERB rel_db");
    close(s.centroid_hz, 1000.0, 100.0, "centroid");
    assert!(s.flatness < 0.05, "flatness {}", s.flatness);
    // Identical channels: no side.
    assert!(s.erb_side_rel_db.iter().all(|v| *v <= DB_FLOOR + 1.0));
}

#[test]
fn noise_tilt_and_flatness() {
    let p = pink(secs(10.0), 0.1, 1);
    let p2 = pink(secs(10.0), 0.1, 2);
    let pa = offline_stereo(&p, &p2);
    close(pa.session.spectrum.tilt_db_per_oct, -3.0, 0.4, "pink tilt");
    let w = white(secs(10.0), 0.1, 3);
    let w2 = white(secs(10.0), 0.1, 4);
    let wa = offline_stereo(&w, &w2);
    close(wa.session.spectrum.tilt_db_per_oct, 0.0, 0.3, "white tilt");
    assert!(
        wa.session.spectrum.flatness > 0.9,
        "white flatness {}",
        wa.session.spectrum.flatness
    );
    assert!(pa.session.spectrum.flatness < wa.session.spectrum.flatness);
    assert!(pa.session.spectrum.centroid_hz < wa.session.spectrum.centroid_hz);
    assert!(
        wa.session.spectrum.rolloff85_hz > 14000.0,
        "{}",
        wa.session.spectrum.rolloff85_hz
    );
}

#[test]
fn stereo_image_measures() {
    let n = secs(5.0);
    let a = white(n, 0.1, 5);
    let b = white(n, 0.1, 6);
    let st = |l: &[f32], r: &[f32]| offline_stereo(l, r).session.stereo.unwrap();

    let same = st(&a, &a);
    close(same.correlation, 1.0, 1e-3, "identical corr");
    assert!(same.width_db <= DB_FLOOR + 1.0);

    let inv: Vec<f32> = a.iter().map(|v| -v).collect();
    let anti = st(&a, &inv);
    close(anti.correlation, -1.0, 1e-3, "inverted corr");
    assert!(anti.width_db > 60.0);

    let ind = st(&a, &b);
    close(ind.correlation, 0.0, 0.05, "independent corr");
    close(ind.width_db, 0.0, 0.5, "independent width");
    for c in ind.band_correlation {
        close(c, 0.0, 0.15, "band corr");
    }

    let half: Vec<f32> = a.iter().map(|v| v * 0.5).collect();
    close(st(&a, &half).balance_db, 6.02, 0.1, "balance");

    // In-phase bass is mono at the low end even with wide independent highs on top.
    let bass = sine(60.0, 0.3, n, SR, 0.0);
    let l: Vec<f32> = bass.iter().zip(&a).map(|(x, y)| x + y * 0.1).collect();
    let r: Vec<f32> = bass.iter().zip(&b).map(|(x, y)| x + y * 0.1).collect();
    let s = st(&l, &r);
    assert!(
        s.low_mono_correlation > 0.99,
        "low corr {}",
        s.low_mono_correlation
    );
    assert!(
        s.band_correlation[5] < 0.2,
        "air corr {}",
        s.band_correlation[5]
    );
}

#[test]
fn silence_is_gated_out_of_spectra() {
    let n = secs(6.0);
    let p = pink(n, 0.1, 7);
    let p2 = pink(n, 0.1, 8);
    let mut l = p.clone();
    let mut r = p2.clone();
    l.extend(vec![0.0; n]);
    r.extend(vec![0.0; n]);
    let gated = offline_stereo(&l, &r).session;
    let full = offline_stereo(&p, &p2).session;
    close(gated.quality.gated_fraction, 0.5, 0.06, "gated fraction");
    for b in 2..38 {
        close(
            gated.spectrum.erb_mid_rel_db[b],
            full.spectrum.erb_mid_rel_db[b],
            0.5,
            "gated band",
        );
    }
    close(
        gated.spectrum.tilt_db_per_oct,
        full.spectrum.tilt_db_per_oct,
        0.2,
        "gated tilt",
    );
}

#[test]
fn onsets_track_click_rate_and_ignore_steady_noise() {
    let n = secs(12.0);
    let c = click_train(4.0, 0.5, n, SR, 9);
    let a = offline_stereo(&c, &c);
    close(
        a.session.transients.onset_rate_hz,
        4.0,
        0.4,
        "click onset rate",
    );
    let last = a.short_term.last().unwrap();
    close(
        last.transients.onset_rate_hz,
        4.0,
        0.7,
        "short-term onset rate",
    );
    assert!(
        a.session.transients.band_onset_rate_hz[2] > 3.0,
        "{:?}",
        a.session.transients.band_onset_rate_hz
    );
    // Clicks: big attack ratio and crest in the mid/high bands.
    for b in 3..6 {
        assert!(
            a.session.dynamics.attack_ratio_db[b] > 3.0,
            "attack {:?}",
            a.session.dynamics.attack_ratio_db
        );
        assert!(
            a.session.dynamics.crest_db[b] > 6.0,
            "crest {:?}",
            a.session.dynamics.crest_db
        );
    }

    let p = pink(n, 0.1, 10);
    let p2 = pink(n, 0.1, 11);
    let s = offline_stereo(&p, &p2).session;
    assert!(
        s.transients.onset_rate_hz < 0.5,
        "pink onset rate {}",
        s.transients.onset_rate_hz
    );
    for b in 3..6 {
        assert!(
            s.dynamics.crest_db[b] < 6.0,
            "pink crest {:?}",
            s.dynamics.crest_db
        );
    }
}

#[test]
fn band_range_follows_level_steps() {
    // Pink noise alternating between two levels 10 dB apart every 6 s: per-band range ≈ 10 dB.
    let mut l = Vec::new();
    let mut r = Vec::new();
    for k in 0..6 {
        let rms = if k % 2 == 0 { 0.1 } else { 0.0316 };
        l.extend(pink(secs(6.0), rms, 20 + k));
        r.extend(pink(secs(6.0), rms, 40 + k));
    }
    let a = offline_stereo(&l, &r);
    for b in 1..6 {
        close(
            a.session.dynamics.range_db[b],
            10.0,
            1.5,
            "session band range",
        );
        close(
            a.short_term.last().unwrap().dynamics.range_db[b],
            10.0,
            1.5,
            "history band range",
        );
    }
}

#[test]
fn snapshots_are_finite_and_json_safe() {
    let a = offline_stereo(&vec![0.0; secs(4.0)], &vec![0.0; secs(4.0)]);
    let j = serde_json::to_string(&a.session).unwrap();
    assert!(
        !j.contains("null,") || j.contains("\"integrated_lufs\""),
        "{j}"
    );
    for f in a.short_term.iter().chain([&a.session]) {
        let v = serde_json::to_value(f).unwrap();
        fn finite(v: &serde_json::Value) -> bool {
            match v {
                serde_json::Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
                serde_json::Value::Array(a) => a.iter().all(finite),
                serde_json::Value::Object(o) => o.values().all(finite),
                _ => true,
            }
        }
        assert!(finite(&v), "{v}");
        f.validate().unwrap();
    }
    assert_eq!(a.session.quality.gated_fraction, 1.0);
}

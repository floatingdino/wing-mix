//! Streaming == offline, window semantics, and profile building end to end.

mod common;

use common::*;
use mix_analysis::signals::{click_train, interleave, pink, sine, Rng};
use mix_analysis::{
    analyse_buffer, AnalyserConfig, OfflineOptions, ProfileBuilder, StreamingAnalyser,
};
use mix_core::{Features, ProfileSource, SectionRef, TargetProfile, Window};

/// Test programme: pink noise bed with clicks, a level drop and a bass line, slightly decorrelated channels.
fn programme(seconds: f32, seed: u64) -> Vec<f32> {
    let n = secs(seconds);
    let pl = pink(n, 0.08, seed);
    let pr = pink(n, 0.08, seed + 1);
    let c = click_train(3.0, 0.4, n, SR, seed + 2);
    let bass = sine(55.0, 0.2, n, SR, 0.0);
    let l: Vec<f32> = (0..n)
        .map(|i| {
            let g = if (i / secs(4.0)) % 2 == 1 { 0.4 } else { 1.0 };
            g * (pl[i] + c[i] + bass[i])
        })
        .collect();
    let r: Vec<f32> = (0..n)
        .map(|i| {
            let g = if (i / secs(4.0)) % 2 == 1 { 0.4 } else { 1.0 };
            g * (pr[i] + 0.8 * c[i] + bass[i])
        })
        .collect();
    interleave(&l, &r)
}

/// Streams `x` in random block sizes, snapshotting ShortTerm at every 100 ms boundary; returns the snapshots and
/// the final Session snapshot.
fn stream(x: &[f32], seed: u64) -> (Vec<Features>, Features) {
    let mut an = StreamingAnalyser::new(AnalyserConfig::new(SR, 2)).unwrap();
    let step = 4800 * 2;
    let mut rng = Rng::new(seed);
    let mut snap = an.new_features("main", Window::ShortTerm);
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < x.len() {
        let next_boundary = (pos / step + 1) * step;
        let block = 2 * (1 + (rng.next_u64() % 3000) as usize);
        let end = (pos + block).min(next_boundary).min(x.len());
        an.push(&x[pos..end]);
        pos = end;
        if pos % step == 0 {
            an.snapshot_into(&mut snap);
            out.push(snap.clone());
        }
    }
    let mut session = an.new_features("main", Window::Session);
    an.snapshot_into(&mut session);
    (out, session)
}

fn check_equal(seconds: f32) {
    let x = programme(seconds, 1);
    let opts = OfflineOptions {
        novelty: false,
        ..OfflineOptions::default()
    };
    let off = analyse_buffer(&x, &AnalyserConfig::new(SR, 2), &opts).unwrap();
    let (snaps, session) = stream(&x, 99);
    assert_eq!(snaps.len(), off.short_term.len());
    // Frames are cut at fixed sample positions and ebur128 is fed in the same hop-aligned pieces, so the results
    // are identical, not merely close.
    for (a, b) in snaps.iter().zip(&off.short_term) {
        assert_eq!(a, b, "snapshot at {} s differs", a.t_end_s);
    }
    assert_eq!(session, off.session);
}

#[test]
fn streaming_equals_offline() {
    check_equal(12.0);
}

#[test]
#[ignore = "slow: run with make check-full"]
fn streaming_equals_offline_long() {
    check_equal(180.0);
}

#[test]
fn windows_report_their_spans_and_long_converges() {
    let x = programme(30.0, 3);
    let mut an = StreamingAnalyser::new(AnalyserConfig::new(SR, 2)).unwrap();
    an.push(&x);
    let mut f = |w: Window| {
        let mut out = an.new_features("main", w);
        an.snapshot_into(&mut out);
        out
    };
    let m = f(Window::Momentary);
    let s = f(Window::ShortTerm);
    let l = f(Window::Long { tau_s: 0.0 });
    let ses = f(Window::Session);
    close(
        (m.t_end_s - m.t_start_s) as f32,
        0.4,
        0.03,
        "momentary span",
    );
    close(
        (s.t_end_s - s.t_start_s) as f32,
        3.0,
        0.03,
        "short-term span",
    );
    assert_eq!(l.window, Window::Long { tau_s: 10.0 });
    assert_eq!(ses.t_start_s, 0.0);
    assert!(m.loudness.integrated_lufs.is_none() && ses.loudness.integrated_lufs.is_some());
    assert!(ses.quality.frames > s.quality.frames && s.quality.frames > m.quality.frames);
    // The programme is stationary on 8 s scales; Long and Session spectra agree within a dB in the mids.
    for b in 10..35 {
        close(
            l.spectrum.erb_mid_rel_db[b],
            ses.spectrum.erb_mid_rel_db[b],
            1.5,
            "long vs session",
        );
    }
}

#[test]
fn session_reset_restarts_integrated() {
    let mut an = StreamingAnalyser::new(AnalyserConfig::new(SR, 2)).unwrap();
    let loud = sine(1000.0, db_amp(-10.0), secs(5.0), SR, 0.0);
    let quiet = sine(1000.0, db_amp(-30.0), secs(5.0), SR, 0.0);
    an.push(&interleave(&loud, &loud));
    an.reset_session();
    an.push(&interleave(&quiet, &quiet));
    let mut ses = an.new_features("main", Window::Session);
    an.snapshot_into(&mut ses);
    close(
        ses.loudness.integrated_lufs.unwrap(),
        -30.0,
        0.1,
        "integrated after reset",
    );
    close(ses.t_start_s as f32, 5.0, 0.03, "session start");
    an.note_dropouts(3);
    an.snapshot_into(&mut ses);
    assert_eq!(ses.quality.dropouts, 3);
}

#[test]
fn profile_from_offline_analysis() {
    let x = programme(24.0, 5);
    let a = analyse_buffer(&x, &AnalyserConfig::new(SR, 2), &OfflineOptions::default()).unwrap();
    assert!(a.novelty.is_some());
    let pb = ProfileBuilder::new(&a);
    let sections = [
        SectionRef {
            index: 0,
            label: "a".into(),
            start_s: 0.0,
            end_s: 12.0,
        },
        SectionRef {
            index: 1,
            label: "b".into(),
            start_s: 12.0,
            end_s: 24.0,
        },
    ];
    let src = ProfileSource::Reference {
        file_name: "programme.wav".into(),
        blake3: "test".into(),
        duration_s: a.duration_s,
        sample_rate_in: SR,
    };
    let p = pb.build(src, &sections, None);
    p.validate().unwrap();
    assert_eq!(p.analysis.version, mix_analysis::ANALYSIS_VERSION);
    assert_eq!(p.sections.len(), 2);
    // Loud/quiet alternation every 4 s → short-term loudness spread of several LU around integrated.
    let st = p.loudness.short_term_rel_lu;
    assert!(st.p90 - st.p10 > 3.0, "{st:?}");
    assert!(p.tonal.tolerance_db.iter().all(|t| *t >= 1.0));
    assert!(p
        .tonal
        .erb_mid_rel_db
        .iter()
        .all(|s| s.p10 <= s.p50 && s.p50 <= s.p90));
    close(p.transients.onset_rate_hz.p50, 3.0, 1.0, "onset p50");
    assert!(
        p.stereo.low_mono_min_correlation > 0.9,
        "{}",
        p.stereo.low_mono_min_correlation
    );
    // The two halves of a stationary programme differ little.
    for s in &p.sections {
        assert!(s.erb_mid_delta_db[20].abs() < 2.0, "{s:?}");
    }
    // Section focus narrows the windows used.
    let focused = pb.build(
        ProfileSource::Genre {
            name: "test".into(),
        },
        &[],
        Some(&sections[1]),
    );
    assert_eq!(focused.section.as_ref().unwrap().label, "b");
    let json = serde_json::to_string(&p).unwrap();
    let back: TargetProfile = serde_json::from_str(&json).unwrap();
    assert_eq!(back, p);
}

#[test]
fn novelty_finds_section_changes() {
    // 20 s pink noise, 20 s chord, 20 s pink noise → boundaries near 20 s and 40 s.
    let n = secs(20.0);
    let mut x = pink(n, 0.1, 30);
    let chord: Vec<f32> = sine(220.0, 0.1, n, SR, 0.0)
        .iter()
        .zip(sine(277.18, 0.1, n, SR, 0.0))
        .zip(sine(329.63, 0.1, n, SR, 0.0))
        .map(|((a, b), c)| a + b + c)
        .collect();
    x.extend(chord);
    x.extend(pink(n, 0.1, 31));
    let a = analyse_buffer(&x, &AnalyserConfig::new(SR, 1), &OfflineOptions::default()).unwrap();
    let nv = a.novelty.unwrap();
    assert_eq!(nv.boundaries_s.len(), 2, "{:?}", nv.boundaries_s);
    close(nv.boundaries_s[0] as f32, 20.0, 1.5, "first boundary");
    close(nv.boundaries_s[1] as f32, 40.0, 1.5, "second boundary");
}

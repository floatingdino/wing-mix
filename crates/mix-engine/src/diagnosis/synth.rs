//! Synthetic profiles and features for tests (here, the planner M032 and the controller M033).
//!
//! [`profile`] / [`profile_with_stems`] build a plausible TargetProfile from smooth curves; [`features_matching`]
//! and [`strips_matching`] build Features that sit exactly on its p50 values (so diagnosis reports nothing), which
//! tests then perturb by known amounts.

use std::collections::BTreeMap;

use mix_core::bands::{BandLayout, N_BAND3, N_BROAD, N_ERB};
use mix_core::features::{Features, Stat, Stereo, Window};
use mix_core::ids::StripId;
use mix_core::profile::{
    AnalysisParams, DynamicsTarget, LoudnessTarget, MatchWeights, ProfileSource, StemKind,
    StemProfile, StemTargets, StereoTarget, TargetProfile, TonalTarget, TransientTarget,
    PROFILE_SCHEMA_VERSION,
};
use mix_knowledge::KnowledgeBase;

use super::StripTap;

/// Main-tap loudness the matching features use (LUFS).
pub const MAIN_LUFS: f32 = -14.0;

fn stat(p50: f32, half: f32) -> Stat {
    Stat {
        p10: p50 - half,
        p50,
        p90: p50 + half,
    }
}

/// Mix curve: −3 dB/oct around −12 dB at 1 kHz, rolled off below 40 Hz and above 16 kHz.
fn mix_curve(f: f32) -> f32 {
    let mut v = -12.0 - 3.0 * (f / 1000.0).log2();
    if f < 40.0 {
        v -= 12.0 * (40.0 / f).log2();
    }
    if f > 16000.0 {
        v -= 12.0 * (f / 16000.0).log2();
    }
    v
}

/// A log-frequency bump: `peak − k·log2(f/centre)²`, floored at −60.
fn bump(f: f32, centre: f32, peak: f32, k: f32) -> f32 {
    (peak - k * (f / centre).log2().powi(2)).max(-60.0)
}

/// Whole-song reference-like profile without stems.
pub fn profile() -> TargetProfile {
    let erb = BandLayout::Erb40.centres_hz();
    let third = BandLayout::ThirdOctave31.centres_hz();
    TargetProfile {
        schema_version: PROFILE_SCHEMA_VERSION,
        source: ProfileSource::Reference {
            file_name: "synthetic.wav".into(),
            blake3: "00".into(),
            duration_s: 180.0,
            sample_rate_in: 48000,
        },
        analysis: AnalysisParams {
            sample_rate: 48000,
            fft_size: 4096,
            hop: 1024,
            version: 1,
        },
        section: None,
        loudness: LoudnessTarget {
            integrated_lufs: -9.0,
            lra_lu: 5.0,
            plr_db: 9.0,
            psr_db: stat(8.0, 1.0),
            short_term_rel_lu: stat(0.0, 2.0),
        },
        tonal: TonalTarget {
            erb_mid_rel_db: erb.iter().map(|&f| stat(mix_curve(f), 1.0)).collect(),
            erb_side_rel_db: erb
                .iter()
                .map(|&f| stat(mix_curve(f) - 10.0, 1.0))
                .collect(),
            third_oct_rel_db: third.iter().map(|&f| stat(mix_curve(f), 1.0)).collect(),
            tilt_db_per_oct: stat(-3.0, 0.3),
            tolerance_db: vec![1.0; N_ERB],
        },
        dynamics: DynamicsTarget {
            crest_db: [12.0, 12.0, 11.0, 10.0, 10.0, 9.0].map(|v| stat(v, 1.0)),
            range_db: [6.0, 6.0, 5.0, 5.0, 5.0, 6.0].map(|v| stat(v, 1.0)),
            attack_ratio_db: [3.0, 4.0, 3.0, 2.0, 2.0, 2.0].map(|v| stat(v, 0.5)),
        },
        stereo: StereoTarget {
            correlation: stat(0.8, 0.1),
            band_width_db: [-30.0, -20.0, -12.0, -10.0, -9.0, -8.0].map(|v| stat(v, 1.0)),
            low_mono_min_correlation: 0.85,
            balance_db: stat(0.0, 0.5),
        },
        transients: TransientTarget {
            onset_rate_hz: stat(4.0, 1.0),
            band_onset_rate_hz: [stat(2.0, 0.5); N_BAND3],
        },
        sections: Vec::new(),
        stems: None,
        weights: MatchWeights::default(),
        applied_deltas: Vec::new(),
    }
}

fn stem_curve(k: StemKind, f: f32) -> f32 {
    let v = match k {
        StemKind::Vocals => bump(f, 1500.0, -6.0, 4.0),
        StemKind::Bass => bump(f, 80.0, -4.0, 4.0),
        StemKind::Guitar => bump(f, 1500.0, -6.0, 2.5),
        StemKind::Drums => bump(f, 3000.0, -10.0, 0.8),
        StemKind::Piano | StemKind::Other => bump(f, 800.0, -8.0, 1.5),
    };
    // Vocals and guitars are high-passed.
    if matches!(k, StemKind::Vocals | StemKind::Guitar) && f < 100.0 {
        v - 24.0 * (100.0 / f).log2()
    } else {
        v
    }
}

/// Stem level relative to the mix (LU) in [`profile_with_stems`].
pub fn stem_level(k: StemKind) -> f32 {
    match k {
        StemKind::Vocals => -3.0,
        StemKind::Bass => -6.0,
        StemKind::Guitar => -8.0,
        StemKind::Drums => -5.0,
        StemKind::Piano | StemKind::Other => -10.0,
    }
}

/// Synthetic stems of [`profile_with_stems`].
pub const STEMS: [StemKind; 3] = [StemKind::Vocals, StemKind::Bass, StemKind::Guitar];

/// [`profile`] plus vocal/bass/guitar stem targets (masking left empty, as M023 produces).
pub fn profile_with_stems() -> TargetProfile {
    let erb = BandLayout::Erb40.centres_hz();
    let mut p = profile();
    let stems: BTreeMap<StemKind, StemProfile> = STEMS
        .iter()
        .map(|&k| {
            (
                k,
                StemProfile {
                    level_rel_mix_db: stat(stem_level(k), 1.0),
                    erb_rel_db: erb.iter().map(|&f| stat(stem_curve(k, f), 1.0)).collect(),
                    crest_db: [12.0, 12.0, 11.0, 11.0, 10.0, 10.0].map(|v| stat(v, 1.0)),
                    width_db: stat(-60.0, 1.0),
                    onset_rate_hz: stat(3.0, 1.0),
                },
            )
        })
        .collect();
    p.stems = Some(StemTargets {
        model: "synthetic".into(),
        stems,
        masking: Vec::new(),
    });
    p
}

/// Main-tap features sitting exactly on the profile's p50 values.
pub fn features_matching(p: &TargetProfile, window: Window) -> Features {
    let mut f = Features::new("main", window, true);
    let l = MAIN_LUFS;
    let session = matches!(window, Window::Session);
    f.t_end_s = 30.0;
    let lo = &mut f.loudness;
    lo.momentary_lufs = l;
    lo.short_term_lufs = l;
    lo.integrated_lufs = session.then_some(l);
    lo.lra_lu = session.then_some(p.loudness.lra_lu);
    lo.plr_db = session.then_some(p.loudness.plr_db);
    lo.psr_db = p.loudness.psr_db.p50;
    lo.true_peak_dbtp = l + lo.psr_db;
    lo.sample_peak_dbfs = lo.true_peak_dbtp;
    lo.rms_dbfs = l - 3.0;
    lo.crest_db = 12.0;
    let t = &p.tonal;
    f.spectrum.erb_mid_rel_db = t.erb_mid_rel_db.iter().map(|s| s.p50).collect();
    f.spectrum.erb_side_rel_db = t.erb_side_rel_db.iter().map(|s| s.p50).collect();
    f.spectrum.third_oct_mid_db = t.third_oct_rel_db.iter().map(|s| s.p50 + l).collect();
    f.spectrum.tilt_db_per_oct = t.tilt_db_per_oct.p50;
    f.spectrum.centroid_hz = 1500.0;
    f.spectrum.rolloff85_hz = 6000.0;
    f.spectrum.flatness = 0.2;
    let d = &p.dynamics;
    f.dynamics.crest_db = d.crest_db.map(|s| s.p50);
    f.dynamics.range_db = d.range_db.map(|s| s.p50);
    f.dynamics.attack_ratio_db = d.attack_ratio_db.map(|s| s.p50);
    let st = &p.stereo;
    f.stereo = Some(Stereo {
        correlation: st.correlation.p50,
        width_db: -10.0,
        band_correlation: [0.95, 0.9, 0.8, 0.7, 0.7, 0.6],
        band_width_db: st.band_width_db.map(|s| s.p50),
        low_mono_correlation: (st.low_mono_min_correlation + 0.1).min(1.0),
        balance_db: st.balance_db.p50,
    });
    f.transients.onset_rate_hz = p.transients.onset_rate_hz.p50;
    f.transients.band_onset_rate_hz = p.transients.band_onset_rate_hz.map(|s| s.p50);
    f.quality.frames = 300;
    f
}

/// Strip name used for each synthetic stem (the knowledge base types it).
pub fn strip_name(k: StemKind) -> &'static str {
    match k {
        StemKind::Vocals => "Lead Vox",
        StemKind::Bass => "Bass DI",
        StemKind::Guitar => "Elec Gtr",
        StemKind::Drums => "Kick",
        StemKind::Piano => "Piano",
        StemKind::Other => "Keys",
    }
}

/// One post-fader mono strip tap per stem of `p`, each exactly on its stem target (`input.1`, `input.2`...).
pub fn strips_matching(p: &TargetProfile, window: Window, kb: &KnowledgeBase) -> Vec<StripTap> {
    let Some(stems) = &p.stems else {
        return Vec::new();
    };
    stems
        .stems
        .iter()
        .enumerate()
        .map(|(i, (&k, sp))| {
            let name = strip_name(k);
            let mut f = Features::new(format!("input.{}", i + 1), window, false);
            let l = MAIN_LUFS + sp.level_rel_mix_db.p50;
            f.t_end_s = 30.0;
            f.loudness.momentary_lufs = l;
            f.loudness.short_term_lufs = l;
            f.loudness.integrated_lufs = matches!(window, Window::Session).then_some(l);
            f.loudness.crest_db = 12.0;
            f.loudness.sample_peak_dbfs = l + 12.0;
            f.loudness.true_peak_dbtp = l + 12.0;
            f.loudness.psr_db = 12.0;
            f.spectrum.erb_mid_rel_db = sp.erb_rel_db.iter().map(|s| s.p50).collect();
            f.dynamics.crest_db = sp.crest_db.map(|s| s.p50);
            f.dynamics.range_db = [5.0; N_BROAD];
            f.quality.frames = 300;
            StripTap::new(StripId::new("input", i as u32 + 1), name, f, kb)
        })
        .collect()
}

/// Adds `delta_db` to the ERB bands of `curve` whose centres lie in `[lo, hi)` Hz.
pub fn shift_band(curve: &mut [f32], lo: f32, hi: f32, delta_db: f32) {
    let c = BandLayout::Erb40.centres_hz();
    for (v, f) in curve.iter_mut().zip(c) {
        if f >= lo && f < hi {
            *v += delta_db;
        }
    }
}

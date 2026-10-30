//! Base profiles a description is applied to: a neutral profile and a few coarse genre profiles, all with wide
//! statistics so any reference or measured profile overrides them (docs/research/analysis.md §3.2: genre targets
//! are a `TargetProfile` with `source = Genre` and wide stats).
//!
//! `worship` (aliases `contemporary`, `worship/contemporary`, `ccm`) is measured: the average of 3 commercial
//! contemporary-worship reference mixes (M016, see [`WORSHIP`] and `tests/worship.rs`), with wide tolerances
//! because 3 tracks are a small sample. The other genres are still coarse placeholders built from a spectral tilt.

use mix_core::bands::{N_BAND3, N_BROAD, N_ERB, N_THIRD_OCT};
use mix_core::features::Stat;
use mix_core::profile::{
    AnalysisParams, DynamicsTarget, LoudnessTarget, MatchWeights, ProfileSource, StereoTarget,
    TargetProfile, TonalTarget, TransientTarget, PROFILE_SCHEMA_VERSION,
};
use mix_core::BandLayout;

/// A `Stat` centred on `p50` with symmetric spread `w`.
fn wide(p50: f32, w: f32) -> Stat {
    Stat {
        p10: p50 - w,
        p50,
        p90: p50 + w,
    }
}

/// Measured per-band statistics (p50s) for a genre derived from reference mixes.
struct Measured {
    erb_mid: [f32; N_ERB],
    erb_side: [f32; N_ERB],
    third_oct: [f32; N_THIRD_OCT],
    psr: f32,
    crest: [f32; N_BROAD],
    range: [f32; N_BROAD],
    attack: [f32; N_BROAD],
    width: [f32; N_BROAD],
    low_mono: f32,
    band_onset: [f32; N_BAND3],
}

/// Averages (p50s) of the whole-song profiles of the 3 reference mixes in fixtures/mixes (Holy Forever, I Know That
/// I Know, Show Me Your Face: contemporary worship, commercially mastered), measured by M016 with mix-reference
/// (analysis v1). Statistics only; `tests/worship.rs` re-derives them from the local files.
const WORSHIP: Measured = Measured {
    erb_mid: [
        -16.6, -13.1, -14.5, -16.5, -16.3, -18.7, -20.0, -20.5, -19.4, -19.9, -21.3, -21.7, -19.3,
        -20.5, -20.8, -21.8, -22.8, -22.7, -23.1, -24.8, -26.5, -26.9, -27.1, -28.0, -28.7, -30.3,
        -30.9, -31.6, -30.6, -31.2, -32.4, -33.2, -34.6, -34.5, -35.5, -37.5, -39.3, -41.5, -45.5,
        -54.2,
    ],
    erb_side: [
        -44.4, -35.2, -29.5, -26.5, -22.6, -21.4, -22.9, -23.1, -21.1, -22.4, -22.3, -23.4, -20.8,
        -21.2, -21.6, -23.0, -22.9, -22.6, -23.3, -25.0, -26.9, -26.6, -27.7, -28.9, -29.7, -31.3,
        -31.8, -33.5, -33.6, -35.2, -36.8, -37.6, -39.2, -40.4, -41.8, -43.8, -46.5, -50.4, -55.8,
        -70.2,
    ],
    third_oct: [
        -35.7, -34.7, -25.4, -20.1, -17.1, -16.6, -16.8, -16.9, -17.5, -17.5, -17.1, -19.5, -18.7,
        -18.1, -19.2, -18.0, -16.7, -18.2, -19.3, -20.0, -22.3, -23.9, -25.1, -27.1, -28.1, -28.3,
        -30.5, -31.6, -34.7, -39.5, -53.1,
    ],
    psr: 8.9,
    crest: [8.7, 6.4, 5.0, 5.4, 4.8, 6.6],
    range: [13.3, 5.5, 5.4, 9.8, 9.4, 9.2],
    attack: [1.5, 2.4, 0.1, 0.0, 0.9, 1.6],
    width: [-22.7, -8.4, -2.4, -0.9, -1.1, -4.8],
    low_mono: 0.79,
    band_onset: [3.3, 1.3, 2.8],
};

/// Spread (dB) around measured curve values: 3 references are a small sample, keep the target wide.
const MEASURED_CURVE_TOL_DB: f32 = 4.0;

/// Coarse genre parameters: tilt (dB/oct), loudness context, dynamics and stereo defaults.
struct GenreParams {
    name: &'static str,
    aliases: &'static [&'static str],
    /// Measured curves/per-band values; `None` = placeholder built from `tilt` and the scalars below.
    measured: Option<&'static Measured>,
    tilt: f32,
    lufs: f32,
    plr: f32,
    lra: f32,
    crest: f32,
    correlation: f32,
    width_db: f32,
    onset_rate: f32,
}

/// Genre parameters (wide tolerances). Only `worship` is measured; the rest are coarse placeholders. A reference
/// profile always overrides them.
const GENRES: &[GenreParams] = &[
    GenreParams {
        name: "worship",
        aliases: &["contemporary", "worship/contemporary", "ccm"],
        measured: Some(&WORSHIP),
        tilt: -5.1,
        lufs: -8.8,
        plr: 9.0,
        lra: 9.7,
        crest: 6.1,
        correlation: 0.47,
        width_db: -6.7,
        onset_rate: 2.1,
    },
    GenreParams {
        aliases: &[],
        measured: None,
        name: "pop",
        tilt: -3.5,
        lufs: -12.0,
        plr: 10.0,
        lra: 6.0,
        crest: 10.0,
        correlation: 0.55,
        width_db: -6.0,
        onset_rate: 5.0,
    },
    GenreParams {
        aliases: &[],
        measured: None,
        name: "rock",
        tilt: -3.0,
        lufs: -11.0,
        plr: 10.0,
        lra: 6.0,
        crest: 10.0,
        correlation: 0.6,
        width_db: -7.0,
        onset_rate: 6.0,
    },
    GenreParams {
        aliases: &[],
        measured: None,
        name: "edm",
        tilt: -3.0,
        lufs: -9.0,
        plr: 8.0,
        lra: 4.0,
        crest: 8.0,
        correlation: 0.6,
        width_db: -5.0,
        onset_rate: 7.0,
    },
    GenreParams {
        aliases: &[],
        measured: None,
        name: "hiphop",
        tilt: -4.0,
        lufs: -10.0,
        plr: 9.0,
        lra: 5.0,
        crest: 9.0,
        correlation: 0.65,
        width_db: -8.0,
        onset_rate: 5.0,
    },
    GenreParams {
        aliases: &[],
        measured: None,
        name: "jazz",
        tilt: -4.5,
        lufs: -18.0,
        plr: 16.0,
        lra: 12.0,
        crest: 14.0,
        correlation: 0.5,
        width_db: -6.0,
        onset_rate: 4.0,
    },
    GenreParams {
        aliases: &[],
        measured: None,
        name: "acoustic",
        tilt: -4.5,
        lufs: -16.0,
        plr: 14.0,
        lra: 9.0,
        crest: 13.0,
        correlation: 0.55,
        width_db: -6.0,
        onset_rate: 3.5,
    },
    GenreParams {
        aliases: &[],
        measured: None,
        name: "live",
        tilt: -4.0,
        lufs: -16.0,
        plr: 12.0,
        lra: 8.0,
        crest: 12.0,
        correlation: 0.6,
        width_db: -7.0,
        onset_rate: 5.0,
    },
];

/// Names accepted by [`genre_profile`].
pub fn genre_names() -> Vec<&'static str> {
    GENRES.iter().map(|g| g.name).collect()
}

/// Neutral profile: -4 dB/oct tilt, mid-range dynamics, centred moderate width, all statistics wide.
pub fn neutral_profile() -> TargetProfile {
    build(
        &GenreParams {
            name: "",
            aliases: &[],
            measured: None,
            tilt: -4.0,
            lufs: -14.0,
            plr: 12.0,
            lra: 7.0,
            crest: 11.0,
            correlation: 0.6,
            width_db: -6.0,
            onset_rate: 5.0,
        },
        ProfileSource::Description {
            text: String::new(),
        },
    )
}

/// Coarse genre profile (`source = Genre`), `None` for an unknown name (see [`genre_names`]).
pub fn genre_profile(name: &str) -> Option<TargetProfile> {
    let g = GENRES.iter().find(|g| {
        g.name.eq_ignore_ascii_case(name) || g.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
    })?;
    Some(build(
        g,
        ProfileSource::Genre {
            name: g.name.to_string(),
        },
    ))
}

fn build(g: &GenreParams, source: ProfileSource) -> TargetProfile {
    let curve = |centres: Vec<f32>| -> Vec<Stat> {
        centres
            .iter()
            .map(|&f| wide(g.tilt * (f / 1000.0).log2(), 3.0))
            .collect()
    };
    let erb = curve(BandLayout::Erb40.centres_hz());
    let third = curve(BandLayout::ThirdOctave31.centres_hz());
    debug_assert_eq!((erb.len(), third.len()), (N_ERB, N_THIRD_OCT));
    let mut p = placeholder(g, source, erb, third);
    if let Some(m) = g.measured {
        apply_measured(&mut p, m);
    }
    p
}

fn apply_measured(p: &mut TargetProfile, m: &Measured) {
    let t = MEASURED_CURVE_TOL_DB;
    let curve = |v: &[f32]| v.iter().map(|&x| wide(x, t)).collect::<Vec<_>>();
    p.tonal.erb_mid_rel_db = curve(&m.erb_mid);
    p.tonal.erb_side_rel_db = curve(&m.erb_side);
    p.tonal.third_oct_rel_db = curve(&m.third_oct);
    p.tonal.tolerance_db = vec![t; N_ERB];
    p.loudness.psr_db = wide(m.psr, 2.5);
    for b in 0..N_BROAD {
        p.dynamics.crest_db[b] = wide(m.crest[b], 3.0);
        p.dynamics.range_db[b] = wide(m.range[b], 4.0);
        p.dynamics.attack_ratio_db[b] = wide(m.attack[b], 3.0);
        p.stereo.band_width_db[b] = wide(m.width[b], 4.0);
    }
    p.stereo.low_mono_min_correlation = m.low_mono;
    for b in 0..N_BAND3 {
        p.transients.band_onset_rate_hz[b] = wide(m.band_onset[b], 1.5);
    }
}

fn placeholder(
    g: &GenreParams,
    source: ProfileSource,
    erb: Vec<Stat>,
    third: Vec<Stat>,
) -> TargetProfile {
    TargetProfile {
        schema_version: PROFILE_SCHEMA_VERSION,
        source,
        analysis: AnalysisParams {
            sample_rate: 48_000,
            fft_size: 8192,
            hop: 2048,
            version: 1,
        },
        section: None,
        loudness: LoudnessTarget {
            integrated_lufs: g.lufs,
            lra_lu: g.lra,
            plr_db: g.plr,
            psr_db: wide(g.plr - 2.0, 2.0),
            short_term_rel_lu: wide(0.0, 3.0),
        },
        tonal: TonalTarget {
            erb_mid_rel_db: erb,
            erb_side_rel_db: Vec::new(),
            third_oct_rel_db: third,
            tilt_db_per_oct: wide(g.tilt, 1.0),
            tolerance_db: vec![3.0; N_ERB],
        },
        dynamics: DynamicsTarget {
            crest_db: [wide(g.crest, 3.0); N_BROAD],
            range_db: [wide(8.0, 3.0); N_BROAD],
            attack_ratio_db: [wide(8.0, 3.0); N_BROAD],
        },
        stereo: StereoTarget {
            correlation: wide(g.correlation, 0.25),
            band_width_db: [wide(g.width_db, 3.0); N_BROAD],
            low_mono_min_correlation: 0.8,
            balance_db: wide(0.0, 1.0),
        },
        transients: TransientTarget {
            onset_rate_hz: wide(g.onset_rate, 2.0),
            band_onset_rate_hz: [wide(g.onset_rate / 2.0, 1.5); N_BAND3],
        },
        sections: Vec::new(),
        stems: None,
        weights: MatchWeights::default(),
        applied_deltas: Vec::new(),
    }
}

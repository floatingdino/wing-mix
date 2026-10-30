//! Base profiles a description is applied to: a neutral profile and a few coarse genre profiles, all with wide
//! statistics so any reference or measured profile overrides them (docs/research/analysis.md §3.2: genre targets
//! are a `TargetProfile` with `source = Genre` and wide stats).

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

/// Coarse genre parameters: tilt (dB/oct), loudness context, dynamics and stereo defaults.
struct GenreParams {
    name: &'static str,
    tilt: f32,
    lufs: f32,
    plr: f32,
    lra: f32,
    crest: f32,
    correlation: f32,
    width_db: f32,
    onset_rate: f32,
}

/// Coarse placeholder values (wide tolerances); not measured data. A reference profile always overrides them.
const GENRES: &[GenreParams] = &[
    GenreParams {
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
    let g = GENRES.iter().find(|g| g.name.eq_ignore_ascii_case(name))?;
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

//! Explainable feature classifier: nearest prototype over a handful of scale-free descriptors.
//!
//! Each [`Prototype`] is a hand-written point in descriptor space (band shares, centroid, flatness, crest, onset
//! rate) for one way a source commonly sounds; a class may have several (a bright close-miked snare and a
//! body-heavy one sound nothing alike). The descriptor is compared with every prototype by weighted Euclidean
//! distance in normalised units ([`SCALES`]); the class probability is a softmax over each class's nearest
//! prototype distance. Evidence names the prototype and the dimensions that agree/disagree most, so every verdict
//! can be read back ("air share −2 dB vs prototype 0 dB...").
//!
//! The prototype values were read off typical spectra (and checked against mix-sim's synthetic tracks and the
//! real live multitrack fixture); they are deliberately few and round so they stay editable.

use crate::descriptor::{Descriptor, BAND_NAMES};
use crate::types::{FeatureClass, SourceType};
use serde::{Deserialize, Serialize};

/// Below this RMS level (dBFS) a capture is treated as silent/bleed-only and features abstain. A line-check or
/// performance capture of a close-miked source sits well above it; on the real live multitrack, captures below it
/// were almost always bleed (a mic whose source was not playing).
pub const MIN_RMS_DBFS: f32 = -50.0;
/// Above this gated fraction the capture is mostly silence and features abstain.
pub const MAX_GATED_FRACTION: f32 = 0.9;

/// Number of descriptor dimensions used for distance.
pub const N_DIMS: usize = 10;
/// Names of the distance dimensions (for evidence).
pub const DIM_NAMES: [&str; N_DIMS] = [
    "sub share",
    "low share",
    "low-mid share",
    "mid share",
    "presence share",
    "air share",
    "centroid",
    "flatness",
    "crest",
    "onset rate",
];
/// One normalised unit per dimension: 8 dB of band share, ¾ octave of centroid, half a decade of flatness,
/// 4 dB of crest, ln-rate 0.6 (about a factor 1.8 in onset rate).
pub const SCALES: [f32; N_DIMS] = [8.0, 8.0, 8.0, 8.0, 8.0, 8.0, 0.75, 0.5, 4.0, 0.6];
/// Per-dimension weights (band shares are correlated, so each counts a little less).
pub const WEIGHTS: [f32; N_DIMS] = [0.7, 0.7, 0.7, 0.7, 0.7, 0.7, 1.0, 1.0, 1.0, 1.0];

/// Band shares are clamped here (dB) so "absent" bands do not dominate distances.
const SHARE_FLOOR_DB: f32 = -45.0;

/// A reference point in descriptor space.
#[derive(Clone, Copy, Debug)]
pub struct Prototype {
    pub name: &'static str,
    pub source: SourceType,
    /// sub, low, low-mid, mid, presence, air share (dB re channel total).
    pub bands: [f32; 6],
    /// Centroid, octaves re 1 kHz.
    pub centroid_oct: f32,
    pub flatness: f32,
    pub crest_db: f32,
    pub onset_rate_hz: f32,
}

const fn p(
    name: &'static str,
    source: SourceType,
    bands: [f32; 6],
    centroid_oct: f32,
    flatness: f32,
    crest_db: f32,
    onset_rate_hz: f32,
) -> Prototype {
    Prototype {
        name,
        source,
        bands,
        centroid_oct,
        flatness,
        crest_db,
        onset_rate_hz,
    }
}

use SourceType::*;

/// The prototype table.
pub const PROTOTYPES: &[Prototype] = &[
    // Kick: almost everything below 250 Hz, centroid ~60-100 Hz, regular hits.
    p(
        "kick",
        Kick,
        [-2.0, -4.0, -30.0, -38.0, -35.0, -35.0],
        -3.8,
        0.001,
        17.0,
        2.5,
    ),
    // Snare, close and bright (tight tuning, lots of wire): noisy, centroid above 2 kHz.
    p(
        "snare/bright",
        SnareTop,
        [-25.0, -7.0, -19.0, -7.0, -4.0, -7.0],
        1.9,
        0.3,
        22.0,
        3.0,
    ),
    // Snare, body-heavy with kit bleed: low/low-mid dominant, very high crest.
    p(
        "snare/body",
        SnareTop,
        [-27.0, -2.0, -5.0, -15.0, -15.0, -25.0],
        -1.4,
        0.015,
        30.0,
        3.0,
    ),
    // Toms: low band dominant, little above 2.5 kHz, high crest (hits with silence or bleed between).
    p(
        "tom",
        TomRack,
        [-35.0, 0.0, -13.0, -20.0, -22.0, -30.0],
        -2.3,
        0.005,
        23.0,
        2.0,
    ),
    // Floor tom with kit bleed: more sub than a rack tom.
    p(
        "tom/floor",
        TomFloor,
        [-16.0, 0.0, -12.0, -17.0, -17.0, -26.0],
        -2.1,
        0.008,
        25.0,
        2.2,
    ),
    p(
        "tom/clean",
        TomFloor,
        [-28.0, 0.0, -22.0, -44.0, -45.0, -45.0],
        -2.9,
        0.001,
        18.0,
        1.5,
    ),
    // Hi-hat, close: air dominant, noise-like.
    p(
        "hihat/close",
        HiHat,
        [-45.0, -22.0, -34.0, -22.0, -15.0, 0.0],
        3.7,
        0.34,
        23.0,
        4.0,
    ),
    // Hi-hat mic dominated by kit bleed (typical live).
    p(
        "hihat/bleed",
        HiHat,
        [-18.0, -1.5, -7.0, -10.0, -15.0, -22.0],
        -1.3,
        0.03,
        25.0,
        3.5,
    ),
    // Overheads: cymbal wash on top of the whole kit.
    p(
        "overheads/bright",
        Overheads,
        [-14.0, -11.0, -25.0, -13.0, -9.0, -1.0],
        3.1,
        0.32,
        20.0,
        3.4,
    ),
    p(
        "overheads/kit",
        Overheads,
        [-12.0, -5.0, -4.0, -10.0, -11.0, -16.0],
        -0.27,
        0.085,
        26.0,
        3.5,
    ),
    // Shakers/tambourines: presence/air, very dense onsets.
    p(
        "percussion/shaker",
        Percussion,
        [-45.0, -45.0, -34.0, -19.0, -5.0, -2.0],
        2.85,
        0.14,
        22.0,
        8.0,
    ),
    // Bass guitar (DI/amp): low band dominant, steady (low crest), dull top.
    p(
        "bass",
        BassDi,
        [-14.0, 0.0, -14.0, -18.0, -27.0, -45.0],
        -2.9,
        0.001,
        14.0,
        2.5,
    ),
    // Sustained bass (long notes, few attacks): even lower centroid and crest.
    p(
        "bass/sustained",
        BassDi,
        [-14.0, 0.0, -27.0, -44.0, -45.0, -45.0],
        -3.5,
        0.001,
        11.0,
        0.3,
    ),
    // Synth/filtered bass: fundamental in the low-mids, brighter but still no top.
    p(
        "bass/synth",
        BassDi,
        [-42.0, -5.0, -2.0, -13.0, -37.0, -45.0],
        -1.5,
        0.001,
        10.5,
        4.0,
    ),
    // Acoustic guitar: broadband, string noise (flatness), strummed onsets.
    p(
        "acoustic guitar",
        AcousticGuitar,
        [-17.0, -4.0, -4.0, -8.0, -13.0, -16.0],
        -0.3,
        0.1,
        18.5,
        4.7,
    ),
    // Plucked bright guitar (single notes/arpeggios).
    p(
        "guitar/plucked",
        ElectricGuitar,
        [-21.0, -27.0, -8.0, -4.0, -6.0, -8.0],
        1.75,
        0.33,
        22.0,
        1.0,
    ),
    // Keys/pads: low-mid and mid, little low end or air, tonal, slow.
    p(
        "keys",
        KeysSynth,
        [-45.0, -45.0, -3.0, -3.0, -25.0, -45.0],
        -0.3,
        0.001,
        12.0,
        0.9,
    ),
    p(
        "pad",
        KeysSynth,
        [-45.0, -45.0, -4.0, -3.0, -22.0, -42.0],
        -0.28,
        0.002,
        13.3,
        0.45,
    ),
    p(
        "synth lead",
        KeysSynth,
        [-45.0, -45.0, -45.0, 0.0, -18.0, -28.0],
        0.6,
        0.001,
        9.5,
        0.9,
    ),
    // Vocals: low-mid/mid with clear presence and some air, phrase-rate onsets.
    p(
        "vocal",
        LeadVocal,
        [-42.0, -25.0, -3.0, -4.0, -21.0, -28.0],
        -0.4,
        0.015,
        16.0,
        2.1,
    ),
    p(
        "vocal/close",
        LeadVocal,
        [-45.0, -45.0, -3.0, -4.0, -17.0, -35.0],
        -0.35,
        0.002,
        12.3,
        1.3,
    ),
];

/// Class probabilities and the evidence behind them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeatureVerdict {
    /// Probability per class, indexed by [`FeatureClass::idx`] (sums to 1).
    pub probs: [f32; 9],
    /// Nearest prototype per class: (prototype name, source, distance), indexed like `probs`.
    pub nearest: Vec<(String, SourceType, f32)>,
    /// Human-readable notes on the best match (closest and furthest dimensions).
    pub notes: Vec<String>,
}

impl FeatureVerdict {
    /// Most likely class.
    pub fn best(&self) -> FeatureClass {
        let i = (0..9)
            .max_by(|&a, &b| self.probs[a].total_cmp(&self.probs[b]))
            .unwrap_or(0);
        FeatureClass::ALL[i]
    }

    pub fn prob(&self, c: FeatureClass) -> f32 {
        self.probs[c.idx()]
    }

    /// The source type of the nearest prototype of a class.
    pub fn source_of(&self, c: FeatureClass) -> SourceType {
        self.nearest[c.idx()].1
    }
}

fn vector(d: &Descriptor) -> [f32; N_DIMS] {
    let s = |x: f32| x.max(SHARE_FLOOR_DB);
    [
        s(d.band_share_db[0]),
        s(d.band_share_db[1]),
        s(d.band_share_db[2]),
        s(d.band_share_db[3]),
        s(d.band_share_db[4]),
        s(d.band_share_db[5]),
        d.centroid_oct,
        d.flatness.clamp(1e-3, 1.0).log10(),
        d.crest_db,
        d.onset_rate_hz.max(0.0).ln_1p(),
    ]
}

fn proto_vector(p: &Prototype) -> [f32; N_DIMS] {
    vector(&Descriptor {
        band_share_db: p.bands,
        centroid_oct: p.centroid_oct,
        rolloff_oct: 0.0,
        flatness: p.flatness,
        crest_db: p.crest_db,
        band_crest_db: 0.0,
        onset_rate_hz: p.onset_rate_hz,
        gated_fraction: 0.0,
        rms_dbfs: 0.0,
    })
}

/// Per-dimension normalised signed differences (descriptor − prototype).
fn diffs(v: &[f32; N_DIMS], p: &Prototype) -> [f32; N_DIMS] {
    let q = proto_vector(p);
    let mut out = [0.0; N_DIMS];
    for i in 0..N_DIMS {
        out[i] = (v[i] - q[i]) / SCALES[i] * WEIGHTS[i].sqrt();
    }
    out
}

/// Why features abstain, if they do.
pub fn abstain_reason(d: &Descriptor) -> Option<String> {
    if d.rms_dbfs < MIN_RMS_DBFS {
        Some(format!(
            "level {:.0} dBFS below {MIN_RMS_DBFS:.0} dBFS: silent or bleed only",
            d.rms_dbfs
        ))
    } else if d.gated_fraction > MAX_GATED_FRACTION {
        Some(format!(
            "{:.0}% of the capture is below the gate",
            d.gated_fraction * 100.0
        ))
    } else {
        None
    }
}

/// Classify a descriptor. `None` when the capture has too little signal ([`abstain_reason`]).
pub fn classify_descriptor(d: &Descriptor) -> Option<FeatureVerdict> {
    if abstain_reason(d).is_some() {
        return None;
    }
    let v = vector(d);
    let mut nearest: Vec<(String, SourceType, f32)> = FeatureClass::ALL
        .iter()
        .map(|c| (String::new(), c.default_source(), f32::INFINITY))
        .collect();
    let mut best_diffs = [0.0f32; N_DIMS];
    let mut best_dist = f32::INFINITY;
    for proto in PROTOTYPES {
        let Some(c) = proto.source.feature_class() else {
            continue;
        };
        let dv = diffs(&v, proto);
        let dist = dv.iter().map(|x| x * x).sum::<f32>().sqrt();
        let slot = &mut nearest[c.idx()];
        if dist < slot.2 {
            *slot = (proto.name.to_string(), proto.source, dist);
        }
        if dist < best_dist {
            best_dist = dist;
            best_diffs = dv;
        }
    }
    // Softmax over -d²/2 (relative to the best, for numerical safety).
    let mut probs = [0.0f32; 9];
    for (i, n) in nearest.iter().enumerate() {
        probs[i] = (-(n.2 * n.2 - best_dist * best_dist) / 2.0).exp();
    }
    let sum: f32 = probs.iter().sum();
    probs.iter_mut().for_each(|x| *x /= sum);

    let best_i = (0..9)
        .min_by(|&a, &b| nearest[a].2.total_cmp(&nearest[b].2))
        .unwrap_or(0);
    let mut order: Vec<usize> = (0..N_DIMS).collect();
    order.sort_by(|&a, &b| best_diffs[a].abs().total_cmp(&best_diffs[b].abs()));
    let fmt_dim = |i: usize| {
        let val = v[i];
        match i {
            0..=5 => format!("{} {:.0} dB", BAND_NAMES[i], val),
            6 => format!("centroid {:.0} Hz", 1000.0 * val.exp2()),
            7 => format!("flatness {:.3}", 10f32.powf(val)),
            8 => format!("crest {val:.0} dB"),
            _ => format!("onsets {:.1}/s", val.exp_m1()),
        }
    };
    let notes = vec![
        format!(
            "nearest prototype '{}' (distance {:.2})",
            nearest[best_i].0, nearest[best_i].2
        ),
        format!("agrees on {}, {}", fmt_dim(order[0]), fmt_dim(order[1])),
        format!("differs most on {}", fmt_dim(order[N_DIMS - 1])),
    ];
    Some(FeatureVerdict {
        probs,
        nearest,
        notes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desc_of(p: &Prototype) -> Descriptor {
        Descriptor {
            band_share_db: p.bands,
            centroid_oct: p.centroid_oct,
            rolloff_oct: p.centroid_oct + 0.5,
            flatness: p.flatness,
            crest_db: p.crest_db,
            band_crest_db: 8.0,
            onset_rate_hz: p.onset_rate_hz,
            gated_fraction: 0.0,
            rms_dbfs: -20.0,
        }
    }

    #[test]
    fn every_prototype_classifies_as_its_own_class() {
        for p in PROTOTYPES {
            let v = classify_descriptor(&desc_of(p)).unwrap();
            assert_eq!(Some(v.best()), p.source.feature_class(), "{}", p.name);
            assert!((v.probs.iter().sum::<f32>() - 1.0).abs() < 1e-4);
            assert!(v.notes[0].contains(p.name));
        }
    }

    #[test]
    fn abstains_on_silence() {
        let mut d = desc_of(&PROTOTYPES[0]);
        d.rms_dbfs = -90.0;
        assert!(classify_descriptor(&d).is_none());
        assert!(abstain_reason(&d).unwrap().contains("bleed"));
        d.rms_dbfs = -20.0;
        d.gated_fraction = 0.95;
        assert!(classify_descriptor(&d).is_none());
    }
}

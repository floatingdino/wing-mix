//! `TargetProfile`: what the mix should move towards (docs/research/analysis.md §5).
//!
//! Built offline from a reference (M022, using mix-analysis' `ProfileBuilder`), a description (M024), a genre table
//! (M005) or a merge of these. JSON on disk, cached by blake3 of the source. All tonal targets are loudness-relative
//! (`rel_db`), so the console mix is never driven to the reference's mastered loudness.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::bands::{N_BAND3, N_BROAD, N_ERB, N_THIRD_OCT};
use crate::features::Stat;

/// Current [`TargetProfile::schema_version`]. Bump on any field/semantics change; caches invalidate.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;

/// What the mix should move towards.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TargetProfile {
    /// Bump on any field/semantics change; cache invalidates.
    pub schema_version: u32,
    pub source: ProfileSource,
    /// So live analysis can assert compatibility.
    pub analysis: AnalysisParams,
    /// Section the top-level targets describe (None = whole song).
    pub section: Option<SectionRef>,
    pub loudness: LoudnessTarget,
    pub tonal: TonalTarget,
    pub dynamics: DynamicsTarget,
    pub stereo: StereoTarget,
    pub transients: TransientTarget,
    pub sections: Vec<SectionProfile>,
    /// Only when the stem sidecar ran.
    pub stems: Option<StemTargets>,
    /// Per-dimension importance for diagnosis.
    pub weights: MatchWeights,
    /// Description terms applied on top (M024), for explanations.
    pub applied_deltas: Vec<DescriptorDelta>,
}

impl TargetProfile {
    /// Checks schema version and curve lengths (call after deserialising).
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != PROFILE_SCHEMA_VERSION {
            return Err(format!(
                "schema_version {} != {PROFILE_SCHEMA_VERSION}",
                self.schema_version
            ));
        }
        let t = &self.tonal;
        check_len("tonal.erb_mid_rel_db", t.erb_mid_rel_db.len(), N_ERB)?;
        if !t.erb_side_rel_db.is_empty() {
            check_len("tonal.erb_side_rel_db", t.erb_side_rel_db.len(), N_ERB)?;
        }
        check_len(
            "tonal.third_oct_rel_db",
            t.third_oct_rel_db.len(),
            N_THIRD_OCT,
        )?;
        check_len("tonal.tolerance_db", t.tolerance_db.len(), N_ERB)?;
        for s in &self.sections {
            check_len(
                "sections[].erb_mid_delta_db",
                s.erb_mid_delta_db.len(),
                N_ERB,
            )?;
        }
        if let Some(st) = &self.stems {
            for (k, p) in &st.stems {
                check_len(
                    &format!("stems.{k:?}.erb_rel_db"),
                    p.erb_rel_db.len(),
                    N_ERB,
                )?;
            }
            for m in &st.masking {
                check_len("stems.masking[].erb_msr_db", m.erb_msr_db.len(), N_ERB)?;
            }
        }
        Ok(())
    }
}

fn check_len(what: &str, got: usize, want: usize) -> Result<(), String> {
    if got == want {
        Ok(())
    } else {
        Err(format!("{what} has {got} bands, want {want}"))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum ProfileSource {
    Reference {
        file_name: String,
        blake3: String,
        duration_s: f64,
        sample_rate_in: u32,
    },
    Description {
        text: String,
    },
    Genre {
        name: String,
    },
    Merged {
        parts: Vec<ProfileSource>,
    },
}

/// Analysis parameters a profile was built with.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnalysisParams {
    pub sample_rate: u32,
    pub fft_size: u32,
    pub hop: u32,
    /// mix-analysis algorithm version (`mix_analysis::ANALYSIS_VERSION`).
    pub version: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LoudnessTarget {
    /// Informational: the console mix is not driven to the reference's mastered loudness; all other
    /// targets are loudness-relative. Used for PLR/PSR context.
    pub integrated_lufs: f32,
    pub lra_lu: f32,
    pub plr_db: f32,
    pub psr_db: Stat,
    /// Short-term minus integrated.
    pub short_term_rel_lu: Stat,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TonalTarget {
    /// 40.
    pub erb_mid_rel_db: Vec<Stat>,
    /// 40 (empty for a mono source).
    pub erb_side_rel_db: Vec<Stat>,
    /// 31, for EQ mapping and display.
    pub third_oct_rel_db: Vec<Stat>,
    pub tilt_db_per_oct: Stat,
    /// 40, default `max(1, (p90 - p10) / 2)`.
    pub tolerance_db: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DynamicsTarget {
    pub crest_db: [Stat; N_BROAD],
    pub range_db: [Stat; N_BROAD],
    pub attack_ratio_db: [Stat; N_BROAD],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StereoTarget {
    pub correlation: Stat,
    pub band_width_db: [Stat; N_BROAD],
    pub low_mono_min_correlation: f32,
    pub balance_db: Stat,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransientTarget {
    pub onset_rate_hz: Stat,
    pub band_onset_rate_hz: [Stat; N_BAND3],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionRef {
    pub index: u32,
    pub label: String,
    pub start_s: f64,
    pub end_s: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionProfile {
    pub section: SectionRef,
    /// Section short-term median minus song integrated.
    pub loudness_rel_lu: f32,
    pub tilt_db_per_oct: f32,
    /// 40, section p50 minus song p50.
    pub erb_mid_delta_db: Vec<f32>,
    pub width_db: f32,
    pub onset_rate_hz: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StemKind {
    Vocals,
    Drums,
    Bass,
    Other,
    Guitar,
    Piano,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StemTargets {
    /// e.g. "htdemucs@4.1.0" or "umxhq@1.3.0".
    pub model: String,
    pub stems: BTreeMap<StemKind, StemProfile>,
    /// Reference's pairwise masking where it matters.
    pub masking: Vec<MaskingPair>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StemProfile {
    /// Stem short-term loudness minus mix short-term loudness.
    pub level_rel_mix_db: Stat,
    /// 40, stem spectrum relative to stem loudness.
    pub erb_rel_db: Vec<Stat>,
    pub crest_db: [Stat; N_BROAD],
    pub width_db: Stat,
    pub onset_rate_hz: Stat,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MaskingPair {
    pub masked: StemKind,
    pub masker: StemKind,
    /// 40.
    pub erb_msr_db: Vec<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct MatchWeights {
    pub tonal: f32,
    pub dynamics: f32,
    pub stereo: f32,
    pub transients: f32,
    pub stems: f32,
}

impl Default for MatchWeights {
    /// Full weight everywhere except stems (bleed in separated stems → lower confidence).
    fn default() -> Self {
        MatchWeights {
            tonal: 1.0,
            dynamics: 1.0,
            stereo: 1.0,
            transients: 1.0,
            stems: 0.5,
        }
    }
}

/// e.g. term "warm", target_path "tonal.erb_mid_rel_db[4..12]", delta +1.5
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DescriptorDelta {
    pub term: String,
    pub target_path: String,
    pub delta: f32,
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn sample_profile() -> TargetProfile {
        let s = Stat {
            p10: -1.0,
            p50: 0.0,
            p90: 1.0,
        };
        TargetProfile {
            schema_version: PROFILE_SCHEMA_VERSION,
            source: ProfileSource::Merged {
                parts: vec![
                    ProfileSource::Reference {
                        file_name: "ref.wav".into(),
                        blake3: "00".into(),
                        duration_s: 180.0,
                        sample_rate_in: 44100,
                    },
                    ProfileSource::Description {
                        text: "warm".into(),
                    },
                ],
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
                psr_db: s,
                short_term_rel_lu: s,
            },
            tonal: TonalTarget {
                erb_mid_rel_db: vec![s; N_ERB],
                erb_side_rel_db: vec![s; N_ERB],
                third_oct_rel_db: vec![s; N_THIRD_OCT],
                tilt_db_per_oct: s,
                tolerance_db: vec![1.0; N_ERB],
            },
            dynamics: DynamicsTarget {
                crest_db: [s; 6],
                range_db: [s; 6],
                attack_ratio_db: [s; 6],
            },
            stereo: StereoTarget {
                correlation: s,
                band_width_db: [s; 6],
                low_mono_min_correlation: 0.8,
                balance_db: s,
            },
            transients: TransientTarget {
                onset_rate_hz: s,
                band_onset_rate_hz: [s; 3],
            },
            sections: vec![SectionProfile {
                section: SectionRef {
                    index: 0,
                    label: "intro".into(),
                    start_s: 0.0,
                    end_s: 10.0,
                },
                loudness_rel_lu: -3.0,
                tilt_db_per_oct: -4.0,
                erb_mid_delta_db: vec![0.0; N_ERB],
                width_db: -9.0,
                onset_rate_hz: 2.0,
            }],
            stems: Some(StemTargets {
                model: "htdemucs@4.1.0".into(),
                stems: [(
                    StemKind::Vocals,
                    StemProfile {
                        level_rel_mix_db: s,
                        erb_rel_db: vec![s; N_ERB],
                        crest_db: [s; 6],
                        width_db: s,
                        onset_rate_hz: s,
                    },
                )]
                .into_iter()
                .collect(),
                masking: vec![MaskingPair {
                    masked: StemKind::Vocals,
                    masker: StemKind::Guitar,
                    erb_msr_db: vec![0.0; N_ERB],
                }],
            }),
            weights: MatchWeights::default(),
            applied_deltas: vec![DescriptorDelta {
                term: "warm".into(),
                target_path: "tonal.erb_mid_rel_db[4..12]".into(),
                delta: 1.5,
            }],
        }
    }

    #[test]
    fn json_round_trip_and_validate() {
        let p = sample_profile();
        p.validate().unwrap();
        let s = serde_json::to_string_pretty(&p).unwrap();
        let back: TargetProfile = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
        back.validate().unwrap();
    }

    #[test]
    fn validate_rejects_wrong_version_and_lengths() {
        let mut p = sample_profile();
        p.schema_version = 0;
        assert!(p.validate().is_err());
        let mut p = sample_profile();
        p.tonal.third_oct_rel_db.pop();
        assert!(p.validate().is_err());
    }
}

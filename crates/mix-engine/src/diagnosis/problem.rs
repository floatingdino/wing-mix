//! Serialisable diagnosis output: [`Problem`] and its parts (GUI, planner M032 and controller M033 read these).

use mix_core::ids::StripId;
use mix_core::profile::StemKind;
use serde::{Deserialize, Serialize};

/// What is wrong, in the knowledge base's vocabulary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProblemKind {
    /// A named band (workflow.toml `[bands]`) deviates from the target (`band_dev_db.<band>`).
    Tonal,
    /// Presence-band (2–5 kHz) excess: a tonal excess reported under its engineering name.
    Harshness,
    /// Vocal sibilance above target or the de-esser threshold (`sibilance_db`).
    Sibilance,
    /// Whole-spectrum slope deviates (`tilt_dev_db_per_oct`).
    Tilt,
    /// A source or stem sits too loud/quiet relative to the mix (`level_dev_lu`).
    Level,
    /// Crest/PSR/PLR/loudness-range above target: too dynamic (`crest_db`, `lra_lu`, `plr_db` deviations).
    TooDynamic,
    /// Crest/PSR/PLR/loudness-range below target: squashed.
    Squashed,
    /// Onset energy (attack ratio) below/above target (`transient_db`).
    Punch,
    /// One source masks a higher-priority one in a band (`masking_db.<band>`).
    Masking,
    /// Low end not mono enough (`low_correlation`).
    LowEndMono,
    /// Side/mid ratio deviates (`width_dev_db`).
    Width,
    /// L/R power imbalance.
    StereoBalance,
    /// L/R correlation far below target (phase problem).
    Phase,
    /// Reverb/FX amount (depth) deviates (`reverb_rel_dev_db`, `wet_rel_dry_lu`).
    Depth,
    /// A per-source problem from sources.toml `problems` matched (id in [`Problem::rules`]).
    SourceProblem,
}

impl ProblemKind {
    /// Stable snake_case name.
    pub fn as_str(self) -> &'static str {
        match self {
            ProblemKind::Tonal => "tonal",
            ProblemKind::Harshness => "harshness",
            ProblemKind::Sibilance => "sibilance",
            ProblemKind::Tilt => "tilt",
            ProblemKind::Level => "level",
            ProblemKind::TooDynamic => "too_dynamic",
            ProblemKind::Squashed => "squashed",
            ProblemKind::Punch => "punch",
            ProblemKind::Masking => "masking",
            ProblemKind::LowEndMono => "low_end_mono",
            ProblemKind::Width => "width",
            ProblemKind::StereoBalance => "stereo_balance",
            ProblemKind::Phase => "phase",
            ProblemKind::Depth => "depth",
            ProblemKind::SourceProblem => "source_problem",
        }
    }
}

/// What a problem is about.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Subject {
    /// The main mix.
    Mix,
    /// One console strip (generic id from mix-surface) with its source type (sources.toml id) when known.
    Strip {
        strip: StripId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
    },
    /// A stem (all strips mapped to that stem kind; reference stem targets).
    Stem {
        stem: StemKind,
        /// Strips aggregated into this stem.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        strips: Vec<StripId>,
    },
    /// A source/family named by a description intent that no strip or stem could be matched to yet.
    Source { source: String },
}

impl Subject {
    /// Short label for ids and explanations (`mix`, `input.3`, `stem:Vocals`, `src:lead_vocal`).
    pub fn label(&self) -> String {
        match self {
            Subject::Mix => "mix".into(),
            Subject::Strip { strip, .. } => strip.to_string(),
            Subject::Stem { stem, .. } => format!("stem:{stem:?}"),
            Subject::Source { source } => format!("src:{source}"),
        }
    }
}

/// A frequency band a problem is about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BandRef {
    /// workflow.toml band name (`low_mid`), or a Broad6 name (`presence`) for dynamics/width bands.
    pub name: String,
    pub lo_hz: f32,
    pub hi_hz: f32,
    /// Centre of the largest deviation (ERB band) inside the band; planners use it as `band_center` /
    /// `masking_peak_hz`.
    pub peak_hz: f32,
}

/// Where a problem lives.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scope {
    pub subject: Subject,
    /// The other party (masker for [`ProblemKind::Masking`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other: Option<Subject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub band: Option<BandRef>,
}

impl Scope {
    pub fn mix() -> Self {
        Scope {
            subject: Subject::Mix,
            other: None,
            band: None,
        }
    }

    pub fn of(subject: Subject) -> Self {
        Scope {
            subject,
            other: None,
            band: None,
        }
    }

    pub fn with_band(mut self, band: BandRef) -> Self {
        self.band = Some(band);
        self
    }
}

/// The numbers behind a problem.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// Knowledge-base feature name (`band_dev_db.low_mid`, `level_dev_lu`, `masking_db.presence`...).
    pub feature: String,
    /// Measured value of the underlying quantity (e.g. band level rel. loudness, crest dB). `None` for open-loop
    /// description intents that have no measurement.
    pub measured: Option<f32>,
    /// Target value of the same quantity.
    pub target: f32,
    /// Allowed |measured − target| before a problem is raised.
    pub tolerance: f32,
    /// Unit (workflow.toml unit names: `db`, `lu`, `db_per_oct`, `ratio`).
    pub unit: String,
    /// `measured − target` (for intents: the remaining requested change, sign = direction of the fix reversed,
    /// i.e. a requested +2 dB that is not yet applied reads −2).
    pub deviation: f32,
    /// `max(0, |deviation| − tolerance)`, or the distance outside the target interval ([`Evidence::interval`]).
    pub excess: f32,
    /// M070: |deviation| allowed on the measured side before `excess` starts (the tolerance, or the interval's
    /// half-width on that side), for the scrutiny scale ([`super::DiagnosisConfig::tolerance_scale`]). 0 = not
    /// rescalable (threshold evidence); infinite = one-sided check whose deviation is on the fine side.
    #[serde(skip)]
    pub allowance: f32,
}

impl Evidence {
    pub fn new(
        feature: impl Into<String>,
        unit: &str,
        measured: f32,
        target: f32,
        tol: f32,
    ) -> Self {
        let deviation = measured - target;
        Evidence {
            feature: feature.into(),
            measured: Some(measured),
            target,
            tolerance: tol,
            unit: unit.into(),
            deviation,
            excess: (deviation.abs() - tol).max(0.0),
            allowance: tol,
        }
    }

    /// Evidence against a target *distribution* (`p10/p50/p90` of the reference's short-term windows): the
    /// deviation is from the median, but only the part outside `[min(p10, p50 − tol), max(p90, p50 + tol)]`
    /// counts as excess. A live long-term/session aggregate lands anywhere inside the reference's window spread
    /// (sparse sources pull the energy mean away from the median, the spread is often skewed), so a symmetric
    /// `|m − p50| − half_spread` raises false positives on a matched mix (M034). `tolerance` is the severity
    /// scale `max(tol, (p90 − p10) / 4)`: a band whose level varies a lot between windows (intermittent parts)
    /// is a less certain comparison for an energy mean, which sits at or past p90 of such a distribution.
    pub fn interval(
        feature: impl Into<String>,
        unit: &str,
        measured: f32,
        target: mix_core::features::Stat,
        tol: f32,
    ) -> Self {
        let lo = target.p10.min(target.p50 - tol);
        let hi = target.p90.max(target.p50 + tol);
        Evidence {
            feature: feature.into(),
            measured: Some(measured),
            target: target.p50,
            tolerance: tol.max((target.p90 - target.p10) / 4.0),
            unit: unit.into(),
            deviation: measured - target.p50,
            excess: (lo - measured).max(measured - hi).max(0.0),
            allowance: if measured >= target.p50 {
                hi - target.p50
            } else {
                target.p50 - lo
            },
        }
    }

    /// One-sided check: the deviation is on the side that is fine (no excess at any scrutiny).
    pub fn fine_side(&mut self) {
        self.excess = 0.0;
        self.allowance = f32::INFINITY;
    }

    /// M070 scrutiny: the evidence with every allowance scaled by `s` (0.5 = twice as strict).
    pub fn scaled(&self, s: f32) -> Evidence {
        let mut e = self.clone();
        if (s - 1.0).abs() < 1e-6 || !(e.allowance > 0.0) || !e.deviation.is_finite() {
            return e;
        }
        e.tolerance *= s;
        if e.allowance.is_finite() {
            e.allowance *= s;
            e.excess = (e.deviation.abs() - e.allowance).max(0.0);
        }
        e
    }

    /// `true` when the measured value is above the target.
    pub fn is_excess(&self) -> bool {
        self.deviation > 0.0
    }
}

/// A strip's share of the subject's energy in the problem band (mix tonal/low-end attribution, eq.toml).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Contributor {
    pub strip: StripId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 0..1 share of the band's power at the main.
    pub share: f32,
}

/// One diagnosed problem.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Problem {
    /// Stable key across diagnosis passes (`tonal:mix@low_mid`), so the controller can track a problem.
    pub id: String,
    pub kind: ProblemKind,
    pub scope: Scope,
    pub evidence: Evidence,
    /// `excess / tolerance` × dimension weight (MatchWeights) × kind weight; ≥ 0, 0 never reported.
    pub severity: f32,
    /// 0..1: how much to trust the measurement and the target (target source, stems, analysis quality).
    pub confidence: f32,
    /// Generic processing blocks (workflow.toml `[blocks]`) that can fix it, best first.
    pub suggested_blocks: Vec<String>,
    /// Knowledge-base rules / source problems / masking strategies whose conditions hold (ids, best first).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<String>,
    /// Description terms (M024 `applied_deltas`) that shaped this target, for explanations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terms: Vec<String>,
    /// Strips contributing to a mix-scope problem, largest share first (only with strip taps).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributors: Vec<Contributor>,
    /// One-line human explanation.
    pub explanation: String,
}

impl Problem {
    /// Ranking score: severity × confidence.
    pub fn score(&self) -> f32 {
        self.severity * self.confidence
    }
}

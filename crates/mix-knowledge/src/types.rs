//! Typed mirror of every `data/knowledge/*.toml` file.
//!
//! Structs reject unknown fields (`deny_unknown_fields`) wherever the shape is fixed, so a typo or a schema drift in
//! a data file fails loading with the offending field named. The only open-ended shapes are an action's block-specific
//! parameters ([`Action::params`]) and delivery profiles ([`Delivery::profiles`]); both are still typed
//! ([`Param`], [`DeliveryProfile`]) and are validated by the consistency checker.

use std::collections::BTreeMap;

use serde::Deserialize;

// ------------------------------------------------------------------------------------------------------------
// Shared value types

/// An inclusive `[lo, hi]` range. Ordering (`lo <= hi`) is verified by the consistency checker, not on parse.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
pub struct Range(pub [f64; 2]);

impl Range {
    /// Lower bound.
    pub fn lo(&self) -> f64 {
        self.0[0]
    }
    /// Upper bound.
    pub fn hi(&self) -> f64 {
        self.0[1]
    }
    /// Arithmetic midpoint.
    pub fn mid(&self) -> f64 {
        0.5 * (self.0[0] + self.0[1])
    }
    /// Geometric midpoint (for log-scaled params such as Hz); falls back to the arithmetic one for non-positive bounds.
    pub fn geo_mid(&self) -> f64 {
        if self.0[0] > 0.0 && self.0[1] > 0.0 {
            (self.0[0] * self.0[1]).sqrt()
        } else {
            self.mid()
        }
    }
    /// `lo <= hi` and both finite.
    pub fn is_ordered(&self) -> bool {
        self.0[0].is_finite() && self.0[1].is_finite() && self.0[0] <= self.0[1]
    }
    /// Whether `x` lies in `[lo, hi]`.
    pub fn contains(&self, x: f64) -> bool {
        x >= self.0[0] && x <= self.0[1]
    }
    /// Whether `other` lies entirely within this range.
    pub fn contains_range(&self, other: &Range) -> bool {
        self.contains(other.lo()) && self.contains(other.hi())
    }
}

/// A number or a `[lo, hi]` range.
#[derive(Debug, Clone, Copy, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum NumOrRange {
    /// Exactly this value.
    Num(f64),
    /// Choose within the range.
    Range(Range),
}

/// An action parameter: a number, a `[lo, hi]` range, or a symbolic name resolved at plan time
/// (`resonance_hz`, `masking_peak_hz`, `loudness_match`, `<yielding>`, ... see workflow.toml `[schema.action]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum Param {
    /// Exactly this value.
    Num(f64),
    /// Choose within the range.
    Range(Range),
    /// Symbolic value.
    Symbol(String),
}

/// How strongly the sources agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    /// Strong consensus.
    High,
    /// Some disagreement or single strong source.
    Medium,
    /// Weak or context-dependent.
    Low,
}

/// Comparison operator of a [`Condition`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Op {
    /// `<`
    #[serde(rename = "<")]
    Lt,
    /// `<=`
    #[serde(rename = "<=")]
    Le,
    /// `>`
    #[serde(rename = ">")]
    Gt,
    /// `>=`
    #[serde(rename = ">=")]
    Ge,
    /// inside `[lo, hi]`
    #[serde(rename = "between")]
    Between,
    /// outside `[lo, hi]`
    #[serde(rename = "outside")]
    Outside,
}

/// `{ feature, op, value }`: one comparison of a measured feature.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    /// Feature name, optionally with `.band` / `.scope` suffixes (`band_dev_db.low_mid`).
    pub feature: String,
    /// Operator.
    pub op: Op,
    /// Threshold (scalar for the four ordering ops, range for `between`/`outside`).
    pub value: NumOrRange,
}

impl Condition {
    /// Feature name without suffixes (`band_dev_db.low_mid` -> `band_dev_db`).
    pub fn base_feature(&self) -> &str {
        self.feature.split('.').next().unwrap_or(&self.feature)
    }

    /// Evaluate against a measured value. Returns `None` when the value shape does not fit the operator.
    pub fn eval(&self, x: f64) -> Option<bool> {
        Some(match (self.op, self.value) {
            (Op::Lt, NumOrRange::Num(v)) => x < v,
            (Op::Le, NumOrRange::Num(v)) => x <= v,
            (Op::Gt, NumOrRange::Num(v)) => x > v,
            (Op::Ge, NumOrRange::Num(v)) => x >= v,
            (Op::Between, NumOrRange::Range(r)) => r.contains(x),
            (Op::Outside, NumOrRange::Range(r)) => !r.contains(x),
            _ => return None,
        })
    }
}

/// A block action (`{ block = "Eq", type = "peq", freq_hz = [..], gain_db = [..], q_class = "medium" }`).
///
/// The structural fields are typed; every other key is a block parameter in [`Action::params`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Action {
    /// Block name from workflow.toml `[blocks]`.
    pub block: String,
    /// Sub-kind (`Filter`: hpf/lpf, `Dyn`: compressor/..., return filters).
    #[serde(default)]
    pub kind: Option<String>,
    /// Eq band type (`peq`, `low_shelf`, `high_shelf`).
    #[serde(default, rename = "type")]
    pub ty: Option<String>,
    /// Gate mode.
    #[serde(default)]
    pub mode: Option<String>,
    /// Named Q class from eq.toml `[q_class]`.
    #[serde(default)]
    pub q_class: Option<String>,
    /// Preset reference (`from_source` or a preset id).
    #[serde(default)]
    pub preset: Option<String>,
    /// Masking target (`<yielding>`, `<keeping>`, `<self>`, `<other>`).
    #[serde(default)]
    pub target: Option<String>,
    /// Block parameters (`freq_hz`, `gain_db`, `q`, ...).
    #[serde(flatten)]
    pub params: BTreeMap<String, Param>,
}

/// `{ param, by | to }`: a relative or absolute change of a strip's current block parameter.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Adjust {
    /// Parameter name (block param such as `ratio`, `attack_ms`).
    pub param: String,
    /// Relative change range.
    #[serde(default)]
    pub by: Option<Range>,
    /// Absolute target (range, number or symbol).
    #[serde(default)]
    pub to: Option<Param>,
    /// Optional target strip/bus (fx rules).
    #[serde(default)]
    pub target: Option<String>,
    /// Optional ordering list (fx rules).
    #[serde(default)]
    pub order: Option<Vec<String>>,
}

/// A conditional rule (shared shape of dynamics.toml, eq.toml and fx.toml `[[rule]]`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Unique id within its file.
    pub id: String,
    /// Source ids, family names, `"*"` or `"mix"`.
    #[serde(default)]
    pub applies_to: Vec<String>,
    /// eq.toml: `strip` or `mix`.
    #[serde(default)]
    pub scope: Option<String>,
    /// All must hold.
    #[serde(default)]
    pub when: Vec<Condition>,
    /// At least one must hold.
    #[serde(default)]
    pub when_any: Vec<Condition>,
    /// How condition features are normalised.
    #[serde(default)]
    pub feature_basis: Option<String>,
    /// Parameter changes on the strip's current block.
    #[serde(default)]
    pub adjust: Vec<Adjust>,
    /// Full block action, or a delegation to another table.
    #[serde(default)]
    pub action: Option<RuleAction>,
    /// Free-text strategy (eq.toml mix rules).
    #[serde(default)]
    pub strategy: Option<String>,
    /// 1 (highest) .. 5.
    #[serde(default)]
    pub priority: Option<u8>,
    /// Consensus strength.
    #[serde(default)]
    pub confidence: Option<Confidence>,
    /// Keys into workflow.toml `[sources]`.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// A rule's `action`: a block action, or a delegation (`{ from = "sources.toml problems" }`).
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum RuleAction {
    /// Evaluate another table instead (eq.toml `eq_source_problem`).
    Delegate(Delegation),
    /// Insert/modify a block.
    Block(Action),
}

/// `{ from = "<file> <table>" }`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delegation {
    /// Delegation target, `"sources.toml problems"`.
    pub from: String,
}

impl Term {
    /// The antonym term id; `antonym = \"\"` in the data means none.
    pub fn antonym_id(&self) -> Option<&str> {
        self.antonym.as_deref().filter(|a| !a.is_empty())
    }
}

impl Rule {
    /// All conditions (`when` then `when_any`).
    pub fn conditions(&self) -> impl Iterator<Item = &Condition> {
        self.when.iter().chain(self.when_any.iter())
    }
}

/// `[schema]` header present in every file (extra documentation sub-tables are ignored).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SchemaHeader {
    /// Schema version.
    pub version: u32,
    /// File tag (`workflow`, `sources`, ...).
    pub file: String,
    /// What the file holds.
    pub description: String,
    /// Unit table (workflow.toml only): unit id -> description.
    #[serde(default)]
    pub units: BTreeMap<String, String>,
}

// ------------------------------------------------------------------------------------------------------------
// workflow.toml

/// `[blocks.<Name>]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlockDef {
    /// Parameter names an action on this block may use.
    pub params: Vec<String>,
    /// Allowed `kind` values (Filter, Dyn).
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Allowed gate modes.
    #[serde(default)]
    pub modes: Vec<String>,
    /// Allowed Eq band types.
    #[serde(default)]
    pub types: Vec<String>,
    /// Allowed Eq bands.
    #[serde(default)]
    pub bands: Vec<String>,
    /// Allowed send taps.
    #[serde(default)]
    pub taps: Vec<String>,
    /// Allowed FX model classes.
    #[serde(default)]
    pub model_classes: Vec<String>,
    /// Capability note.
    pub note: String,
}

/// `[features.<name>]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeatureDef {
    /// Unit id from the `[schema] units` table.
    pub unit: String,
    /// `"band"` when the feature has a per-band suffix.
    #[serde(default)]
    pub per: Option<String>,
    /// Definition.
    pub doc: String,
}

/// `[[phase]]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Phase {
    /// 1-based position in the workflow.
    pub order: u32,
    /// Phase id.
    pub id: String,
    /// Blocks the phase may touch.
    pub blocks: Vec<String>,
    /// Goal text.
    pub goal: String,
    /// Conditions that mark the phase finished.
    #[serde(default)]
    pub done_when: Vec<Condition>,
    /// Later phases are not scheduled while this is violated.
    #[serde(default)]
    pub gate_tolerance: Option<Condition>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
    /// References.
    #[serde(default)]
    pub sources: Vec<String>,
}

/// `[iteration]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Iteration {
    /// Phase ids after which lower phases are re-examined.
    pub revisit_lower_phases_after: Vec<String>,
    /// Move budget per strip per pass.
    pub max_moves_per_strip_per_pass: u32,
    /// Settling time between move and measurement (s).
    pub settle_s: Range,
    /// Minimum relative distance improvement to keep a move.
    pub keep_if_distance_reduced_by: f64,
}

/// `[targets.channel]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelTargets {
    /// Sample-peak window (dBFS).
    pub peak_dbfs: Range,
    /// RMS window (dBFS).
    pub rms_dbfs: Range,
    /// Clip guard (dBFS).
    pub clip_guard_dbfs: f64,
    /// References.
    pub sources: Vec<String>,
}

/// `[targets.mix_bus]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MixBusTargets {
    /// Sample-peak window (dBFS).
    pub peak_dbfs: Range,
    /// True-peak ceiling (dBTP).
    pub true_peak_max_dbtp: f64,
    /// References.
    pub sources: Vec<String>,
}

/// One delivery profile in `[targets.delivery]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryProfile {
    /// Integrated loudness target (LUFS), number or window.
    #[serde(default)]
    pub lufs_i: Option<NumOrRange>,
    /// Tolerance around `lufs_i` (LU).
    #[serde(default)]
    pub tolerance_lu: Option<f64>,
    /// True-peak ceiling (dBTP).
    pub true_peak_max_dbtp: f64,
    /// Peak-to-loudness-ratio window (dB).
    #[serde(default)]
    pub plr_db: Option<Range>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// `[targets.delivery]`: named profiles plus the references.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Delivery {
    /// References.
    pub sources: Vec<String>,
    /// Profiles by name (`live_main`, `streaming_spotify`, ...).
    #[serde(flatten)]
    pub profiles: BTreeMap<String, DeliveryProfile>,
}

/// `[targets.reference_matching]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceMatching {
    /// Loudness every comparison is normalised to (LUFS).
    pub normalise_to_lufs: f64,
    /// Per-band deviation tolerance (dB), keyed by band name.
    pub band_tolerance_db: BTreeMap<String, f64>,
    /// Tilt tolerance (dB/oct).
    pub tilt_tolerance_db_per_oct: f64,
    /// Level tolerance (LU).
    pub level_tolerance_lu: f64,
    /// Width tolerance (dB).
    pub width_tolerance_db: f64,
    /// Crest tolerance (dB).
    pub crest_tolerance_db: f64,
    /// Free text.
    pub note: String,
    /// References.
    pub sources: Vec<String>,
}

/// `[targets]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Targets {
    /// Per-channel gain staging targets.
    pub channel: ChannelTargets,
    /// Mix-bus headroom targets.
    pub mix_bus: MixBusTargets,
    /// Delivery loudness profiles.
    pub delivery: Delivery,
    /// Reference-matching tolerances.
    pub reference_matching: ReferenceMatching,
}

/// `[sources.<key>]`: bibliography entry.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    /// Title.
    pub title: String,
    /// Author or body.
    #[serde(default)]
    pub author: Option<String>,
    /// Year.
    #[serde(default)]
    pub year: Option<u32>,
    /// Publisher.
    #[serde(default)]
    pub publisher: Option<String>,
    /// Venue (journal/conference).
    #[serde(default)]
    pub venue: Option<String>,
    /// URL.
    #[serde(default)]
    pub url: Option<String>,
}

/// workflow.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    /// Header.
    pub schema: SchemaHeader,
    /// Processing blocks by name.
    pub blocks: BTreeMap<String, BlockDef>,
    /// Band edges (Hz) by name.
    pub bands: BTreeMap<String, Range>,
    /// Measurable features by name.
    pub features: BTreeMap<String, FeatureDef>,
    /// Workflow phases.
    pub phase: Vec<Phase>,
    /// Iteration policy.
    pub iteration: Iteration,
    /// Loudness / headroom targets.
    pub targets: Targets,
    /// Bibliography.
    pub sources: BTreeMap<String, Reference>,
}

// ------------------------------------------------------------------------------------------------------------
// sources.toml

/// Source family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    /// Drum kit parts.
    Drums,
    /// Bass.
    Bass,
    /// Guitars.
    Guitar,
    /// Keys and synths.
    Keys,
    /// Vocals.
    Vocal,
    /// Playback / tracks.
    Playback,
    /// Ambience / room mics.
    Ambience,
    /// Speech.
    Speech,
    /// Brass and strings.
    BrassStrings,
}

impl Family {
    /// The snake_case name used in the data files.
    pub fn as_str(self) -> &'static str {
        match self {
            Family::Drums => "drums",
            Family::Bass => "bass",
            Family::Guitar => "guitar",
            Family::Keys => "keys",
            Family::Vocal => "vocal",
            Family::Playback => "playback",
            Family::Ambience => "ambience",
            Family::Speech => "speech",
            Family::BrassStrings => "brass_strings",
        }
    }
}

/// `hpf` / `lpf` defaults of a source.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterDefault {
    /// Frequency window (Hz).
    pub freq_hz: Range,
    /// Slope (dB/oct).
    pub slope_db_per_oct: f64,
    /// Conservative first move (Hz).
    pub start_hz: f64,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// One entry of a source's `character` list.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Character {
    /// Region name.
    pub name: String,
    /// Region (Hz).
    pub freq_hz: Range,
    /// What boosting it does.
    pub effect: String,
}

/// One entry of a source's `problems` list.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceProblem {
    /// Id, unique within the source.
    pub id: String,
    /// Region (Hz).
    pub freq_hz: Range,
    /// Conditions (AND).
    #[serde(default)]
    pub when: Vec<Condition>,
    /// Conditions (OR).
    #[serde(default)]
    pub when_any: Vec<Condition>,
    /// Corrective action.
    pub action: Action,
    /// Consensus strength.
    pub confidence: Confidence,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// `[[source]]`: a source type.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Unique id.
    pub id: String,
    /// Family.
    pub family: Family,
    /// Name-matching hints (`?` makes the previous character optional).
    pub name_hints: Vec<String>,
    /// Fundamental/body region (Hz).
    pub fundamental_hz: Range,
    /// Useful range (Hz).
    pub useful_range_hz: Range,
    /// High-pass default.
    pub hpf: FilterDefault,
    /// Low-pass default.
    #[serde(default)]
    pub lpf: Option<FilterDefault>,
    /// Characteristic regions.
    #[serde(default)]
    pub character: Vec<Character>,
    /// Typical problems.
    #[serde(default)]
    pub problems: Vec<SourceProblem>,
    /// Gate preset id or `"none"`.
    pub gate_preset: String,
    /// Compressor preset id or `"none"`.
    pub dyn_preset: String,
    /// De-esser preset id.
    #[serde(default)]
    pub deess_preset: Option<String>,
    /// FX send preset id or `"none"`.
    pub fx_preset: String,
    /// Typical pan range (-100..100).
    pub pan_pct: Range,
    /// Typical level relative to the mix (LU).
    pub level_rel_mix_lu: Range,
    /// Polarity/alignment advice.
    #[serde(default)]
    pub polarity_note: Option<String>,
    /// Consensus strength.
    pub confidence: Confidence,
    /// References.
    pub sources: Vec<String>,
}

/// sources.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourcesFile {
    /// Header.
    pub schema: SchemaHeader,
    /// Source types.
    pub source: Vec<Source>,
}

// ------------------------------------------------------------------------------------------------------------
// dynamics.toml

/// `threshold_clamp` of a gate preset.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThresholdClamp {
    /// Statistic the clamp is relative to.
    pub max_rel: String,
    /// Maximum offset from it (dB).
    pub max_offset_db: f64,
}

/// `[[gate_preset]]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatePreset {
    /// Unique id.
    pub id: String,
    /// `gate` or `expander`.
    pub kind: String,
    /// Statistic the threshold is relative to.
    pub threshold_rel: String,
    /// Offset range (dB).
    pub threshold_offset_db: Range,
    /// Upper bound on the threshold.
    #[serde(default)]
    pub threshold_clamp: Option<ThresholdClamp>,
    /// Maximum attenuation (dB).
    #[serde(default)]
    pub range_db: Option<Range>,
    /// Attack (ms).
    #[serde(default)]
    pub attack_ms: Option<Range>,
    /// Hold (ms).
    #[serde(default)]
    pub hold_ms: Option<Range>,
    /// Release (ms).
    #[serde(default)]
    pub release_ms: Option<Range>,
    /// Expander ratio 1:N.
    #[serde(default)]
    pub expander_ratio: Option<Range>,
    /// Key high-pass (Hz).
    #[serde(default)]
    pub sidechain_hpf_hz: Option<Range>,
    /// Key low-pass (Hz).
    #[serde(default)]
    pub sidechain_lpf_hz: Option<Range>,
    /// Consensus strength.
    pub confidence: Confidence,
    /// References.
    pub sources: Vec<String>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// `[[comp_preset]]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompPreset {
    /// Unique id.
    pub id: String,
    /// `compressor` or `limiter`.
    pub kind: String,
    /// Statistic the threshold is relative to.
    pub threshold_rel: String,
    /// Offset range (dB).
    pub threshold_offset_db: Range,
    /// Target gain reduction on peaks (dB).
    #[serde(default)]
    pub target_gr_db: Option<Range>,
    /// Ratio N:1.
    pub ratio: Range,
    /// Knee (dB).
    #[serde(default)]
    pub knee_db: Option<Range>,
    /// Attack (ms).
    pub attack_ms: Range,
    /// Release (ms).
    pub release_ms: Range,
    /// `peak` or `rms`.
    #[serde(default)]
    pub detector: Option<String>,
    /// Makeup rule (`loudness_match` or `none`).
    #[serde(default)]
    pub makeup: Option<String>,
    /// Wet/dry (%).
    #[serde(default)]
    pub mix_pct: Option<Range>,
    /// Consensus strength.
    pub confidence: Confidence,
    /// References.
    pub sources: Vec<String>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// `[[deess_preset]]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeessPreset {
    /// Unique id.
    pub id: String,
    /// `deesser`.
    pub kind: String,
    /// Centre/split frequency window (Hz).
    pub freq_hz: Range,
    /// Maximum attenuation (dB).
    pub range_db: Range,
    /// Target gain reduction on sibilants (dB).
    pub target_gr_db: Range,
    /// `split_band` or `wideband`.
    pub mode: String,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
    /// Consensus strength.
    pub confidence: Confidence,
    /// References.
    pub sources: Vec<String>,
}

/// dynamics.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DynamicsFile {
    /// Header.
    pub schema: SchemaHeader,
    /// Gate/expander presets.
    pub gate_preset: Vec<GatePreset>,
    /// Compressor presets.
    pub comp_preset: Vec<CompPreset>,
    /// De-esser presets.
    pub deess_preset: Vec<DeessPreset>,
    /// Conditional rules.
    pub rule: Vec<Rule>,
}

// ------------------------------------------------------------------------------------------------------------
// eq.toml

/// `[q_class.<name>]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QClass {
    /// Q range (RBJ bandwidth Q).
    pub q: Range,
    /// Intended use.
    #[serde(rename = "use")]
    pub use_: String,
}

/// `[limits]` of eq.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EqLimits {
    /// Max boost of a PEQ band (dB).
    pub max_boost_db_peq: f64,
    /// Max boost of a shelf (dB).
    pub max_boost_db_shelf: f64,
    /// Preferred boost window (dB).
    pub preferred_boost_db: Range,
    /// Max cut of a PEQ band (dB).
    pub max_cut_db_peq: f64,
    /// Max cut of a notch (dB).
    pub max_cut_db_notch: f64,
    /// Largest single gain move (dB).
    pub max_step_db: f64,
    /// Max summed boost per strip (dB).
    pub max_total_boost_db_per_strip: f64,
    /// Largest frequency move per step (octaves).
    pub max_freq_step_oct: f64,
    /// Max Q for a boost.
    pub boost_q_max: f64,
    /// Console gain range (dB).
    pub surface_gain_range_db: Range,
    /// References.
    pub sources: Vec<String>,
}

/// `[policy]` of eq.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EqPolicy {
    /// Try cuts before boosts.
    pub cut_before_boost: bool,
    /// Cost multiplier for boosts.
    pub boost_penalty: f64,
    /// Prefer fixing at the source.
    pub prefer_fix_at_source: bool,
    /// Prefer a filter over a shelf cut.
    pub prefer_filter_over_shelf_cut: bool,
    /// Prefer dynamic EQ for intermittent problems.
    pub prefer_dynamic_eq_when_intermittent: bool,
    /// Compare loudness-matched.
    pub evaluate_loudness_matched: bool,
    /// Small moves first.
    pub small_moves_first: bool,
    /// Order of move kinds within a strip.
    pub order_within_strip: Vec<String>,
    /// References.
    pub sources: Vec<String>,
}

/// `low_end_split` of `[masking]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LowEndSplit {
    /// Band the kick keeps (Hz).
    pub kick_owns_hz: Range,
    /// Band the bass keeps (Hz).
    pub bass_owns_hz: Range,
    /// Alternative arrangement.
    pub alternative: String,
}

/// `[masking]` of eq.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Masking {
    /// Masking (dB) above which a strip is considered masked.
    pub threshold_db: f64,
    /// Minimum time overlap (%).
    pub min_overlap_pct: f64,
    /// Source ids, highest priority first.
    pub priority_order: Vec<String>,
    /// Kick/bass split.
    pub low_end_split: LowEndSplit,
    /// References.
    pub sources: Vec<String>,
}

/// `[[masking_strategy]]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaskingStrategy {
    /// Unique id.
    pub id: String,
    /// Try lowest first.
    pub order: u32,
    /// Conditions (AND).
    #[serde(default)]
    pub when: Vec<Condition>,
    /// Feature normalisation note.
    #[serde(default)]
    pub feature_basis: Option<String>,
    /// Actions on `<yielding>` / `<keeping>` strips.
    pub actions: Vec<Action>,
    /// Relative change cost.
    pub cost: f64,
    /// Consensus strength.
    pub confidence: Confidence,
    /// References.
    pub sources: Vec<String>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// eq.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EqFile {
    /// Header.
    pub schema: SchemaHeader,
    /// Q vocabulary.
    pub q_class: BTreeMap<String, QClass>,
    /// Hard caps.
    pub limits: EqLimits,
    /// Ordering preferences.
    pub policy: EqPolicy,
    /// Mix-level tonal rules.
    pub rule: Vec<Rule>,
    /// Masking parameters.
    pub masking: Masking,
    /// Masking-resolution strategies.
    pub masking_strategy: Vec<MaskingStrategy>,
}

// ------------------------------------------------------------------------------------------------------------
// fx.toml

/// `[tempo]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Tempo {
    /// Formula text.
    pub quarter_ms: String,
    /// Formula text.
    pub eighth_ms: String,
    /// Formula text.
    pub dotted_eighth_ms: String,
    /// Formula text.
    pub sixteenth_ms: String,
    /// BPM used when none is known.
    pub fallback_bpm: f64,
    /// Pre-delay rule text.
    pub predelay_rule: String,
    /// Decay rule text.
    pub decay_rule: String,
    /// References.
    pub sources: Vec<String>,
}

impl Tempo {
    /// Quarter-note length in ms at `bpm`.
    pub fn quarter_ms_at(bpm: f64) -> f64 {
        60_000.0 / bpm
    }
}

/// `[[fx_bus]]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxBus {
    /// Unique id.
    pub id: String,
    /// Role text.
    pub role: String,
    /// Class from workflow.toml `[blocks.FxSlot].model_classes`.
    pub model_class: String,
    /// Reverb decay (s).
    #[serde(default)]
    pub decay_s: Option<Range>,
    /// Pre-delay (ms).
    #[serde(default)]
    pub predelay_ms: Option<Range>,
    /// Delay time (ms).
    #[serde(default)]
    pub time_ms: Option<Range>,
    /// Delay time in quarter notes.
    #[serde(default)]
    pub time_beats: Option<Range>,
    /// Feedback (%).
    #[serde(default)]
    pub feedback_pct: Option<Range>,
    /// Low cut (Hz).
    #[serde(default)]
    pub lo_cut_hz: Option<Range>,
    /// High cut (Hz).
    #[serde(default)]
    pub hi_cut_hz: Option<Range>,
    /// Wet mix (%).
    #[serde(default)]
    pub mix_pct: Option<f64>,
    /// Filters on the return.
    #[serde(default)]
    pub return_filter: Vec<Action>,
    /// Consensus strength.
    pub confidence: Confidence,
    /// References.
    pub sources: Vec<String>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// One send of a [`SendPreset`].
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendSpec {
    /// Target fx_bus id.
    pub fx: String,
    /// Wet contribution relative to dry (LU).
    pub wet_rel_dry_lu: Range,
    /// `pre` or `post`.
    #[serde(default)]
    pub tap: Option<String>,
}

/// `[[send_preset]]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SendPreset {
    /// Unique id.
    pub id: String,
    /// Sends.
    pub sends: Vec<SendSpec>,
    /// Consensus strength.
    pub confidence: Confidence,
    /// References.
    pub sources: Vec<String>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// One performance context modifier.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextMod {
    /// Offset applied to wet_rel_dry_lu (LU).
    pub wet_offset_lu: Range,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// `[context]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxContext {
    /// Reverberant venue.
    pub live_reverberant_venue: ContextMod,
    /// Dry venue or outdoors.
    pub live_dry_venue_or_outdoor: ContextMod,
    /// Broadcast or recording tap.
    pub broadcast_or_recording_tap: ContextMod,
    /// References.
    pub sources: Vec<String>,
}

/// fx.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FxFile {
    /// Header.
    pub schema: SchemaHeader,
    /// Tempo formulas.
    pub tempo: Tempo,
    /// Standard FX rack.
    pub fx_bus: Vec<FxBus>,
    /// Per-source send presets.
    pub send_preset: Vec<SendPreset>,
    /// Context modifiers.
    pub context: FxContext,
    /// Depth rules.
    pub rule: Vec<Rule>,
}

// ------------------------------------------------------------------------------------------------------------
// vocabulary.toml

/// Term kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermKind {
    /// Spectral balance.
    Tonal,
    /// Dynamics.
    Dynamics,
    /// Stereo/space.
    Spatial,
    /// Level balance.
    Balance,
    /// Effects.
    Fx,
    /// Loudness.
    Loudness,
    /// Character/texture.
    Character,
}

/// Whether the term names something wanted or a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Polarity {
    /// Deltas apply as written.
    Desirable,
    /// A fault: deltas apply negated.
    Problem,
    /// Deltas apply as written.
    Neutral,
}

/// `{ feature, delta = [lo, hi] }`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Delta {
    /// Target feature (with optional band/scope suffixes).
    pub feature: String,
    /// Shift range.
    pub delta: Range,
}

/// `[[term]]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Term {
    /// Canonical lowercase id.
    pub id: String,
    /// Other words resolving to this term.
    #[serde(default)]
    pub synonyms: Vec<String>,
    /// Roughly opposite term id.
    #[serde(default)]
    pub antonym: Option<String>,
    /// Kind.
    pub kind: TermKind,
    /// Polarity.
    pub polarity: Polarity,
    /// `mix`, a source id or a family.
    pub default_scope: String,
    /// May be qualified by a source/family.
    pub scopable: bool,
    /// Target shifts.
    #[serde(default)]
    pub deltas: Vec<Delta>,
    /// Blocks that realise the term.
    #[serde(default)]
    pub blocks_hint: Vec<String>,
    /// Terms that contradict this one.
    #[serde(default)]
    pub conflicts: Vec<String>,
    /// Agreement on the meaning.
    pub confidence: Confidence,
    /// References.
    pub sources: Vec<String>,
    /// Free text.
    #[serde(default)]
    pub note: Option<String>,
}

/// `[modifiers.negation]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Negation {
    /// Negation words.
    pub words: Vec<String>,
    /// Multiplier (-1).
    pub factor: f64,
    /// Free text.
    pub note: String,
}

/// `[modifiers.comparative]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Comparative {
    /// Comparative markers (`-er` is a suffix).
    pub words: Vec<String>,
    /// Free text.
    pub note: String,
}

/// `[modifiers]`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Modifiers {
    /// Intensity word -> multiplier; `""` is the unmodified default.
    pub intensity: BTreeMap<String, f64>,
    /// Negation words.
    pub negation: Negation,
    /// Comparatives.
    pub comparative: Comparative,
}

/// `[limits]` of vocabulary.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VocabLimits {
    /// Max |band delta| (dB).
    pub max_band_delta_db: f64,
    /// Max |level delta| (LU).
    pub max_level_delta_lu: f64,
    /// Max |width delta| (dB).
    pub max_width_delta_db: f64,
    /// Max |reverb delta| (dB).
    pub max_reverb_delta_db: f64,
    /// Max |crest delta| (dB).
    pub max_crest_delta_db: f64,
    /// Free text.
    pub note: String,
}

/// vocabulary.toml.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VocabularyFile {
    /// Header.
    pub schema: SchemaHeader,
    /// Modifier words.
    pub modifiers: Modifiers,
    /// Delta clamps.
    pub limits: VocabLimits,
    /// Terms.
    pub term: Vec<Term>,
}

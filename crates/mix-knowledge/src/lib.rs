//! mix-knowledge: the mixing knowledge base (`data/knowledge/*.toml`) with typed access.
//!
//! * [`types`]: serde structs for every file (no untyped TOML values in the public API).
//! * [`KnowledgeBase::load`] reads a directory, [`KnowledgeBase::embedded`] returns the copy compiled into the
//!   binary (`include_str!`); both parse **and** run the consistency [`check`].
//! * Queries: [`KnowledgeBase::source_by_name`] (strip name hints), [`KnowledgeBase::presets_for`] (gate/dyn/de-ess/fx
//!   presets of a source), [`KnowledgeBase::rules_by_feature`] / [`KnowledgeBase::problems_by_feature`] (rules whose
//!   conditions mention a feature), [`KnowledgeBase::resolve_phrase`] (vocabulary lookup with modifiers).
//! * [`check::check`]: cross-references resolve, ranges are ordered, units/blocks/features/bands are known.
//!
//! Rationale for every value lives in `docs/research/mix-knowledge.md`; the grammar in `workflow.toml [schema]`.

pub mod check;
pub mod types;

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

pub use types::*;

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-knowledge";

/// The six knowledge files as text, in the order workflow, sources, dynamics, eq, fx, vocabulary.
#[derive(Debug, Clone, Copy)]
pub struct RawFiles<'a> {
    /// workflow.toml
    pub workflow: &'a str,
    /// sources.toml
    pub sources: &'a str,
    /// dynamics.toml
    pub dynamics: &'a str,
    /// eq.toml
    pub eq: &'a str,
    /// fx.toml
    pub fx: &'a str,
    /// vocabulary.toml
    pub vocabulary: &'a str,
}

/// The knowledge files compiled into the crate.
pub const EMBEDDED: RawFiles<'static> = RawFiles {
    workflow: include_str!("../../../data/knowledge/workflow.toml"),
    sources: include_str!("../../../data/knowledge/sources.toml"),
    dynamics: include_str!("../../../data/knowledge/dynamics.toml"),
    eq: include_str!("../../../data/knowledge/eq.toml"),
    fx: include_str!("../../../data/knowledge/fx.toml"),
    vocabulary: include_str!("../../../data/knowledge/vocabulary.toml"),
};

/// Why a knowledge base could not be loaded.
#[derive(Debug)]
pub enum KnowledgeError {
    /// A file could not be read.
    Io {
        /// File path.
        path: PathBuf,
        /// Underlying error.
        source: std::io::Error,
    },
    /// A file is not valid TOML or does not match the typed schema (unknown field, wrong type, missing field).
    Parse {
        /// File name (`sources.toml`).
        file: String,
        /// Parser message (includes line/column and the field name).
        message: String,
    },
    /// Everything parsed but cross-references/ranges/units are inconsistent.
    Inconsistent(Vec<String>),
}

impl fmt::Display for KnowledgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KnowledgeError::Io { path, source } => write!(f, "cannot read {}: {source}", path.display()),
            KnowledgeError::Parse { file, message } => write!(f, "{file}: {message}"),
            KnowledgeError::Inconsistent(p) => {
                write!(f, "knowledge base is inconsistent ({} problems):", p.len())?;
                for line in p {
                    write!(f, "\n  - {line}")?;
                }
                Ok(())
            }
        }
    }
}

impl std::error::Error for KnowledgeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            KnowledgeError::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Which file a [`RuleRef`] comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleFile {
    /// dynamics.toml
    Dynamics,
    /// eq.toml
    Eq,
    /// fx.toml
    Fx,
}

/// A rule together with its file.
#[derive(Debug, Clone, Copy)]
pub struct RuleRef<'a> {
    /// Origin file.
    pub file: RuleFile,
    /// The rule.
    pub rule: &'a Rule,
}

/// The presets a source type points at (`None` where the source says `"none"`).
#[derive(Debug, Clone, Copy, Default)]
pub struct SourcePresets<'a> {
    /// Gate/expander preset.
    pub gate: Option<&'a GatePreset>,
    /// Compressor preset.
    pub comp: Option<&'a CompPreset>,
    /// De-esser preset.
    pub deess: Option<&'a DeessPreset>,
    /// FX send preset.
    pub send: Option<&'a SendPreset>,
}

/// A vocabulary phrase resolved to a term plus modifiers (see [`KnowledgeBase::resolve_phrase`]).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedTerm<'a> {
    /// The matched term.
    pub term: &'a Term,
    /// Intensity multiplier from words such as "very" (1.0 when unmodified).
    pub intensity: f64,
    /// A negation word ("not", "less", "too", ...) preceded the term.
    pub negated: bool,
    /// Source id or family named elsewhere in the phrase ("warm bass"), only for scopable terms; otherwise the
    /// term's `default_scope` applies.
    pub scope: Option<String>,
}

/// One feature shift of a resolved term, after intensity, sign and clamping.
#[derive(Debug, Clone, PartialEq)]
pub struct ScaledDelta {
    /// Feature (with band/scope suffixes as in the data file).
    pub feature: String,
    /// Shift of the TargetProfile value, in the feature's unit.
    pub delta: f64,
}

impl ResolvedTerm<'_> {
    /// +1 when the deltas apply as written, -1 when negated. Problem-polarity terms describe a fault, so their
    /// deltas are always negated ("muddy", "too muddy" and "less muddy" all mean: reduce the muddiness); desirable
    /// and neutral terms are negated only by an explicit negation word ("less warm").
    pub fn sign(&self) -> f64 {
        if self.negated || self.term.polarity == Polarity::Problem {
            -1.0
        } else {
            1.0
        }
    }
}

/// The whole knowledge base.
#[derive(Debug, Clone)]
pub struct KnowledgeBase {
    /// workflow.toml
    pub workflow: Workflow,
    /// sources.toml
    pub sources: SourcesFile,
    /// dynamics.toml
    pub dynamics: DynamicsFile,
    /// eq.toml
    pub eq: EqFile,
    /// fx.toml
    pub fx: FxFile,
    /// vocabulary.toml
    pub vocabulary: VocabularyFile,
    /// normalised word/phrase -> index into `vocabulary.term`
    word_index: HashMap<String, usize>,
}

fn parse<T: serde::de::DeserializeOwned>(file: &str, text: &str) -> Result<T, KnowledgeError> {
    toml::from_str(text).map_err(|e| KnowledgeError::Parse {
        file: file.to_string(),
        message: e.to_string(),
    })
}

/// Lowercase, hyphens/underscores to spaces, whitespace collapsed (vocabulary words are hyphen/space-insensitive).
pub(crate) fn normalize_word(w: &str) -> String {
    w.to_lowercase()
        .replace(['-', '_'], " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

impl KnowledgeBase {
    /// Parse the six files without running the consistency checker (use [`check::check`] afterwards).
    pub fn parse_unchecked(raw: &RawFiles<'_>) -> Result<Self, KnowledgeError> {
        let vocabulary: VocabularyFile = parse("vocabulary.toml", raw.vocabulary)?;
        let mut word_index = HashMap::new();
        for (i, t) in vocabulary.term.iter().enumerate() {
            for w in std::iter::once(&t.id).chain(t.synonyms.iter()) {
                word_index.entry(normalize_word(w)).or_insert(i);
            }
        }
        Ok(Self {
            workflow: parse("workflow.toml", raw.workflow)?,
            sources: parse("sources.toml", raw.sources)?,
            dynamics: parse("dynamics.toml", raw.dynamics)?,
            eq: parse("eq.toml", raw.eq)?,
            fx: parse("fx.toml", raw.fx)?,
            vocabulary,
            word_index,
        })
    }

    /// Parse the six files and run the consistency checker.
    pub fn from_raw(raw: &RawFiles<'_>) -> Result<Self, KnowledgeError> {
        let kb = Self::parse_unchecked(raw)?;
        let problems = check::check(&kb);
        if problems.is_empty() {
            Ok(kb)
        } else {
            Err(KnowledgeError::Inconsistent(problems))
        }
    }

    /// Load and check `workflow.toml`, `sources.toml`, `dynamics.toml`, `eq.toml`, `fx.toml`, `vocabulary.toml` from
    /// `dir` (normally `data/knowledge`).
    pub fn load(dir: impl AsRef<Path>) -> Result<Self, KnowledgeError> {
        let dir = dir.as_ref();
        let read = |name: &str| {
            let path = dir.join(name);
            std::fs::read_to_string(&path).map_err(|source| KnowledgeError::Io { path, source })
        };
        let (workflow, sources, dynamics, eq, fx, vocabulary) = (
            read("workflow.toml")?,
            read("sources.toml")?,
            read("dynamics.toml")?,
            read("eq.toml")?,
            read("fx.toml")?,
            read("vocabulary.toml")?,
        );
        Self::from_raw(&RawFiles {
            workflow: &workflow,
            sources: &sources,
            dynamics: &dynamics,
            eq: &eq,
            fx: &fx,
            vocabulary: &vocabulary,
        })
    }

    /// The knowledge base compiled into the binary, parsed and checked once.
    ///
    /// # Panics
    /// If the embedded data is invalid (the crate's tests guarantee it is not).
    pub fn embedded() -> &'static KnowledgeBase {
        static KB: OnceLock<KnowledgeBase> = OnceLock::new();
        KB.get_or_init(|| Self::from_raw(&EMBEDDED).unwrap_or_else(|e| panic!("embedded knowledge base is invalid: {e}")))
    }

    // ---------------------------------------------------------------------------------------------------------
    // Source queries

    /// A source type by exact id.
    pub fn source(&self, id: &str) -> Option<&Source> {
        self.sources.source.iter().find(|s| s.id == id)
    }

    /// Source types of a family.
    pub fn sources_in_family(&self, family: Family) -> impl Iterator<Item = &Source> {
        self.sources.source.iter().filter(move |s| s.family == family)
    }

    /// Every source type whose name hints match a strip name, best first. The score is the longest matched hint
    /// (hints are case-insensitive substrings where `x?` makes `x` optional), with a bonus for whole-word matches
    /// and for the source id itself.
    pub fn match_sources(&self, strip_name: &str) -> Vec<(&Source, u32)> {
        let name = strip_name.to_lowercase().replace('_', " ");
        let chars: Vec<char> = name.chars().collect();
        let norm_name = normalize_word(strip_name);
        let mut out: Vec<(&Source, u32)> = Vec::new();
        for s in &self.sources.source {
            let mut best = 0u32;
            if !norm_name.is_empty() && normalize_word(&s.id) == norm_name {
                best = 1000;
            }
            for h in &s.name_hints {
                let pat = compile_hint(h);
                for start in 0..chars.len() {
                    if let Some(len) = match_hint(&pat, &chars, start) {
                        if len == 0 {
                            continue;
                        }
                        let word_start = start == 0 || !chars[start - 1].is_alphanumeric();
                        let word_end = start + len == chars.len() || !chars[start + len].is_alphanumeric();
                        let score = len as u32 * 10 + if word_start && word_end { 5 } else { 0 };
                        best = best.max(score);
                    }
                }
            }
            if best > 0 {
                out.push((s, best));
            }
        }
        out.sort_by(|a, b| b.1.cmp(&a.1));
        out
    }

    /// Best source type for a strip name ("Kick In", "LEAD VOX", "bass_di"), if any hint matches.
    pub fn source_by_name(&self, strip_name: &str) -> Option<&Source> {
        self.match_sources(strip_name).first().map(|(s, _)| *s)
    }

    /// Gate/expander preset by id.
    pub fn gate_preset(&self, id: &str) -> Option<&GatePreset> {
        self.dynamics.gate_preset.iter().find(|p| p.id == id)
    }
    /// Compressor preset by id.
    pub fn comp_preset(&self, id: &str) -> Option<&CompPreset> {
        self.dynamics.comp_preset.iter().find(|p| p.id == id)
    }
    /// De-esser preset by id.
    pub fn deess_preset(&self, id: &str) -> Option<&DeessPreset> {
        self.dynamics.deess_preset.iter().find(|p| p.id == id)
    }
    /// FX send preset by id.
    pub fn send_preset(&self, id: &str) -> Option<&SendPreset> {
        self.fx.send_preset.iter().find(|p| p.id == id)
    }
    /// FX bus by id.
    pub fn fx_bus(&self, id: &str) -> Option<&FxBus> {
        self.fx.fx_bus.iter().find(|p| p.id == id)
    }

    /// The presets a source type references.
    pub fn presets_for<'a>(&'a self, source: &Source) -> SourcePresets<'a> {
        SourcePresets {
            gate: self.gate_preset(&source.gate_preset),
            comp: self.comp_preset(&source.dyn_preset),
            deess: source.deess_preset.as_deref().and_then(|id| self.deess_preset(id)),
            send: self.send_preset(&source.fx_preset),
        }
    }

    /// The presets of the source type with this id.
    pub fn presets_by_source(&self, source_id: &str) -> Option<SourcePresets<'_>> {
        self.source(source_id).map(|s| self.presets_for(s))
    }

    // ---------------------------------------------------------------------------------------------------------
    // Rule queries

    /// All rules (dynamics, eq, fx) in file order.
    pub fn all_rules(&self) -> impl Iterator<Item = RuleRef<'_>> {
        let d = self.dynamics.rule.iter().map(|rule| RuleRef { file: RuleFile::Dynamics, rule });
        let e = self.eq.rule.iter().map(|rule| RuleRef { file: RuleFile::Eq, rule });
        let f = self.fx.rule.iter().map(|rule| RuleRef { file: RuleFile::Fx, rule });
        d.chain(e).chain(f)
    }

    /// Rules with a condition on `feature`. `crest_db` matches `crest_db`; `band_dev_db` matches every
    /// `band_dev_db.<band>`; `band_dev_db.low_mid` matches only that band (and unsuffixed uses).
    pub fn rules_by_feature<'a>(&'a self, feature: &str) -> Vec<RuleRef<'a>> {
        self.all_rules()
            .filter(|r| r.rule.conditions().any(|c| feature_matches(&c.feature, feature)))
            .collect()
    }

    /// Rules that apply to a source (by id, its family, `"*"`).
    pub fn rules_for_source<'a>(&'a self, source: &Source) -> Vec<RuleRef<'a>> {
        self.all_rules()
            .filter(|r| {
                r.rule
                    .applies_to
                    .iter()
                    .any(|a| a == "*" || a == &source.id || a == source.family.as_str())
            })
            .collect()
    }

    /// Source-specific problems with a condition on `feature` (same matching as [`Self::rules_by_feature`]).
    pub fn problems_by_feature<'a>(&'a self, feature: &str) -> Vec<(&'a Source, &'a SourceProblem)> {
        let mut out = Vec::new();
        for s in &self.sources.source {
            for p in &s.problems {
                if p.when.iter().chain(p.when_any.iter()).any(|c| feature_matches(&c.feature, feature)) {
                    out.push((s, p));
                }
            }
        }
        out
    }

    /// Masking strategies in the order the planner should try them.
    pub fn masking_strategies(&self) -> Vec<&MaskingStrategy> {
        let mut v: Vec<&MaskingStrategy> = self.eq.masking_strategy.iter().collect();
        v.sort_by_key(|m| m.order);
        v
    }

    /// Which of two sources keeps its band when they mask each other (earlier in `priority_order`); `None` when
    /// either is not in the list.
    pub fn masking_winner<'a>(&self, a: &'a str, b: &'a str) -> Option<&'a str> {
        let pos = |id: &str| self.eq.masking.priority_order.iter().position(|x| x == id);
        match (pos(a), pos(b)) {
            (Some(x), Some(y)) if x != y => Some(if x < y { a } else { b }),
            _ => None,
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Vocabulary

    /// Term by id or synonym (case, hyphen and space insensitive): `"Warmer"`, `"hi-hat"`.
    pub fn term(&self, word: &str) -> Option<&Term> {
        self.word_index.get(&normalize_word(word)).map(|&i| &self.vocabulary.term[i])
    }

    /// Resolve a description fragment ("very warm", "a bit less muddy", "punchy drums", "too bright"): the longest
    /// word/phrase that names a term, an intensity modifier before it, a negation word before it, and a source/family
    /// qualifier for scopable terms. `None` when no term is found.
    pub fn resolve_phrase(&self, phrase: &str) -> Option<ResolvedTerm<'_>> {
        let norm = normalize_word(phrase);
        let tokens: Vec<&str> = norm.split(' ').filter(|t| !t.is_empty()).collect();
        // longest n-gram that is a term word; earliest wins on equal length
        let mut found: Option<(usize, usize, usize)> = None; // (start, len, term idx)
        for len in (1..=tokens.len()).rev() {
            for start in 0..=tokens.len() - len {
                if let Some(&i) = self.word_index.get(&tokens[start..start + len].join(" ")) {
                    found = Some((start, len, i));
                    break;
                }
            }
            if found.is_some() {
                break;
            }
        }
        let (start, len, idx) = found?;
        let term = &self.vocabulary.term[idx];
        let before = &tokens[..start];
        let after = &tokens[start + len..];

        let m = &self.vocabulary.modifiers;
        let negated = before.iter().any(|t| m.negation.words.iter().any(|w| w == t));
        let mut intensity = m.intensity.get("").copied().unwrap_or(1.0);
        let mut best_len = 0;
        for (word, factor) in &m.intensity {
            let w = normalize_word(word);
            if w.is_empty() {
                continue;
            }
            let wt: Vec<&str> = w.split(' ').collect();
            if wt.len() >= best_len && before.windows(wt.len()).any(|win| win == wt.as_slice()) {
                best_len = wt.len();
                intensity = *factor;
            }
        }
        let scope = if term.scopable {
            before
                .iter()
                .chain(after.iter())
                .find_map(|t| self.scope_word(t))
        } else {
            None
        };
        Some(ResolvedTerm {
            term,
            intensity,
            negated,
            scope,
        })
    }

    fn scope_word(&self, token: &str) -> Option<String> {
        let t = token.replace(' ', "_");
        if self.source(&t).is_some() || self.sources.source.iter().any(|s| s.family.as_str() == t) {
            return Some(t);
        }
        // plural/loose forms: "drums", "vocals", "guitars"
        let singular = t.strip_suffix('s').unwrap_or(&t).to_string();
        if self.source(&singular).is_some() || self.sources.source.iter().any(|s| s.family.as_str() == singular) {
            return Some(singular);
        }
        None
    }

    /// The feature shifts of a resolved term: the midpoint of each delta range times intensity times
    /// [`ResolvedTerm::sign`], clamped by vocabulary.toml `[limits]` (so "very warm, very dark, very full" stays sane).
    pub fn scaled_deltas(&self, r: &ResolvedTerm<'_>) -> Vec<ScaledDelta> {
        let l = &self.vocabulary.limits;
        r.term
            .deltas
            .iter()
            .map(|d| {
                let raw = d.delta.mid() * r.intensity * r.sign();
                let base = d.feature.split('.').next().unwrap_or(&d.feature);
                let cap = match base {
                    "band_dev_db" | "tilt_dev_db_per_oct" => Some(l.max_band_delta_db),
                    "level_dev_lu" => Some(l.max_level_delta_lu),
                    "width_dev_db" | "side_to_mid_db" => Some(l.max_width_delta_db),
                    "reverb_rel_dev_db" | "reverb_rel_db" | "wet_rel_dry_lu" => Some(l.max_reverb_delta_db),
                    "crest_db" => Some(l.max_crest_delta_db),
                    _ => None,
                };
                ScaledDelta {
                    feature: d.feature.clone(),
                    delta: cap.map_or(raw, |c| raw.clamp(-c, c)),
                }
            })
            .collect()
    }

    /// The workflow phases in order.
    pub fn phases(&self) -> Vec<&Phase> {
        let mut v: Vec<&Phase> = self.workflow.phase.iter().collect();
        v.sort_by_key(|p| p.order);
        v
    }

    /// A band's frequency edges by name.
    pub fn band(&self, name: &str) -> Option<Range> {
        self.workflow.bands.get(name).copied()
    }
}

/// Does a condition's feature (`band_dev_db.low_mid`) match a query (`band_dev_db` or `band_dev_db.low_mid`)?
fn feature_matches(cond_feature: &str, query: &str) -> bool {
    cond_feature == query
        || cond_feature.strip_prefix(query).is_some_and(|rest| rest.starts_with('.'))
        || query.strip_prefix(cond_feature).is_some_and(|rest| rest.starts_with('.'))
}

/// A name hint compiled to `(char, optional)` pairs (`?` after a char makes it optional).
fn compile_hint(h: &str) -> Vec<(char, bool)> {
    let chars: Vec<char> = h.to_lowercase().chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '?' {
            i += 1;
            continue;
        }
        let opt = chars.get(i + 1) == Some(&'?');
        out.push((chars[i], opt));
        i += 1;
    }
    out
}

/// Longest match of `pat` against `text` starting at `pos` (length in text chars), `None` if it does not match.
fn match_hint(pat: &[(char, bool)], text: &[char], pos: usize) -> Option<usize> {
    match pat.split_first() {
        None => Some(0),
        Some((&(c, opt), rest)) => {
            let mut best = None;
            if text.get(pos) == Some(&c) {
                if let Some(n) = match_hint(rest, text, pos + 1) {
                    best = Some(n + 1);
                }
            }
            if opt {
                if let Some(n) = match_hint(rest, text, pos) {
                    best = Some(best.map_or(n, |b: usize| b.max(n)));
                }
            }
            best
        }
    }
}

#[cfg(test)]
mod tests;

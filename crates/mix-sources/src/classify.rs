//! Combining name/tag/icon/colour heuristics with feature evidence into a ranked, explained verdict, plus overrides.
//!
//! Scoring (per source type, all in 0..1):
//! - *label score* `L`: the best of the name rule match, tag match (×0.9), icon label match (×0.8), knowledge-base
//!   hint fallback (≤ 0.5) and colour consensus (≤ [`COLOUR_PRIOR`]).
//! - *feature score* `F = 0.8 · p(class)` from [`crate::features`] when a capture is available and the type has
//!   a feature class (the nearest prototype's own type gets +0.05 so features alone still pick a type).
//! - combined: noisy-OR `1 − (1 − L)(1 − F)`. If features confidently contradict a label (`p(class) < 0.05`) the
//!   label's score is scaled by 0.85; types features cannot judge (playback, click...) keep `L` × 0.95 when a
//!   capture exists.
//!
//! A user override always wins with confidence 1.

use std::collections::{BTreeMap, HashMap};

use mix_core::Features;
use mix_knowledge::KnowledgeBase;
use mix_surface::{Strip, StripId};
use serde::{Deserialize, Serialize};

use crate::descriptor::Descriptor;
use crate::features::{abstain_reason, classify_descriptor, FeatureVerdict};
use crate::names::{match_kb_hints, match_name};
use crate::types::{FeatureClass, SourceType};

/// Ranked results below this confidence are dropped.
pub const MIN_CANDIDATE: f32 = 0.05;
/// [`Classification::source`] returns `None` below this confidence.
pub const MIN_CONFIDENCE: f32 = 0.35;
/// Colour consensus prior for the types of the family shared by same-coloured named strips.
pub const COLOUR_PRIOR: f32 = 0.3;

/// What is known about one strip.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct StripInput {
    pub id: Option<StripId>,
    pub name: String,
    /// Console colour index (WING: 1..12), if known.
    pub colour: Option<u32>,
    /// Icon label, if the adapter maps the console's icon to a word ("kick", "guitar"...).
    pub icon: Option<String>,
    pub tags: Vec<String>,
    /// Descriptor of a capture (solo or channel tap), if any.
    pub descriptor: Option<Descriptor>,
}

impl StripInput {
    pub fn named(name: impl Into<String>) -> Self {
        StripInput {
            name: name.into(),
            ..Default::default()
        }
    }

    /// From a surface strip (name + tags).
    pub fn from_strip(s: &Strip) -> Self {
        StripInput {
            id: Some(s.id.clone()),
            name: s.name.clone(),
            tags: s.tags.clone(),
            ..Default::default()
        }
    }

    pub fn with_features(mut self, f: &Features) -> Self {
        self.descriptor = Some(Descriptor::from_features(f));
        self
    }

    pub fn with_descriptor(mut self, d: Descriptor) -> Self {
        self.descriptor = Some(d);
        self
    }

    pub fn with_colour(mut self, c: u32) -> Self {
        self.colour = Some(c);
        self
    }
}

/// One piece of evidence for a candidate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Evidence {
    /// Name rule matched `matched` (tokens).
    Name { matched: String, score: f32, alternative: bool },
    Tag { tag: String, matched: String, score: f32 },
    Icon { icon: String, score: f32 },
    /// Knowledge-base `name_hints` fallback.
    KbHint { source_id: String, score: f32 },
    /// Same-coloured strips are mostly this family.
    Colour { colour: u32, family: String, strips: usize, score: f32 },
    /// Feature classifier result for the candidate's class.
    Features { class: FeatureClass, prob: f32, prototype: String, distance: f32, notes: Vec<String> },
    /// Features confidently point elsewhere.
    FeaturesDisagree { best: FeatureClass, prob: f32 },
    /// Features had too little signal.
    NoSignal { reason: String },
    Override,
}

/// A ranked candidate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    pub source: SourceType,
    pub confidence: f32,
    pub evidence: Vec<Evidence>,
}

/// Verdict for one strip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Classification {
    pub id: Option<StripId>,
    /// Best first; empty when nothing is known.
    pub ranked: Vec<Candidate>,
    pub overridden: bool,
    /// Feature classifier output (None without a capture or when it abstained).
    pub features: Option<FeatureVerdict>,
}

impl Classification {
    pub fn best(&self) -> Option<&Candidate> {
        self.ranked.first()
    }

    /// The best type if it is at least [`MIN_CONFIDENCE`].
    pub fn source(&self) -> Option<SourceType> {
        self.best()
            .filter(|c| c.confidence >= MIN_CONFIDENCE)
            .map(|c| c.source)
    }

    pub fn confidence(&self) -> f32 {
        self.best().map_or(0.0, |c| c.confidence)
    }

    /// The knowledge-base entry of the best type (for preset/rule lookups).
    pub fn kb_source<'a>(&self, kb: &'a KnowledgeBase) -> Option<&'a mix_knowledge::Source> {
        kb.source(self.source()?.kb_id()?)
    }
}

/// Classifier with user overrides. Cheap to build; holds a reference to the knowledge base.
#[derive(Clone, Debug)]
pub struct Identifier<'kb> {
    kb: &'kb KnowledgeBase,
    overrides: BTreeMap<StripId, SourceType>,
}

impl Default for Identifier<'static> {
    fn default() -> Self {
        Identifier::new(KnowledgeBase::embedded())
    }
}

#[derive(Default)]
struct Acc {
    label: f32,
    evidence: Vec<Evidence>,
}

impl<'kb> Identifier<'kb> {
    pub fn new(kb: &'kb KnowledgeBase) -> Self {
        Identifier {
            kb,
            overrides: BTreeMap::new(),
        }
    }

    /// Set (`Some`) or clear (`None`) the user's override for a strip.
    pub fn set_override(&mut self, id: StripId, source: Option<SourceType>) {
        match source {
            Some(s) => {
                self.overrides.insert(id, s);
            }
            None => {
                self.overrides.remove(&id);
            }
        }
    }

    pub fn override_of(&self, id: &StripId) -> Option<SourceType> {
        self.overrides.get(id).copied()
    }

    /// All overrides (serialisable for persistence).
    pub fn overrides(&self) -> &BTreeMap<StripId, SourceType> {
        &self.overrides
    }

    /// Replace all overrides (e.g. loaded from a session file).
    pub fn load_overrides(&mut self, o: BTreeMap<StripId, SourceType>) {
        self.overrides = o;
    }

    /// Classify one strip on its own.
    pub fn classify(&self, s: &StripInput) -> Classification {
        self.classify_with_colour(s, None)
    }

    /// Classify a set of strips; strips without label evidence also get a colour-consensus prior from named
    /// strips of the same colour.
    pub fn classify_all(&self, strips: &[StripInput]) -> Vec<Classification> {
        // colour -> family -> count, from strips with a confident label.
        let mut by_colour: HashMap<u32, HashMap<&'static str, usize>> = HashMap::new();
        for s in strips {
            let (Some(c), Some(t)) = (s.colour, self.label_best(s)) else {
                continue;
            };
            *by_colour.entry(c).or_default().entry(t.family()).or_default() += 1;
        }
        strips
            .iter()
            .map(|s| {
                let prior = s.colour.and_then(|c| {
                    let fams = by_colour.get(&c)?;
                    let total: usize = fams.values().sum();
                    let (fam, n) = fams.iter().max_by_key(|(f, n)| (**n, std::cmp::Reverse(**f)))?;
                    // Need two agreeing strips and a clear majority.
                    (*n >= 2 && *n * 3 >= total * 2).then(|| (c, *fam, *n))
                });
                self.classify_with_colour(s, prior)
            })
            .collect()
    }

    fn label_best(&self, s: &StripInput) -> Option<SourceType> {
        let acc = self.labels(s);
        acc.into_iter()
            .filter(|(_, a)| a.label >= 0.6)
            .max_by(|a, b| a.1.label.total_cmp(&b.1.label))
            .map(|(t, _)| t)
    }

    fn labels(&self, s: &StripInput) -> BTreeMap<SourceType, Acc> {
        let mut acc: BTreeMap<SourceType, Acc> = BTreeMap::new();
        let mut add = |t: SourceType, score: f32, e: Evidence| {
            let a = acc.entry(t).or_default();
            a.label = a.label.max(score);
            a.evidence.push(e);
        };
        let name_matches = match_name(&s.name);
        for m in &name_matches {
            add(
                m.source,
                m.confidence,
                Evidence::Name {
                    matched: m.matched.clone(),
                    score: m.confidence,
                    alternative: m.alternative,
                },
            );
        }
        for tag in &s.tags {
            for m in match_name(tag) {
                let score = m.confidence * 0.9;
                add(
                    m.source,
                    score,
                    Evidence::Tag {
                        tag: tag.clone(),
                        matched: m.matched,
                        score,
                    },
                );
            }
        }
        if let Some(icon) = &s.icon {
            for m in match_name(icon) {
                let score = m.confidence * 0.8;
                add(
                    m.source,
                    score,
                    Evidence::Icon {
                        icon: icon.clone(),
                        score,
                    },
                );
            }
        }
        if name_matches.is_empty() && !s.name.trim().is_empty() {
            for m in match_kb_hints(self.kb, &s.name) {
                add(
                    m.source,
                    m.confidence,
                    Evidence::KbHint {
                        source_id: m.matched,
                        score: m.confidence,
                    },
                );
            }
        }
        acc
    }

    fn classify_with_colour(
        &self,
        s: &StripInput,
        colour: Option<(u32, &'static str, usize)>,
    ) -> Classification {
        let verdict = s.descriptor.as_ref().and_then(classify_descriptor);
        if let Some(t) = s.id.as_ref().and_then(|id| self.override_of(id)) {
            return Classification {
                id: s.id.clone(),
                ranked: vec![Candidate {
                    source: t,
                    confidence: 1.0,
                    evidence: vec![Evidence::Override],
                }],
                overridden: true,
                features: verdict,
            };
        }
        let mut acc = self.labels(s);
        if acc.is_empty() {
            if let Some((c, fam, n)) = colour {
                for t in SourceType::ALL.into_iter().filter(|t| t.family() == fam) {
                    let a = acc.entry(t).or_default();
                    a.label = a.label.max(COLOUR_PRIOR);
                    a.evidence.push(Evidence::Colour {
                        colour: c,
                        family: fam.to_string(),
                        strips: n,
                        score: COLOUR_PRIOR,
                    });
                }
            }
        }
        let no_signal = s
            .descriptor
            .as_ref()
            .and_then(abstain_reason)
            .map(|reason| Evidence::NoSignal { reason });

        let mut ranked: Vec<Candidate> = Vec::new();
        match &verdict {
            None => {
                for (t, a) in acc {
                    let mut evidence = a.evidence;
                    evidence.extend(no_signal.clone());
                    ranked.push(Candidate {
                        source: t,
                        confidence: a.label,
                        evidence,
                    });
                }
            }
            Some(v) => {
                let best_class = v.best();
                for t in SourceType::ALL {
                    let a = acc.remove(&t).unwrap_or_default();
                    let mut evidence = a.evidence;
                    let conf = match t.feature_class() {
                        Some(c) => {
                            let prob = v.prob(c);
                            let (proto, _, dist) = &v.nearest[c.idx()];
                            let bonus = if v.source_of(c) == t { 0.05 } else { 0.0 };
                            let f = (0.8 * prob + bonus).min(0.95);
                            let mut label = a.label;
                            if label > 0.0 && prob < 0.05 {
                                label *= 0.85;
                                evidence.push(Evidence::FeaturesDisagree {
                                    best: best_class,
                                    prob: v.prob(best_class),
                                });
                            }
                            if prob >= 0.05 {
                                evidence.push(Evidence::Features {
                                    class: c,
                                    prob,
                                    prototype: proto.clone(),
                                    distance: *dist,
                                    notes: if c == best_class {
                                        v.notes.clone()
                                    } else {
                                        Vec::new()
                                    },
                                });
                            }
                            1.0 - (1.0 - label) * (1.0 - f)
                        }
                        None => a.label * 0.95,
                    };
                    ranked.push(Candidate {
                        source: t,
                        confidence: conf,
                        evidence,
                    });
                }
            }
        }
        ranked.retain(|c| c.confidence >= MIN_CANDIDATE);
        ranked.sort_by(|a, b| {
            b.confidence
                .total_cmp(&a.confidence)
                .then_with(|| a.source.cmp(&b.source))
        });
        Classification {
            id: s.id.clone(),
            ranked,
            overridden: false,
            features: verdict,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::PROTOTYPES;

    fn desc(name: &str) -> Descriptor {
        let p = PROTOTYPES.iter().find(|p| p.name == name).unwrap();
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
    fn name_only() {
        let id = Identifier::default();
        let c = id.classify(&StripInput::named("KICK IN"));
        assert_eq!(c.source(), Some(SourceType::Kick));
        assert!(matches!(c.best().unwrap().evidence[0], Evidence::Name { .. }));
        assert_eq!(c.kb_source(KnowledgeBase::embedded()).unwrap().id, "kick");
        assert!(id.classify(&StripInput::named("Ch 28")).ranked.is_empty());
    }

    #[test]
    fn features_confirm_and_raise_confidence() {
        let id = Identifier::default();
        let name_only = id.classify(&StripInput::named("VOX 1"));
        let both = id.classify(&StripInput::named("VOX 1").with_descriptor(desc("vocal")));
        assert_eq!(both.source(), Some(SourceType::LeadVocal));
        assert!(both.confidence() > name_only.confidence());
        assert!(both.best().unwrap().evidence.iter().any(|e| matches!(e, Evidence::Features { .. })));
    }

    #[test]
    fn features_alone_pick_a_type() {
        let id = Identifier::default();
        let c = id.classify(&StripInput::named("").with_descriptor(desc("kick")));
        assert_eq!(c.source(), Some(SourceType::Kick));
        assert!(c.confidence() < 0.9, "features alone are never certain");
    }

    #[test]
    fn disagreement_is_reported() {
        let id = Identifier::default();
        let c = id.classify(&StripInput::named("GTR").with_descriptor(desc("kick")));
        let gtr = c.ranked.iter().find(|c| c.source == SourceType::ElectricGuitar).unwrap();
        assert!(gtr.evidence.iter().any(|e| matches!(e, Evidence::FeaturesDisagree { .. })));
        assert!(gtr.confidence < 0.8);
    }

    #[test]
    fn silent_capture_falls_back_to_name() {
        let id = Identifier::default();
        let mut d = desc("kick");
        d.rms_dbfs = -100.0;
        let c = id.classify(&StripInput::named("TX").with_descriptor(d));
        assert_eq!(c.source(), Some(SourceType::Playback));
        assert!(c.features.is_none());
        assert!(c.best().unwrap().evidence.iter().any(|e| matches!(e, Evidence::NoSignal { .. })));
    }

    #[test]
    fn override_wins_and_clears() {
        let mut id = Identifier::default();
        let sid = StripId::new("input", 7);
        let mut s = StripInput::named("KICK");
        s.id = Some(sid.clone());
        id.set_override(sid.clone(), Some(SourceType::Sampler));
        let c = id.classify(&s);
        assert!(c.overridden);
        assert_eq!(c.source(), Some(SourceType::Sampler));
        assert_eq!(c.confidence(), 1.0);
        let json = serde_json::to_string(id.overrides()).unwrap();
        assert_eq!(json, r#"{"input.7":"sampler"}"#);
        id.set_override(sid, None);
        assert_eq!(id.classify(&s).source(), Some(SourceType::Kick));
    }

    #[test]
    fn tags_and_icons_count() {
        let id = Identifier::default();
        let mut s = StripInput::named("Dave");
        s.tags = vec!["bv".into()];
        assert_eq!(id.classify(&s).source(), Some(SourceType::BackingVocal));
        let mut s = StripInput::named("Ch 3");
        s.icon = Some("snare".into());
        assert_eq!(id.classify(&s).source(), Some(SourceType::SnareTop));
    }

    #[test]
    fn colour_consensus_lends_a_family_prior() {
        let id = Identifier::default();
        let strips = vec![
            StripInput::named("VOX 1").with_colour(5),
            StripInput::named("BV 2").with_colour(5),
            StripInput::named("Sarah").with_colour(5),
            StripInput::named("Dan").with_colour(7),
        ];
        let out = id.classify_all(&strips);
        let sarah = &out[2];
        assert_eq!(sarah.best().unwrap().source.family(), "vocal");
        assert!(matches!(sarah.best().unwrap().evidence[0], Evidence::Colour { .. }));
        assert!(sarah.source().is_none(), "a colour prior alone stays below MIN_CONFIDENCE");
        assert!(out[3].ranked.is_empty());
        // With features, the colour prior and a vocal-like capture agree.
        let mut strips = strips;
        strips[2].descriptor = Some(desc("vocal"));
        let out = id.classify_all(&strips);
        assert_eq!(out[2].source(), Some(SourceType::LeadVocal));
    }

    #[test]
    fn from_surface_strip() {
        let mut st = Strip::new(StripId::new("input", 3), mix_surface::StripKind::Input, "SN TOP");
        st.tags.push("AI".into());
        let c = Identifier::default().classify(&StripInput::from_strip(&st));
        assert_eq!(c.source(), Some(SourceType::SnareTop));
        assert_eq!(c.id, Some(StripId::new("input", 3)));
    }
}

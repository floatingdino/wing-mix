//! LLM path: phrases the rules did not understand are interpreted by `mix-llm` against a closed vocabulary.
//!
//! The JSON schema restricts `term`, `feature` and `scope` to enums built from vocabulary.toml, and the answer is
//! validated again here (the CLI's schema enforcement is a request, not a guarantee). Rejected items are reported,
//! accepted ones go through the same appliers and caps as rule-derived deltas.

use std::collections::BTreeSet;

use mix_knowledge::{KnowledgeBase, Term};
use mix_llm::{render_template, LlmBridge};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::apply::Target;

const TEMPLATE: &str = include_str!("../../../data/prompts/describe_mix.md");

#[derive(Debug, Deserialize)]
struct RawOut {
    #[serde(default)]
    terms: Vec<RawTerm>,
    #[serde(default)]
    deltas: Vec<RawDelta>,
}

#[derive(Debug, Deserialize)]
struct RawTerm {
    #[serde(default)]
    phrase: String,
    term: String,
    #[serde(default)]
    intensity: Option<f64>,
    #[serde(default)]
    negated: bool,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawDelta {
    #[serde(default)]
    phrase: String,
    feature: String,
    #[serde(default)]
    scope: Option<String>,
    delta: f64,
}

/// A validated term request.
pub struct LlmTerm<'a> {
    pub term: &'a Term,
    pub intensity: f64,
    pub negated: bool,
    pub scope: Option<String>,
    pub text: String,
}

/// A validated direct delta.
pub struct LlmDelta {
    pub target: Target,
    pub delta: f64,
    pub text: String,
}

/// Validated LLM answer.
pub struct LlmResult<'a> {
    pub terms: Vec<LlmTerm<'a>>,
    pub deltas: Vec<LlmDelta>,
    /// Human-readable reasons for dropped items.
    pub rejected: Vec<String>,
    /// Leftover phrases when nothing usable came back.
    pub unplaced: Vec<String>,
}

/// Distinct delta targets of the vocabulary, scope segments removed (`band_dev_db.presence`, `level_dev_lu`, ...).
pub fn allowed_features(kb: &KnowledgeBase) -> Vec<String> {
    let set: BTreeSet<String> = kb
        .vocabulary
        .term
        .iter()
        .flat_map(|t| &t.deltas)
        .map(|d| Target::parse(&d.feature, "mix").feature())
        .collect();
    set.into_iter().collect()
}

/// `mix` plus every source id and family.
pub fn allowed_scopes(kb: &KnowledgeBase) -> Vec<String> {
    let mut set: BTreeSet<String> = BTreeSet::from(["mix".to_string()]);
    for s in &kb.sources.source {
        set.insert(s.id.clone());
        set.insert(s.family.as_str().to_string());
    }
    set.into_iter().collect()
}

/// The JSON schema sent with the request.
pub fn schema(kb: &KnowledgeBase) -> Value {
    let terms: Vec<&str> = kb.vocabulary.term.iter().map(|t| t.id.as_str()).collect();
    let max_intensity = max_intensity(kb);
    let scope = json!({"type": ["string", "null"], "enum": allowed_scopes(kb).into_iter().map(Value::from).chain([Value::Null]).collect::<Vec<_>>()});
    json!({
        "type": "object",
        "properties": {
            "terms": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "phrase": {"type": "string"},
                    "term": {"type": "string", "enum": terms},
                    "intensity": {"type": "number", "minimum": 0.0, "maximum": max_intensity},
                    "negated": {"type": "boolean"},
                    "scope": scope,
                },
                "required": ["phrase", "term", "intensity", "negated"],
                "additionalProperties": false
            }},
            "deltas": {"type": "array", "items": {
                "type": "object",
                "properties": {
                    "phrase": {"type": "string"},
                    "feature": {"type": "string", "enum": allowed_features(kb)},
                    "scope": scope,
                    "delta": {"type": "number", "minimum": -12.0, "maximum": 12.0},
                },
                "required": ["phrase", "feature", "delta"],
                "additionalProperties": false
            }},
        },
        "required": ["terms", "deltas"],
        "additionalProperties": false
    })
}

fn max_intensity(kb: &KnowledgeBase) -> f64 {
    kb.vocabulary
        .modifiers
        .intensity
        .values()
        .fold(1.0_f64, |a, &b| a.max(b))
}

/// Ask the LLM about `leftovers` ((phrase, context clause) pairs). `Err` carries a note for `Description::notes`.
pub fn interpret<'a>(
    kb: &'a KnowledgeBase,
    bridge: &LlmBridge,
    leftovers: &[(String, String)],
) -> Result<LlmResult<'a>, String> {
    if !bridge.is_available() {
        return Err("llm unavailable (disabled, binary missing or budget used)".into());
    }
    let input = json!({
        "phrases": leftovers.iter().map(|(p, c)| json!({"phrase": p, "context": c})).collect::<Vec<_>>(),
        "terms": kb.vocabulary.term.iter().map(|t| json!({
            "id": t.id, "synonyms": t.synonyms, "kind": format!("{:?}", t.kind).to_lowercase(),
            "scopable": t.scopable,
        })).collect::<Vec<_>>(),
        "features": allowed_features(kb),
        "scopes": allowed_scopes(kb),
    });
    let prompt = render_template(TEMPLATE, &input.to_string());
    let raw: RawOut = bridge
        .ask_prompt("describe_mix", &prompt, &schema(kb))
        .map_err(|e| e.to_string())?;
    Ok(validate(kb, raw, leftovers))
}

fn validate<'a>(
    kb: &'a KnowledgeBase,
    raw: RawOut,
    leftovers: &[(String, String)],
) -> LlmResult<'a> {
    let scopes = allowed_scopes(kb);
    let features = allowed_features(kb);
    let max_i = max_intensity(kb);
    let norm_scope = |s: &Option<String>| -> Result<Option<String>, String> {
        match s.as_deref() {
            None | Some("") | Some("mix") => Ok(None),
            Some(x) if scopes.iter().any(|a| a == x) => Ok(Some(x.to_string())),
            Some(x) => Err(format!("unknown scope `{x}`")),
        }
    };
    let mut res = LlmResult {
        terms: Vec::new(),
        deltas: Vec::new(),
        rejected: Vec::new(),
        unplaced: Vec::new(),
    };
    for t in raw.terms {
        let (Some(term), Ok(scope)) = (kb.term(&t.term), norm_scope(&t.scope)) else {
            res.rejected.push(format!(
                "llm term `{}` (scope {:?}) rejected",
                t.term, t.scope
            ));
            continue;
        };
        let intensity = t.intensity.filter(|i| i.is_finite()).unwrap_or(1.0);
        res.terms.push(LlmTerm {
            term,
            intensity: intensity.clamp(0.0, max_i),
            negated: t.negated,
            scope: scope.filter(|_| term.scopable),
            text: t.phrase,
        });
    }
    for d in raw.deltas {
        let scope = norm_scope(&d.scope);
        if !features.contains(&d.feature) || scope.is_err() || !d.delta.is_finite() {
            res.rejected
                .push(format!("llm delta `{}` ({}) rejected", d.feature, d.delta));
            continue;
        }
        let scope = scope.ok().flatten().unwrap_or_else(|| "mix".into());
        res.deltas.push(LlmDelta {
            target: Target::parse(&d.feature, &scope),
            delta: d.delta,
            text: d.phrase,
        });
    }
    if res.terms.is_empty() && res.deltas.is_empty() {
        res.unplaced = leftovers.iter().map(|(p, _)| p.clone()).collect();
    }
    res
}

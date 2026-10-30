//! mix-describe: turn a text mix description ("warm, vocal-forward, punchy drums, wide") into a [`TargetProfile`].
//!
//! Pipeline ([`describe`]):
//! 1. Start from a base profile (the caller's reference/genre profile, else [`neutral_profile`]) and clone it.
//! 2. Rule path: the text is split into clauses; every vocabulary term (`data/knowledge/vocabulary.toml`, via
//!    `mix_knowledge`) found in a clause is resolved with its intensity/negation/scope modifiers and its deltas are
//!    applied to the clone ([`apply::Applier`]), each recorded in `TargetProfile::applied_deltas`. Stacked terms
//!    are capped per target by the vocabulary `[limits]`.
//! 3. LLM path (optional): words the rules did not understand go to `mix-llm` with a JSON schema whose enums are
//!    the vocabulary's terms, features and scopes. The answer is validated (unknown terms/features/scopes are
//!    dropped and reported) and goes through the same appliers and caps as the rules.
//!
//! With the LLM disabled or failing, step 3 is skipped and the unrecognised phrases are reported in
//! [`Description::unrecognised`]; nothing else changes. All profile edits are small shifts of a cloned profile,
//! so the planner still reaches them through the normal diagnosis loop (remit, limits and workflow order apply).

pub mod apply;
pub mod base;
mod llm;

use std::collections::BTreeSet;

use mix_core::profile::{ProfileSource, TargetProfile};
use mix_knowledge::{KnowledgeBase, ResolvedTerm};
use mix_llm::LlmBridge;

pub use apply::{Applier, Target};
pub use base::{genre_names, genre_profile, neutral_profile};

/// One vocabulary term that was applied, with its effective deltas.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedTerm {
    /// Term id.
    pub term: String,
    /// Original wording this came from (the clause or leftover phrase).
    pub text: String,
    /// `true` when the LLM identified it rather than the rule parser.
    pub from_llm: bool,
    /// Effective `(feature, delta)` after intensity, sign and caps.
    pub deltas: Vec<(String, f64)>,
}

/// Outcome of [`describe`].
#[derive(Debug, Clone)]
pub struct Description {
    /// The edited clone; `applied_deltas` lists every change.
    pub profile: TargetProfile,
    /// Terms applied, in order.
    pub terms: Vec<AppliedTerm>,
    /// Pairs of requested terms whose deltas oppose each other (they were summed; mention the trade-off).
    pub conflicts: Vec<(String, String)>,
    /// Phrases neither rules nor the LLM could place (LLM disabled/failed, or it declined).
    pub unrecognised: Vec<String>,
    /// Whether an LLM call was made.
    pub llm_used: bool,
    /// Why the LLM path did not contribute (unavailable, error) or what it rejected.
    pub notes: Vec<String>,
}

/// Words that carry no mix meaning; they do not count as "unrecognised".
const FILLER: &[&str] = &[
    "a", "an", "the", "of", "and", "with", "it", "its", "is", "are", "to", "be", "mix", "sound",
    "sounds", "overall", "kind", "sort", "i", "want", "like", "please", "make", "that", "this",
    "bit", "all", "feel", "feels", "just", "in", "on", "for", "but", "some", "little", "more",
    "vibe", "track", "song", "me", "my", "so", "very",
];

fn tokens(s: &str) -> Vec<String> {
    s.to_lowercase()
        .replace(['-', '_'], " ")
        .split(|c: char| !c.is_alphanumeric() && c != ' ' && c != '\'')
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .map(str::to_string)
        .collect()
}

/// Split into clauses on punctuation and the joiners "and", "but", "with", "plus".
fn clauses(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    for chunk in text.split([',', ';', '.', '+', '&', '\n', '/']) {
        for tok in chunk.split_whitespace() {
            if matches!(tok.to_lowercase().as_str(), "and" | "but" | "plus" | "with") {
                if !cur.is_empty() {
                    out.push(cur.join(" "));
                    cur.clear();
                }
            } else {
                cur.push(tok.to_string());
            }
        }
        if !cur.is_empty() {
            out.push(cur.join(" "));
            cur.clear();
        }
    }
    out
}

/// The scope (source id or family, singularised) a token names, if any: "drums" -> "drums", "vocals" -> "vocal".
fn scope_of(kb: &KnowledgeBase, token: &str) -> Option<String> {
    let sing = token.strip_suffix('s').unwrap_or(token);
    [token, sing].into_iter().find_map(|t| {
        (kb.source(t).is_some() || kb.sources.source.iter().any(|s| s.family.as_str() == t))
            .then(|| t.to_string())
    })
}

/// Terms found in one clause (rule path) plus the leftover content words.
struct Scan<'a> {
    resolved: Vec<ResolvedTerm<'a>>,
    leftover: Vec<String>,
}

fn scan_clause<'a>(kb: &'a KnowledgeBase, clause: &str) -> Scan<'a> {
    let toks = tokens(clause);
    let mut owner: Vec<Option<usize>> = vec![None; toks.len()]; // span index per token
    let mut spans: Vec<(usize, usize)> = Vec::new(); // (start, end)
    for len in (1..=5.min(toks.len())).rev() {
        for start in 0..=toks.len() - len {
            if owner[start..start + len].iter().any(Option::is_some) {
                continue;
            }
            if kb.term(&toks[start..start + len].join(" ")).is_some() {
                spans.push((start, start + len));
                let id = spans.len() - 1;
                owner[start..start + len]
                    .iter_mut()
                    .for_each(|o| *o = Some(id));
            }
        }
    }
    spans.sort();
    // A scope word anywhere in the clause qualifies scopable terms that have none of their own.
    let clause_scope = toks
        .iter()
        .filter(|t| kb.term(t).is_none())
        .find_map(|t| scope_of(kb, t));
    let mut resolved = Vec::new();
    for i in 0..spans.len() {
        let from = if i == 0 { 0 } else { spans[i - 1].1 };
        let to = spans.get(i + 1).map_or(toks.len(), |n| n.0);
        if let Some(mut r) = kb.resolve_phrase(&toks[from..to].join(" ")) {
            if r.scope.is_none() && r.term.scopable {
                r.scope = clause_scope.clone();
            }
            resolved.push(r);
        }
    }
    let m = &kb.vocabulary.modifiers;
    let mut covered: BTreeSet<String> = FILLER.iter().map(|s| s.to_string()).collect();
    covered.extend(m.negation.words.iter().flat_map(|w| tokens(w)));
    covered.extend(m.intensity.keys().flat_map(|w| tokens(w)));
    covered.extend(m.comparative.words.iter().flat_map(|w| tokens(w)));
    let leftover = toks
        .iter()
        .enumerate()
        .filter(|(i, t)| owner[*i].is_none() && !covered.contains(*t) && scope_of(kb, t).is_none())
        .map(|(_, t)| t.clone())
        .collect();
    Scan { resolved, leftover }
}

/// Convert a description into a profile. `base` is the profile the description modifies (a reference or genre
/// profile); `None` uses [`neutral_profile`]. `llm` may be [`LlmBridge::disabled`].
pub fn describe(
    text: &str,
    base: Option<&TargetProfile>,
    kb: &KnowledgeBase,
    llm: &LlmBridge,
) -> Description {
    let neutral;
    let base_ref = match base {
        Some(b) => b,
        None => {
            neutral = neutral_profile();
            &neutral
        }
    };
    let mut ap = Applier::new(kb, base_ref);
    let mut out = Description {
        profile: base_ref.clone(),
        terms: Vec::new(),
        conflicts: Vec::new(),
        unrecognised: Vec::new(),
        llm_used: false,
        notes: Vec::new(),
    };
    let mut leftovers: Vec<(String, String)> = Vec::new(); // (leftover phrase, context clause)

    for clause in clauses(text) {
        let scan = scan_clause(kb, &clause);
        for r in &scan.resolved {
            out.terms
                .push(apply_resolved(kb, &mut ap, r, &clause, false));
        }
        if !scan.leftover.is_empty() {
            leftovers.push((scan.leftover.join(" "), clause));
        }
    }

    if !leftovers.is_empty() {
        match llm::interpret(kb, llm, &leftovers) {
            Ok(res) => {
                out.llm_used = true;
                out.notes.extend(res.rejected);
                for t in res.terms {
                    let rt = ResolvedTerm {
                        term: t.term,
                        intensity: t.intensity,
                        negated: t.negated,
                        scope: t.scope,
                    };
                    out.terms
                        .push(apply_resolved(kb, &mut ap, &rt, &t.text, true));
                }
                for d in res.deltas {
                    let applied = ap.apply(&format!("llm:{}", d.text), &d.target, d.delta);
                    out.terms.push(AppliedTerm {
                        term: format!("llm:{}", d.target.feature()),
                        text: d.text,
                        from_llm: true,
                        deltas: vec![(d.target.feature(), applied)],
                    });
                }
                out.unrecognised.extend(res.unplaced);
            }
            Err(note) => {
                out.notes.push(note);
                out.unrecognised
                    .extend(leftovers.into_iter().map(|(l, _)| l));
            }
        }
    }

    // conflicts between requested terms
    let ids: Vec<&str> = out.terms.iter().map(|t| t.term.as_str()).collect();
    for a in &ids {
        for b in &ids {
            if a < b
                && kb
                    .term(a)
                    .is_some_and(|t| t.conflicts.iter().any(|c| c == b))
            {
                let pair = (a.to_string(), b.to_string());
                if !out.conflicts.contains(&pair) {
                    out.conflicts.push(pair);
                }
            }
        }
    }

    let mut profile = ap.profile;
    profile.source = match base {
        Some(b) => ProfileSource::Merged {
            parts: vec![
                b.source.clone(),
                ProfileSource::Description { text: text.into() },
            ],
        },
        None => ProfileSource::Description { text: text.into() },
    };
    out.profile = profile;
    out
}

fn apply_resolved(
    kb: &KnowledgeBase,
    ap: &mut Applier<'_>,
    r: &ResolvedTerm<'_>,
    text: &str,
    from_llm: bool,
) -> AppliedTerm {
    let fallback = r
        .scope
        .clone()
        .unwrap_or_else(|| r.term.default_scope.clone());
    let mut deltas = Vec::new();
    for d in kb.scaled_deltas(r) {
        let target = Target::parse(&d.feature, &fallback);
        let eff = ap.apply(&r.term.id, &target, d.delta);
        deltas.push((d.feature, eff));
    }
    AppliedTerm {
        term: r.term.id.clone(),
        text: text.to_string(),
        from_llm,
        deltas,
    }
}

#[cfg(test)]
mod tests;

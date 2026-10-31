//! Description intents (`TargetProfile.applied_deltas` with `target_path = "intent.<scope>.<feature>"`, M024).
//!
//! mix-describe records a delta as an intent when the profile has no field for it: source levels without stems,
//! reverb/FX amount, sibilance, masking, scoped spectra without stems. Closed-loop checks that can measure the
//! quantity (source/stem level, masking, sibilance) shift their own target by the summed delta and mark the path
//! consumed. Every remaining intent becomes an **open-loop** problem: the requested change minus what the
//! controller reports as applied ([`DiagnosisInput::intent_progress`](super::DiagnosisInput)), with no measurement.
//! The subject is the mix, the matching strips (as contributors of a `Source` subject), or the bare source/family
//! when no strip matches.

use std::collections::BTreeMap;

use super::problem::{Contributor, Evidence, ProblemKind, Scope, Subject};
use super::{Bag, Ctx, Draft};

/// `(scope, feature)` of an intent path.
pub fn parse_intent_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("intent.")?;
    let (scope, feature) = rest.split_once('.')?;
    (!scope.is_empty() && !feature.is_empty()).then_some((scope, feature))
}

fn kind_of(base: &str, delta: f32) -> (ProblemKind, &'static [&'static str]) {
    match base {
        "band_dev_db" => (ProblemKind::Tonal, &["Eq"]),
        "tilt_dev_db_per_oct" => (ProblemKind::Tilt, &["Eq"]),
        "level_dev_lu" | "lufs_i" => (ProblemKind::Level, &["Fader"]),
        "reverb_rel_dev_db" | "reverb_rel_db" | "wet_rel_dry_lu" => {
            (ProblemKind::Depth, &["Send", "FxSlot"])
        }
        "width_dev_db" | "side_to_mid_db" | "correlation" => (ProblemKind::Width, &["Pan"]),
        "crest_db" | "lra_lu" | "plr_db" => {
            // A request for more dynamics means the mix is currently too squashed.
            if delta > 0.0 {
                (ProblemKind::Squashed, &["Dyn"])
            } else {
                (ProblemKind::TooDynamic, &["Dyn"])
            }
        }
        "transient_db" => (ProblemKind::Punch, &["Dyn"]),
        "sibilance_db" => (ProblemKind::Sibilance, &["Dyn"]),
        "masking_db" => (ProblemKind::Masking, &["Eq"]),
        _ => (ProblemKind::Tonal, &["Eq"]),
    }
}

pub(crate) fn check(cx: &mut Ctx<'_>) {
    // Sum per path, keeping first-seen order of terms.
    let mut sums: BTreeMap<String, (f32, Vec<String>)> = BTreeMap::new();
    for d in &cx.input.target.applied_deltas {
        if parse_intent_path(&d.target_path).is_none() {
            continue;
        }
        let e = sums.entry(d.target_path.clone()).or_default();
        e.0 += d.delta;
        if !e.1.contains(&d.term) {
            e.1.push(d.term.clone());
        }
    }
    for (path, (delta, terms)) in sums {
        if cx.consumed_intents.contains(&path) || delta == 0.0 {
            continue;
        }
        let (scope, feature) = parse_intent_path(&path).expect("filtered above");
        let base = feature.split('.').next().unwrap_or(feature);
        let ratio = matches!(base, "correlation");
        let tol = if ratio {
            cx.cfg.intent_tolerance * 0.1
        } else {
            cx.cfg.intent_tolerance
        };
        let tracked = cx.input.intent_progress.get(&path).copied();
        let done = tracked.unwrap_or(0.0);
        let deviation = done - delta;
        let unit = cx
            .kb
            .workflow
            .features
            .get(base)
            .map_or("db", |f| f.unit.as_str())
            .to_string();
        let e = Evidence {
            feature: feature.to_string(),
            // M062: progress followed through the console (source level intents via the faders) is a measurement
            measured: tracked.filter(|_| base == "level_dev_lu" && scope != "mix"),
            target: delta,
            tolerance: tol,
            unit,
            deviation,
            excess: (deviation.abs() - tol).max(0.0),
            allowance: tol,
        };
        let (kind, blocks) = kind_of(base, delta);
        let (subject, contributors) = if scope == "mix" {
            (Subject::Mix, Vec::new())
        } else {
            let matching: Vec<Contributor> = cx
                .input
                .strips
                .iter()
                .filter(|s| {
                    // M062: estimated strips count too: an open-loop intent's deviation does not read the taps
                    s.source.as_deref().is_some_and(|src| {
                        src == scope
                            || cx
                                .kb
                                .source(src)
                                .is_some_and(|x| x.family.as_str() == scope)
                    })
                })
                .map(|s| Contributor {
                    strip: s.strip.clone(),
                    source: s.source.clone(),
                    share: 0.0,
                })
                .collect();
            let n = matching.len().max(1) as f32;
            let contrib = matching
                .into_iter()
                .map(|mut c| {
                    c.share = 1.0 / n;
                    c
                })
                .collect();
            (
                Subject::Source {
                    source: scope.to_string(),
                },
                contrib,
            )
        };
        let rules = cx.rules_for(&subject, feature, -(deviation as f64), &Bag::new());
        let explanation = format!(
            "description asks for {feature} {delta:+.1} on {scope} ({}); {done:+.1} applied so far",
            terms.join(", ")
        );
        cx.push(
            Draft::new(kind, Scope::of(subject), e)
                .weight(1.0)
                .confidence(0.5)
                .rules(rules)
                .blocks(blocks)
                .terms(terms)
                .contributors(contributors)
                .explain(explanation),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::parse_intent_path;

    #[test]
    fn parses_paths() {
        assert_eq!(
            parse_intent_path("intent.lead_vocal.band_dev_db.presence"),
            Some(("lead_vocal", "band_dev_db.presence"))
        );
        assert_eq!(
            parse_intent_path("intent.mix.reverb_rel_dev_db"),
            Some(("mix", "reverb_rel_dev_db"))
        );
        assert_eq!(parse_intent_path("tonal.tilt_db_per_oct"), None);
        assert_eq!(parse_intent_path("intent.mix"), None);
    }
}

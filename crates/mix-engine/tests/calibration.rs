//! Diagnosis calibration on mix-sim (M034): the reference scene measured against its own profile (perfect-separation
//! stem targets, see `common`) must be quiet, and the degraded scene must still show every observable ground-truth
//! delta of `mix_sim::scenes::ground_truth`.
//!
//! Ground truth → expected problem (subject strips: a stem's strips or a mix problem's contributors):
//! - vocal fader −8 dB → `level` on the vocal stem/strip, below target;
//! - bass fader +6 dB → `level` on the bass stem/strip, above target;
//! - keys L/R +8 dB at 250 Hz → `tonal` above target in low_mid_lo/low_mid involving KEYS L or R;
//! - guitar HPF off → `tonal` above target in sub/bass involving GTR;
//! - hats +8 dB → `tonal` above target in presence/brilliance/air involving HAT;
//! - snare −6 dB → `tonal` below target in mid/upper_mid/presence involving SNARE (weak: the louder hats also
//!   lower every other drum band relative to the drum stem);
//! - OH L/R panned centre → a `width` problem, narrower than target.
//!
//! Below observability on some seeds (also missed by the M031 diagnosis before M034, which raised ~150 problems
//! on the matched mix): the keys' +8 dB at 250 Hz on seeds 6 and 8 (the keys carry < 5 % of the main's low-mid and
//! the Other stem's low-mid stays inside the reference's 20 dB window spread) and the snare on seed 8.
//!
//! Not observable: "vocal not compressed" (no strip/stem dynamics check: the session crest of a strip is not
//! comparable with the stem's short-term crest distribution; M034 follow-up).

mod common;

use std::collections::BTreeMap;

use common::*;
use mix_core::ids::StripId;
use mix_engine::diagnosis::Subject;
use mix_engine::{Diagnosis, Problem, ProblemKind};
use mix_knowledge::KnowledgeBase;

/// Severity a matched mix may reach (acceptance: none above).
const QUIET: f32 = 0.3;

fn involves(p: &Problem, ch: u32) -> bool {
    let id = StripId::new("input", ch);
    let in_subject = |s: &Subject| match s {
        Subject::Strip { strip, .. } => *strip == id,
        Subject::Stem { strips, .. } => strips.contains(&id),
        _ => false,
    };
    in_subject(&p.scope.subject)
        || p.scope.other.as_ref().is_some_and(in_subject)
        || p.contributors.iter().any(|c| c.strip == id)
}

fn in_bands(p: &Problem, bands: &[&str]) -> bool {
    p.scope
        .band
        .as_ref()
        .is_some_and(|b| bands.contains(&b.name.as_str()))
}

type Check = dyn Fn(&Problem) -> bool;

fn tonal(p: &Problem) -> bool {
    matches!(p.kind, ProblemKind::Tonal | ProblemKind::Harshness)
}

/// Ground-truth deltas without a matching problem.
fn missing(d: &Diagnosis) -> Vec<&'static str> {
    let has = |f: &Check| d.problems.iter().any(f);
    let mut out = Vec::new();
    let checks: [(&'static str, &Check); 7] = [
        ("vocal buried", &|p| {
            p.kind == ProblemKind::Level && involves(p, 13) && p.evidence.deviation < 0.0
        }),
        ("bass too loud", &|p| {
            p.kind == ProblemKind::Level && involves(p, 7) && p.evidence.deviation > 0.0
        }),
        ("keys muddy", &|p| {
            tonal(p)
                && in_bands(p, &["low_mid_lo", "low_mid"])
                && (involves(p, 8) || involves(p, 9))
                && p.evidence.deviation > 0.0
        }),
        ("guitar HPF off", &|p| {
            tonal(p)
                && in_bands(p, &["sub", "bass"])
                && involves(p, 10)
                && p.evidence.deviation > 0.0
        }),
        ("hats too loud", &|p| {
            tonal(p)
                && in_bands(p, &["presence", "brilliance", "air"])
                && involves(p, 3)
                && p.evidence.deviation > 0.0
        }),
        ("snare too quiet", &|p| {
            tonal(p)
                && in_bands(p, &["mid", "upper_mid", "presence"])
                && involves(p, 2)
                && p.evidence.deviation < 0.0
        }),
        ("overheads centred", &|p| {
            p.kind == ProblemKind::Width && p.evidence.deviation < 0.0
        }),
    ];
    for (name, f) in checks {
        if !has(f) {
            out.push(name);
        }
    }
    out
}

fn summary(d: &Diagnosis) -> BTreeMap<&'static str, (usize, usize)> {
    let mut m = BTreeMap::new();
    for p in &d.problems {
        let e = m.entry(p.kind.as_str()).or_insert((0, 0));
        e.0 += 1;
        if p.severity > QUIET {
            e.1 += 1;
        }
    }
    m
}

/// (reference problems above `QUIET`, missing ground truth) for one seed.
fn calibrate(seed: u64, kb: &KnowledgeBase) -> (Vec<String>, Vec<&'static str>) {
    let fx = Fixture::new(seed, kb);
    let r = fx.diagnose(&fx.reference, kb);
    let deg = Measured::of(&render(&fx.console(&fx.pair.degraded)));
    let d = fx.diagnose(&deg, kb);
    eprintln!(
        "seed {seed}: reference {} problems {:?}; degraded {} problems {:?}",
        r.problems.len(),
        summary(&r),
        d.problems.len(),
        summary(&d)
    );
    if std::env::var("VERBOSE").is_ok() {
        print_diagnosis("reference", &r);
        print_diagnosis("degraded", &d);
    }
    let loud = r
        .problems
        .iter()
        .filter(|p| p.severity > QUIET)
        .map(|p| format!("{} sev {:.2}: {}", p.id, p.severity, p.explanation))
        .collect();
    let mut miss = missing(&d);
    miss.retain(|m| !UNOBSERVABLE.contains(&(seed, *m)));
    (loud, miss)
}

/// Deltas this fixture cannot show on a seed (module docs).
const UNOBSERVABLE: &[(u64, &str)] =
    &[(6, "keys muddy"), (8, "keys muddy"), (8, "snare too quiet")];

fn assert_calibrated(seeds: &[u64]) {
    let kb = KnowledgeBase::embedded();
    let mut fails = Vec::new();
    for &seed in seeds {
        let (loud, miss) = calibrate(seed, kb);
        if !loud.is_empty() || !miss.is_empty() {
            fails.push(format!(
                "seed {seed}: reference {loud:#?}, missing {miss:?}"
            ));
        }
    }
    assert!(fails.is_empty(), "{fails:#?}");
}

#[test]
fn matched_mix_is_quiet_and_ground_truth_is_found() {
    assert_calibrated(&[7]);
}

#[test]
#[ignore = "slow: run with make check-full"]
fn matched_mix_is_quiet_seeds_3_to_11() {
    assert_calibrated(&[3, 4, 5, 6, 7, 8, 9, 10, 11]);
}

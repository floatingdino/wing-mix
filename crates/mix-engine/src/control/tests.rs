//! Unit tests of the controller's pure parts (keep rule, hunting guard, section detector). The closed loop itself
//! is tested against the simulator in `tests/control_sim.rs`.

use std::collections::BTreeMap;

use mix_core::action::{Action, ParamChange};
use mix_core::features::{Features, Window};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_knowledge::KnowledgeBase;

use super::*;
use crate::plan::{PlannedMove, PredictedEffect};

fn cfg() -> ControlConfig {
    ControlConfig::from_kb(KnowledgeBase::embedded())
}

fn mv(param: &str, from: f64, to: f64, gain: f64) -> PlannedMove {
    let mut c = ParamChange::new(ParamId::new(param), Value::Float(to));
    c.from = Some(Value::Float(from));
    PlannedMove {
        action: Action::new(format!("{param} -> {to}")).with(c),
        strip: StripId::parse(param.split('/').next().unwrap()).unwrap(),
        phase: "balance".into(),
        problem: "p".into(),
        rules: Vec::new(),
        explanation: String::new(),
        predicted: PredictedEffect {
            objective_before: 1.0,
            objective_after: 1.0 - gain,
            distance_before: 1.0,
            distance_after: 1.0 - gain as f32,
            problem: None,
            strip_level_db: BTreeMap::new(),
            main_level_db: 0.0,
        },
        cost: 0.0,
        benefit: gain,
        needs_confirmation: Vec::new(),
    }
}

#[test]
fn keep_rule() {
    let r = cfg().keep;
    // delivered what was predicted (or more)
    assert!(keep(1.0, 0.8, 0.2, &r));
    assert!(keep(1.0, 0.7, 0.2, &r));
    // half of a large prediction: inside rel_tol
    assert!(keep(1.0, 0.89, 0.2, &r));
    // a tiny predicted gain within measurement noise
    assert!(keep(1.0, 0.995, 0.01, &r));
    // far below prediction, but a >= 5 % improvement: kept (workflow.toml keep_if_distance_reduced_by)
    assert!(keep(2.0, 1.88, 0.5, &r));
    // got worse / did almost nothing of a big prediction
    assert!(!keep(1.0, 1.1, 0.2, &r));
    assert!(!keep(1.0, 0.99, 0.3, &r));
    assert!(!keep(1.0, 1.03, 0.01, &r));
}

#[test]
fn block_and_type() {
    assert_eq!(guard::block_of(&ParamId::new("input.3/eq/band.1.gain")), "eq");
    assert_eq!(guard::block_of(&ParamId::new("input.3/fader")), "fader");
    assert_eq!(action_type(&mv("input.3/fader", 0.0, 1.0, 0.1)), "balance:fader");
    assert!(guard::is_db_param(&ParamId::new("input.3/eq/band.1.gain")));
    assert!(!guard::is_db_param(&ParamId::new("input.3/eq/band.1.freq")));
}

#[test]
fn hunting_guard_limits_reversals() {
    let c = cfg();
    let mut g = Guard::new();
    let p = ParamId::new("input.1/fader");
    let f = Value::Float;
    // up, up: no reversal
    assert_eq!(g.record(&p, &f(-10.0), &f(-7.0), 0.0, &c), None);
    assert_eq!(g.record(&p, &f(-7.0), &f(-4.0), 1.0, &c), None);
    assert_eq!(g.reversals(&p), 0);
    assert!(g.check(&mv("input.1/fader", -4.0, -1.0, 0.1), 2.0, &c).is_ok());
    // down: a reversal, then the param cools down
    assert_eq!(g.record(&p, &f(-4.0), &f(-7.0), 2.0, &c), None);
    assert_eq!(g.reversals(&p), 1);
    assert!(matches!(
        g.check(&mv("input.1/fader", -7.0, -9.0, 0.1), 3.0, &c),
        Err(Block::Wait { .. })
    ));
    // after the cool-down: continuing down is fine, going up again would need the last spare reversal
    let later = 3.0 + c.reversal_cooldown_s;
    assert!(g.check(&mv("input.1/fader", -7.0, -9.0, 0.1), later, &c).is_ok());
    assert!(matches!(
        g.check(&mv("input.1/fader", -7.0, -5.0, 0.1), later, &c),
        Err(Block::Never(_))
    ));
    // a revert still records (and may freeze): reverts are never blocked
    assert_eq!(
        g.record(&p, &f(-7.0), &f(-4.0), later, &c),
        Some(c.max_reversals)
    );
    assert!(g.is_frozen(&p, &c));
    assert!(matches!(
        g.check(&mv("input.1/fader", -4.0, -6.0, 0.1), later + 1e6, &c),
        Err(Block::Never(_))
    ));
}

#[test]
fn penalties_and_bans() {
    let c = cfg();
    let mut g = Guard::new();
    let m = mv("input.2/eq/band.1.gain", 0.0, 3.0, 0.015);
    assert!(g.check(&m, 0.0, &c).is_ok());
    g.penalise(&m);
    // 0.015 x 0.5 = 0.0075 still >= min_benefit
    assert!(g.check(&m, 0.0, &c).is_ok());
    g.penalise(&m);
    // banned on input.2 after two reverts; another strip only sees the penalty (0.015 x 0.25 < 0.005? no: 0.00375)
    assert!(matches!(g.check(&m, 0.0, &c), Err(Block::Never(_))));
    let other = mv("input.4/eq/band.1.gain", 0.0, 3.0, 0.015);
    assert!(matches!(g.check(&other, 0.0, &c), Err(Block::Never(_))));
    let big = mv("input.4/eq/band.1.gain", 0.0, 3.0, 0.2);
    assert!(g.check(&big, 0.0, &c).is_ok());
}

#[test]
fn engineer_hold() {
    let c = cfg();
    let mut g = Guard::new();
    let p = ParamId::new("input.1/fader");
    g.engineer(&p, 10.0, &c);
    assert!(matches!(
        g.check(&mv("input.1/fader", 0.0, -3.0, 0.1), 11.0, &c),
        Err(Block::Wait { .. })
    ));
    assert!(g
        .check(&mv("input.1/fader", 0.0, -3.0, 0.1), 10.0 + c.engineer_hold_s, &c)
        .is_ok());
}

fn st(loud: f32, tilt: f32) -> Features {
    let mut f = Features::new("main", Window::ShortTerm, true);
    f.loudness.short_term_lufs = loud;
    for (b, x) in f.spectrum.erb_mid_rel_db.iter_mut().enumerate() {
        *x = -20.0 + tilt * (b as f32 / 40.0 - 0.5);
    }
    f
}

#[test]
fn section_detector() {
    let c = cfg().section;
    let mut d = SectionDetector::new(c);
    let mut t = 0.0;
    let mut feed = |d: &mut SectionDetector, f: &Features, secs: f64| {
        let mut hits = Vec::new();
        let end = t + secs;
        while t < end {
            if let Some(j) = d.observe(t, f) {
                hits.push(j);
            }
            t += c.poll_s;
        }
        hits
    };
    // stationary with small wobble: nothing
    assert!(feed(&mut d, &st(-18.0, 0.0), 10.0).is_empty());
    assert!(feed(&mut d, &st(-19.5, 2.0), 10.0).is_empty());
    // a 7 LU jump (verse -> chorus) is detected once, after the hold time
    let h = feed(&mut d, &st(-11.0, 0.0), 10.0);
    assert_eq!(h.len(), 1, "{h:?}");
    assert!(h[0].loudness_jump_lu > 5.0);
    // a big spectral change at the same loudness
    let h = feed(&mut d, &st(-11.0, 30.0), 10.0);
    assert_eq!(h.len(), 1, "{h:?}");
    assert!(h[0].spectral_jump_db > 5.0);
    // a short spike shorter than the hold time and silence are ignored
    assert!(feed(&mut d, &st(-3.0, 30.0), c.hold_s * 0.5).is_empty());
    assert!(feed(&mut d, &st(-90.0, 30.0), 5.0).is_empty());
    assert!(feed(&mut d, &st(-11.0, 30.0), 10.0).is_empty());
}

#[test]
fn scaled_config() {
    let c = cfg();
    let s = c.clone().scaled(0.5);
    assert_eq!(s.window_s, c.window_s * 0.5);
    assert_eq!(s.engineer_hold_s, c.engineer_hold_s * 0.5);
    assert_eq!(s.section.tau_s, c.section.tau_s * 0.5);
    assert_eq!(s.max_batches, c.max_batches);
    // planning stays within the safety layer's per-move boost
    assert!(c.plan.candidates.max_fader_step_db <= 3.0);
}

#[test]
fn events_serialise() {
    let e = ControlEvent::Holding {
        reason: HoldReason::Converged,
        objective: 0.3,
    };
    let j = serde_json::to_string(&e).unwrap();
    assert!(j.contains("\"type\":\"holding\"") && j.contains("converged"), "{j}");
    let s = MoveSummary::of(&mv("input.1/fader", 0.0, -3.0, 0.1), false);
    let j = serde_json::to_string(&ControlEvent::Suggested { moves: vec![s] }).unwrap();
    assert!(j.contains("suggested") && j.contains("input.1/fader"), "{j}");
}

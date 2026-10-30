//! M035 end-to-end: the planner proposes a de-esser for a sibilant vocal and a gate for a bleeding tom, and the
//! moves do in the sim what was predicted.
//!
//! - De-esser: the synthetic song with sibilants on VOX (`common::add_sibilants`); the target is the reference scene
//!   with WING `DS902` de-essing VOX in its gate slot (5 kHz, 6 dB); the console is the reference scene without it.
//! - Gate: TOM plays only part of its first fill (3.0-3.35 s of 8 s: a sparse tom: activity < 20 %, sources.toml `tom_cymbal_bleed`) with the
//!   OH L track at −40 dB as cymbal bleed (below the analyser's relative gate: the tom reads as sparse); the target is the reference scene with the TOM gated (dynamics.toml
//!   `tom_gate`); the console is the reference scene without the gate.
//!
//! The default remit makes input model switches (`gate/model`) conditional: planning allows conditional changes
//! (flagged with their confirmation key), so the de-esser move carries a confirmation.

mod common;

use common::*;
use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_engine::diagnosis::Diagnosis;
use mix_engine::plan::{PlanConfig, PlannedMove};
use mix_engine::ProblemKind;
use mix_knowledge::KnowledgeBase;
use mix_sim::scenes::FixturePair;
use mix_surface::remit::RemitSession;

const SEED: u64 = 7;

fn w(p: &str, v: Value) -> (ParamId, Value) {
    (ParamId::new(p), v)
}

fn sibilance_score(d: &Diagnosis) -> f32 {
    d.problems
        .iter()
        .filter(|p| p.kind == ProblemKind::Sibilance)
        .map(|p| p.score())
        .sum()
}

struct Outcome {
    plan: mix_engine::Plan,
    before: Diagnosis,
    after: Diagnosis,
    after_objective: f64,
}

/// Plan the `pair.degraded` console against the `pair.reference` render, apply the plan in the sim, re-measure.
fn plan_apply(pair: FixturePair, kb: &KnowledgeBase) -> Outcome {
    let fx = Fixture::from_pair(pair, kb);
    let sim = fx.console(&fx.pair.degraded);
    let m = Measured::of(&render(&sim));
    let (policy, ctx) = remit_parts(&sim);
    let cfg = PlanConfig::from_kb(kb);
    let plan = make_plan(
        &fx,
        &sim,
        &m,
        &policy,
        &ctx,
        &RemitSession::default(),
        &cfg,
        kb,
    );
    print_plan(&plan);
    apply_plan(&sim, &plan);
    let after_m = Measured::of(&render(&sim));
    // the planner's diagnosis (gate opportunities on)
    let diag = |mm: &Measured| {
        let taps = mm.strip_taps(kb);
        mix_engine::diagnose(
            &mix_engine::DiagnosisInput::new(mm.main_features(), &fx.target).with_strips(&taps),
            kb,
            &cfg.diagnosis,
        )
        .unwrap()
    };
    let before = diag(&m);
    let after = diag(&after_m);
    let state = mix_engine::plan::ConsoleState::capture(&sim).unwrap();
    let after_objective =
        mix_engine::plan::score_with(&after, cfg.target_weight, Some(&state)).objective;
    eprintln!(
        "objective {:.3} -> {:.3} predicted, {:.3} actual",
        plan.before.objective, plan.after.objective, after_objective
    );
    Outcome {
        plan,
        before,
        after,
        after_objective,
    }
}

fn moves_on<'a>(plan: &'a mix_engine::Plan, prefix: &str) -> Vec<&'a PlannedMove> {
    plan.moves()
        .filter(|m| {
            m.action
                .changes
                .iter()
                .any(|c| c.param.as_str().starts_with(prefix))
        })
        .collect()
}

#[test]
fn sibilant_vocal_gets_a_deesser() {
    let kb = KnowledgeBase::embedded();
    let pair = song_pair(SEED, |p| {
        add_sibilants(&mut p.song.tracks[12].audio, SR as f64);
        p.degraded = p.reference.clone();
        for (k, v) in [
            w("input.13/gate/model", Value::Enum("DS902".into())),
            w("input.13/gate/f", Value::Float(5000.0)),
            w("input.13/gate/range", Value::Float(6.0)),
            w("input.13/gate/on", Value::Bool(true)),
        ] {
            p.reference.writes.push((k, v));
        }
    });
    let o = plan_apply(pair, kb);
    let s0 = sibilance_score(&o.before);
    let s1 = sibilance_score(&o.after);
    eprintln!("sibilance score {s0:.3} -> {s1:.3}");
    assert!(s0 > 0.0, "the sibilant vocal is diagnosed");
    let ds = moves_on(&o.plan, "input.13/gate/");
    assert!(
        ds.iter().any(|m| m
            .action
            .changes
            .iter()
            .any(|c| c.param.as_str() == "input.13/gate/model"
                && c.to == Value::Enum("DS902".into()))),
        "a DS902 on the vocal"
    );
    assert!(
        ds.iter().all(|m| !m.needs_confirmation.is_empty()),
        "model switch is conditional"
    );
    assert!(s1 < 0.5 * s0, "sibilance {s0:.3} -> {s1:.3}");
    assert!(
        (o.after_objective - o.plan.after.objective).abs() < 0.1,
        "objective predicted {:.3} vs actual {:.3}",
        o.plan.after.objective,
        o.after_objective
    );
}

#[test]
fn sparse_bleeding_tom_gets_a_gate() {
    let kb = KnowledgeBase::embedded();
    let pair = song_pair(SEED, |p| {
        let oh = p.song.tracks[4].audio.clone();
        let keep = (3.0 * SR as f64) as usize..(3.35 * SR as f64) as usize;
        for (i, (x, y)) in p.song.tracks[3].audio.iter_mut().zip(&oh).enumerate() {
            if !keep.contains(&i) {
                *x = 0.0;
            }
            *x += 0.01 * y;
        }
        p.degraded = p.reference.clone();
        for (k, v) in [
            w("input.4/gate/threshold", Value::Float(-30.0)),
            w("input.4/gate/range", Value::Float(30.0)),
            w("input.4/gate/ratio", Value::Enum("gate".into())),
            w("input.4/gate/attack", Value::Float(1.0)),
            w("input.4/gate/hold", Value::Float(100.0)),
            w("input.4/gate/release", Value::Float(500.0)),
            w("input.4/gate/on", Value::Bool(true)),
        ] {
            p.reference.writes.push((k, v));
        }
    });
    let o = plan_apply(pair, kb);
    let bleed = |d: &Diagnosis| {
        d.problems.iter().any(|p| {
            p.id.contains("tom_cymbal_bleed")
                || p.rules.iter().any(|r| r.contains("tom_cymbal_bleed"))
        })
    };
    if !bleed(&o.before) {
        print_diagnosis("before", &o.before);
    }
    assert!(bleed(&o.before), "tom bleed diagnosed");
    let g = moves_on(&o.plan, "input.4/gate/");
    assert!(
        g.iter().any(|m| m
            .action
            .changes
            .iter()
            .any(|c| c.param.as_str() == "input.4/gate/on" && c.to == Value::Bool(true))),
        "a gate on the tom"
    );
    assert!(
        o.after_objective < o.plan.before.objective,
        "objective {:.3} -> {:.3}",
        o.plan.before.objective,
        o.after_objective
    );
    assert!(
        (o.after_objective - o.plan.after.objective).abs() < 0.1,
        "objective predicted {:.3} vs actual {:.3}",
        o.plan.after.objective,
        o.after_objective
    );
}

//! Planner validation on mix-sim's synthetic fixture (16-track song, reference and degraded scenes, see
//! `mix_sim::scenes`): the target profile is the reference render (stem targets summed from the reference strips),
//! the degraded console is measured, planned, the plan is applied in the sim and the result re-rendered.
//!
//! Prediction tolerances (single moves on the degraded console, predicted vs rendered; documented in
//! `mix_engine::plan` and the M032 log):
//! - main ERB40 `rel_db` curve (bands within 45 dB of the target's loudest band): RMS error ≤ 0.75 dB, max ≤ 2 dB;
//! - main loudness change ≤ 0.75 LU; side/mid width ≤ 0.75 dB; PLR and LRA ≤ 1.0 dB/LU;
//! - the touched strip's loudness ≤ 0.75 LU (≤ 1.5 LU for compressor moves: static curve only, no attack/release
//!   or auto-release dynamics);
//! - diagnosis distance after the move ≤ 0.08.

mod common;

use std::sync::OnceLock;

use common::*;
use mix_core::action::ParamChange;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_engine::diagnosis::window_loudness;
use mix_engine::plan::{ConsoleState, MixModel, PlanConfig, PlanStep, WingTransfer};
use mix_knowledge::KnowledgeBase;
use mix_surface::remit::{evaluate, RemitSession, Verdict, WriteRequest};
use mix_surface::Surface;

const SEED: u64 = 7;

struct Shared {
    fx: Fixture,
    degraded: Measured,
}

fn shared() -> &'static Shared {
    static S: OnceLock<Shared> = OnceLock::new();
    S.get_or_init(|| {
        let kb = KnowledgeBase::embedded();
        let fx = Fixture::new(SEED, kb);
        let degraded = Measured::of(&render(&fx.console(&fx.pair.degraded)));
        Shared { fx, degraded }
    })
}

/// Tolerances (see module docs).
const SPEC_RMS_DB: f32 = 0.75;
const SPEC_MAX_DB: f32 = 2.0;
const LOUD_LU: f32 = 0.75;
const WIDTH_DB: f32 = 0.75;
const PLR_DB: f32 = 1.0;
const STRIP_LU: f32 = 0.75;
const STRIP_DYN_LU: f32 = 1.5;
const DIST: f32 = 0.08;

#[derive(Debug)]
struct MoveError {
    spec_rms: f32,
    spec_max: f32,
    loud: f32,
    width: f32,
    plr: f32,
    lra: f32,
    strip: f32,
    dist: f32,
}

fn check_move(name: &str, changes: &[ParamChange], dynamics: bool) -> MoveError {
    let kb = KnowledgeBase::embedded();
    let s = shared();
    let (fx, m) = (&s.fx, &s.degraded);
    let deg = fx.console(&fx.pair.degraded);
    let taps = m.strip_taps(kb);
    let state = ConsoleState::capture(&deg).unwrap();
    let t = WingTransfer::default();
    let model = MixModel::new(m.main_features(), &taps, &state, &t);
    let mut st2 = state.clone();
    st2.apply(changes);
    let pred = model.predict(&st2);
    for c in changes {
        deg.write(&c.param, c.to.clone()).unwrap();
    }
    let m2 = Measured::of(&render(&deg));
    let (a, p) = (m2.main_features(), &pred.main);
    let tgt: Vec<f32> = fx
        .target
        .tonal
        .erb_mid_rel_db
        .iter()
        .map(|s| s.p50)
        .collect();
    let top = tgt.iter().cloned().fold(f32::MIN, f32::max);
    let idx: Vec<usize> = (0..tgt.len()).filter(|&b| tgt[b] > top - 45.0).collect();
    let errs: Vec<f32> = idx
        .iter()
        .map(|&b| p.spectrum.erb_mid_rel_db[b] - a.spectrum.erb_mid_rel_db[b])
        .collect();
    let spec_rms = (errs.iter().map(|e| e * e).sum::<f32>() / errs.len() as f32).sqrt();
    let spec_max = errs.iter().fold(0.0f32, |m, e| m.max(e.abs()));
    let width = |f: &mix_core::Features| f.stereo.as_ref().unwrap().width_db;
    let strip = StripId::parse(changes[0].param.head()).unwrap();
    let ps = pred.strips.iter().find(|s| s.strip == strip).unwrap();
    let as_ = m2
        .strip_taps(kb)
        .into_iter()
        .find(|s| s.strip == strip)
        .unwrap();
    let dist_p = mix_engine::diagnose(
        &mix_engine::DiagnosisInput::new(p, &fx.target).with_strips(&pred.strips),
        kb,
        &Default::default(),
    )
    .unwrap()
    .distance;
    let dist_a = fx.diagnose(&m2, kb).distance;
    let e = MoveError {
        spec_rms,
        spec_max,
        loud: window_loudness(p) - window_loudness(a),
        width: width(p) - width(a),
        plr: p.loudness.plr_db.unwrap() - a.loudness.plr_db.unwrap(),
        lra: p.loudness.lra_lu.unwrap() - a.loudness.lra_lu.unwrap(),
        strip: window_loudness(&ps.features) - window_loudness(&as_.features),
        dist: dist_p - dist_a,
    };
    eprintln!("{name:<24} {e:.2?} (distance {dist_a:.3} actual)");
    assert!(
        e.spec_rms <= SPEC_RMS_DB && e.spec_max <= SPEC_MAX_DB,
        "{name}: spectrum {e:?}"
    );
    assert!(e.loud.abs() <= LOUD_LU, "{name}: loudness {e:?}");
    assert!(e.width.abs() <= WIDTH_DB, "{name}: width {e:?}");
    assert!(
        e.plr.abs() <= PLR_DB && e.lra.abs() <= PLR_DB,
        "{name}: PLR/LRA {e:?}"
    );
    let strip_tol = if dynamics { STRIP_DYN_LU } else { STRIP_LU };
    assert!(e.strip.abs() <= strip_tol, "{name}: strip loudness {e:?}");
    assert!(e.dist.abs() <= DIST, "{name}: distance {e:?}");
    e
}

fn ch(p: &str, v: Value) -> ParamChange {
    ParamChange::new(ParamId::new(p), v)
}

fn all_moves() -> Vec<(&'static str, Vec<ParamChange>, bool)> {
    use Value::{Bool, Float};
    vec![
        (
            "bass fader -6",
            vec![ch("input.7/fader", Float(-5.0))],
            false,
        ),
        (
            "vox fader +8",
            vec![ch("input.13/fader", Float(-2.0))],
            false,
        ),
        (
            "snare fader +6",
            vec![ch("input.2/fader", Float(-6.0))],
            false,
        ),
        (
            "keys L 250 Hz 8->0",
            vec![ch("input.8/eq/band.1.gain", Float(0.0))],
            false,
        ),
        (
            "keys L 250 Hz 8->-4",
            vec![ch("input.8/eq/band.1.gain", Float(-4.0))],
            false,
        ),
        (
            "gtr low cut on",
            vec![ch("input.10/filter/lowcut.on", Bool(true))],
            false,
        ),
        (
            "OH pans +-70",
            vec![
                ch("input.5/pan", Float(-70.0)),
                ch("input.6/pan", Float(70.0)),
            ],
            false,
        ),
        ("vox comp on", vec![ch("input.13/dyn/on", Bool(true))], true),
        (
            "hat 8 kHz -6",
            vec![
                ch("input.3/eq/band.4.freq", Float(8000.0)),
                ch("input.3/eq/band.4.q", Float(1.0)),
                ch("input.3/eq/band.4.gain", Float(-6.0)),
            ],
            false,
        ),
    ]
}

/// Sampled: one fader, one EQ and one compressor move.
#[test]
fn prediction_vs_render_sampled() {
    for (name, changes, dynamics) in all_moves() {
        if matches!(
            name,
            "bass fader -6" | "keys L 250 Hz 8->-4" | "vox comp on"
        ) {
            check_move(name, &changes, dynamics);
        }
    }
}

#[test]
#[ignore = "slow: run with make check-full"]
fn prediction_vs_render_all_moves() {
    for (name, changes, dynamics) in all_moves() {
        check_move(name, &changes, dynamics);
    }
}

/// Plans for `fx`'s degraded scene under the default remit, applies the plan in the sim and returns
/// (degraded distance, predicted distance, actual distance after, degraded load, after load).
fn plan_and_apply(fx: &Fixture, m: &Measured, label: &str) -> (f32, f32, f32, f32, f32) {
    let kb = KnowledgeBase::embedded();
    let deg = fx.console(&fx.pair.degraded);
    let (policy, ctx) = remit_parts(&deg);
    let session = RemitSession::default();
    let cfg = PlanConfig::from_kb(kb);
    let t0 = std::time::Instant::now();
    let plan = make_plan(fx, &deg, m, &policy, &ctx, &session, &cfg, kb);
    eprintln!("{label}: planned in {:?}", t0.elapsed());
    print_plan(&plan);

    // remit: the default policy allows every planned change outright, or the move carries the confirmation
    // (M035: switching a gate on / a dynamics model is conditional)
    let mut st = ConsoleState::capture(&deg).unwrap();
    for mv in plan.moves() {
        for c in &mv.action.changes {
            let mut req = WriteRequest::new(c.param.clone(), st.spec(&c.param).unwrap().class);
            req.from = st.get(&c.param).cloned();
            req.to = Some(c.to.clone());
            let d = evaluate(&policy, &ctx, &session, &req);
            match d.class {
                Verdict::Allowed => {}
                Verdict::Conditional => assert!(
                    d.confirm_key
                        .as_ref()
                        .is_some_and(|k| mv.needs_confirmation.contains(k)),
                    "{}: conditional without its confirmation",
                    c.param
                ),
                Verdict::Forbidden => panic!("{}: forbidden", c.param),
            }
            st.apply_one(c);
        }
        assert!(!mv.explanation.is_empty() && !mv.problem.is_empty());
        assert!(mv.predicted.objective_after < mv.predicted.objective_before);
    }
    // structure: phases in workflow order, strip work in solo phases is inspected first
    let orders: Vec<u32> = plan.phases.iter().map(|p| p.order).collect();
    assert!(orders.windows(2).all(|w| w[0] < w[1]), "{orders:?}");
    assert!(plan.steps().any(|s| matches!(s, PlanStep::Inspect { .. })));

    apply_plan(&deg, &plan);
    let after = Measured::of(&render(&deg));
    let d0 = fx.diagnose(m, kb);
    let d1 = fx.diagnose(&after, kb);
    let load = |d: &mix_engine::Diagnosis| {
        d.problems
            .iter()
            .filter(|p| {
                !matches!(
                    p.kind,
                    mix_engine::ProblemKind::Masking
                        | mix_engine::ProblemKind::SourceProblem
                        | mix_engine::ProblemKind::Sibilance
                )
            })
            .map(|p| p.score())
            .sum::<f32>()
    };
    let r = (
        d0.distance,
        plan.after.distance,
        d1.distance,
        load(&d0),
        load(&d1),
    );
    eprintln!(
        "{label}: target distance {:.3} -> {:.3} actual ({:.3} predicted, reference scene {:.3}); target-referenced problem load {:.2} -> {:.2}",
        r.0,
        r.2,
        r.1,
        fx.diagnose(&fx.reference, kb).distance,
        r.3,
        r.4
    );
    r
}

#[test]
fn plan_reduces_target_distance_on_degraded_fixture() {
    let s = shared();
    let (d0, pred, d1, l0, l1) = plan_and_apply(&s.fx, &s.degraded, "seed 7");
    assert!(d1 < 0.5 * d0, "distance {d0:.3} -> {d1:.3}");
    assert!(
        (pred - d1).abs() < 0.1,
        "predicted {pred:.3} vs actual {d1:.3}"
    );
    assert!(l1 < 0.6 * l0, "problem load {l0:.2} -> {l1:.2}");
}

#[test]
fn plan_moves_follow_the_ground_truth() {
    // the largest level faults are undone in the right direction
    let kb = KnowledgeBase::embedded();
    let s = shared();
    let deg = s.fx.console(&s.fx.pair.degraded);
    let (policy, ctx) = remit_parts(&deg);
    let plan = make_plan(
        &s.fx,
        &deg,
        &s.degraded,
        &policy,
        &ctx,
        &RemitSession::default(),
        &PlanConfig::from_kb(kb),
        kb,
    );
    let net = |p: &str| -> f64 {
        plan.changes()
            .iter()
            .filter(|c| c.param.as_str() == p)
            .map(|c| c.to.as_f64().unwrap() - c.from.as_ref().unwrap().as_f64().unwrap())
            .sum()
    };
    assert!(
        net("input.13/fader") >= 4.0,
        "vocal {:+.1}",
        net("input.13/fader")
    );
    assert!(
        net("input.7/fader") <= -3.0,
        "bass {:+.1}",
        net("input.7/fader")
    );
    assert!(
        net("input.2/fader") >= 2.0,
        "snare {:+.1}",
        net("input.2/fader")
    );
    assert!(
        plan.changes()
            .iter()
            .any(|c| c.param.as_str() == "input.10/filter/lowcut.on" && c.to == Value::Bool(true)),
        "guitar low cut"
    );
    let _ = deg.read(&ParamId::new("input.1/fader"));
}

#[test]
#[ignore = "slow: run with make check-full"]
fn plan_reduces_target_distance_second_seed() {
    let kb = KnowledgeBase::embedded();
    let fx = Fixture::new(11, kb);
    let m = Measured::of(&render(&fx.console(&fx.pair.degraded)));
    let (d0, pred, d1, _, _) = plan_and_apply(&fx, &m, "seed 11");
    assert!(d1 < 0.6 * d0, "distance {d0:.3} -> {d1:.3}");
    assert!(
        (pred - d1).abs() < 0.15,
        "predicted {pred:.3} vs actual {d1:.3}"
    );
}

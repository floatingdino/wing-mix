//! Closed-loop controller (M033) against the in-process WING: mix-sim's synthetic 16-track song (8 s, looping)
//! through a degraded scene, measured by streaming analysers on a virtual clock, steered towards the reference
//! scene's profile (see `loop_harness`). Durations are scaled so one measurement window is one loop of the song
//! (matched section, analysis.md §2); the rest of the live constants scale with it (x 0.4).
//!
//! Planning costs ~0.2 s per planned move in dev and every batch is a full 8 s window of 17 analysed taps, so the
//! fast tests run a few batches each; the full convergence runs are ignored (`make check-full`).

mod common;
mod loop_harness;

use std::collections::BTreeMap;
use std::sync::OnceLock;

use common::{sources_of, Fixture, SECONDS};
use loop_harness::{default_policy, Rig, TapSpec};
use mix_actuator::ActuatorEvent;
use mix_core::bands::N_ERB;
use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_engine::control::{ControlConfig, ControlEvent, HoldReason, Mode, Outcome, PlanReason};
use mix_engine::plan::{
    state::{CutSettings, DynSettings, EqSettings, GateSettings},
    PlanConfig, TransferModel, WingTransfer,
};
use mix_knowledge::KnowledgeBase;
use mix_sim::scenes::Scene;
use mix_sim::synth::TRACKS;
use mix_surface::model::StripKind;
use mix_surface::remit::Policy;
use mix_surface::Surface;

const SEED: u64 = 7;

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| Fixture::new(SEED, KnowledgeBase::embedded()))
}

fn strips() -> Vec<TapSpec> {
    let kb = KnowledgeBase::embedded();
    let src = sources_of(&fixture().reference, kb);
    TRACKS
        .iter()
        .enumerate()
        .map(|(i, n)| TapSpec {
            ch: i as u8 + 1,
            name: n.to_string(),
            source: src[i].clone(),
        })
        .collect()
}

/// Test clock: one window = one loop of the song; the other durations scale with it (live window 20 s).
fn config(mode: Mode) -> ControlConfig {
    let mut c = ControlConfig::from_kb(KnowledgeBase::embedded())
        .scaled(SECONDS / 20.0)
        .with_mode(mode);
    c.window_s = SECONDS;
    c
}

fn rig_with(scene: &Scene, cfg: ControlConfig, policy: Policy) -> Rig {
    let fx = fixture();
    Rig::new(
        fx.console(scene),
        SECONDS,
        strips(),
        fx.target.clone(),
        cfg,
        policy,
    )
}

fn rig(scene: &Scene, cfg: ControlConfig) -> Rig {
    rig_with(scene, cfg, default_policy())
}

fn holding(r: &Rig) -> bool {
    r.ctl.holding().is_some() && !r.ctl.is_busy()
}

fn value(r: &Rig, p: &str) -> Value {
    r.sim.read(&ParamId::new(p)).unwrap()
}

/// The degraded scene with only the given ground-truth faults (the others at their reference values).
fn partly_degraded(params: &[&str]) -> Scene {
    let fx = fixture();
    let mut s = fx.pair.reference.clone();
    for d in &fx.pair.deltas {
        if params.contains(&d.param.as_str()) {
            s.set(&d.param, d.degraded.clone());
        }
    }
    s
}

/// No param reversed direction more than `max_reversals` times (writes of the whole session, in order, folded
/// to one value per job).
fn assert_no_hunting(r: &Rig) {
    let max = r.ctl.config().max_reversals;
    let mut per: BTreeMap<ParamId, Vec<f64>> = BTreeMap::new();
    for (_, e) in &r.act_log {
        if let ActuatorEvent::Done { changes, .. } = e {
            for c in changes {
                if let (Some(a), Some(b)) = (c.from.as_f64(), c.to.as_f64()) {
                    let v = per.entry(c.param.clone()).or_insert_with(|| vec![a]);
                    v.push(b);
                }
            }
        }
    }
    for (p, vals) in &per {
        let dirs: Vec<f64> = vals
            .windows(2)
            .map(|w| w[1] - w[0])
            .filter(|d| d.abs() > 1e-6)
            .collect();
        let rev = dirs.windows(2).filter(|w| w[0] * w[1] < 0.0).count() as u32;
        assert!(rev <= max, "{p} reversed {rev} times: {vals:?}");
        assert_eq!(r.ctl.guard().reversals(p), rev, "{p}: guard count");
    }
}

// -------------------------------------------------------------------------------------------------------------
// Convergence

/// mix-sim's seed-7 ground truth (`mix_sim::scenes::ground_truth`) after a closed-loop run: (fault, addressed,
/// what the console shows). A fault counts as addressed when the loop moved its param (or an equivalent one) at
/// least ~1/3 of the way back: levels by >= 2-2.5 dB in the right direction, the keys' 250 Hz boost by a >= 2.5 dB
/// cut/reduction around 250 Hz (or a 3 dB fader cut), switches back on, the overheads panned >= 20 % to their sides.
fn ground_truth_status(r: &Rig) -> Vec<(&'static str, bool, String)> {
    let f = |p: &str| value(r, p).as_f64().unwrap_or(f64::NAN);
    let b = |p: &str| value(r, p) == Value::Bool(true);
    let keys = |ch: u32| -> (bool, String) {
        let mut cut = 8.0 - f(&format!("input.{ch}/eq/band.1.gain"));
        for band in ["band.2", "band.3", "band.4", "low", "high"] {
            let fr = f(&format!("input.{ch}/eq/{band}.freq"));
            let g = f(&format!("input.{ch}/eq/{band}.gain"));
            if (125.0..=500.0).contains(&fr) && g < 0.0 {
                cut += -g;
            }
        }
        let fader = -12.0 - f(&format!("input.{ch}/fader"));
        (
            cut >= 2.5 || fader >= 3.0,
            format!("250 Hz cut {cut:.1} dB, fader {:+.1}", -fader),
        )
    };
    let (k8, d8) = keys(8);
    let (k9, d9) = keys(9);
    vec![
        (
            "vocal buried (-8 dB)",
            f("input.13/fader") >= -10.0 + 2.5,
            format!("vox fader {:+.1}", f("input.13/fader")),
        ),
        (
            "vocal not compressed",
            b("input.13/dyn/on"),
            format!("vox comp on {}", b("input.13/dyn/on")),
        ),
        (
            "bass too loud (+6 dB)",
            f("input.7/fader") <= 1.0 - 2.5,
            format!("bass fader {:+.1}", f("input.7/fader")),
        ),
        (
            "keys +8 dB at 250 Hz",
            k8 && k9,
            format!("KEYS L {d8}; KEYS R {d9}"),
        ),
        (
            "guitar HPF off",
            b("input.10/filter/lowcut.on"),
            format!("gtr low cut {}", b("input.10/filter/lowcut.on")),
        ),
        (
            "hats too loud (+8 dB)",
            f("input.3/fader") <= -6.0 - 2.5,
            format!("hat fader {:+.1}", f("input.3/fader")),
        ),
        (
            "snare too quiet (-6 dB)",
            f("input.2/fader") >= -12.0 + 2.0,
            format!("snare fader {:+.1}", f("input.2/fader")),
        ),
        (
            "overheads panned centre",
            f("input.5/pan") <= -20.0 && f("input.6/pan") >= 20.0,
            format!(
                "OH pans {:+.0} / {:+.0}",
                f("input.5/pan"),
                f("input.6/pan")
            ),
        ),
    ]
}

/// Seed-7 faults the closed loop does not undo yet (M036 log): the keys' +8 dB at 250 Hz moves the Other stem's
/// low-mid by ~3 dB inside the reference's ~20 dB window spread, and the snare's -6 dB shows only as Drums-stem tonal
/// deficits that the power-summed kit already reads on the matched mix (-3..-6 dB).
const NOT_YET_ADDRESSED: [&str; 2] = ["keys +8 dB at 250 Hz", "snare too quiet (-6 dB)"];

/// Full run: converge on the degraded fixture, then keep running on the (stationary) input: no further moves, no
/// hunting.
fn converge(seed_label: &str, max_s: f64) -> Rig {
    let fx = fixture();
    let mut r = rig(&fx.pair.degraded, config(Mode::Autopilot));
    // the operator accepts the conditional suggestions (compressor switch-ons are `revive:` confirmations)
    r.auto_confirm = true;
    let done = r.run_until(max_s, holding);
    let batches = r.ctl.records().len();
    r.run(4.0 * SECONDS);
    r.print_report(seed_label);
    if std::env::var("M036_VERBOSE").is_ok() {
        // the end state, rendered offline and diagnosed as the planner sees it
        let kb = KnowledgeBase::embedded();
        let m = common::Measured::of(&common::render(&r.sim));
        let taps = m.strip_taps(kb);
        let cfg = PlanConfig::from_kb(kb);
        let d = mix_engine::diagnosis::diagnose(
            &mix_engine::diagnosis::DiagnosisInput::new(m.main_features(), &fx.target)
                .with_strips(&taps),
            kb,
            &cfg.diagnosis,
        )
        .unwrap();
        common::print_diagnosis("end state", &d);
    }
    assert!(done, "never settled in {max_s} s");
    assert_eq!(r.ctl.records().len(), batches, "moved again while holding");
    r
}

#[test]
#[ignore = "slow: run with make check-full"]
fn autopilot_converges_on_degraded_fixture() {
    let r = converge("autopilot seed 7 (degraded)", 600.0);
    let curve = r.curve();
    let (first, last) = (curve[0], *curve.last().unwrap());
    assert_eq!(r.ctl.holding(), Some(HoldReason::Converged), "{curve:.3?}");
    // converged within tolerance; later windows of the unchanged mix stay within the resume hysteresis
    let at_hold = r
        .log
        .iter()
        .find_map(|(_, e)| match e {
            ControlEvent::Holding {
                reason: HoldReason::Converged,
                objective,
            } => Some(*objective),
            _ => None,
        })
        .unwrap();
    let target = r.ctl.config().target_objective;
    assert!(at_hold <= target, "{curve:.3?}");
    assert!(last <= target * r.ctl.config().resume_factor, "{curve:.3?}");
    assert!(at_hold < 0.4 * first, "{first:.3} -> {at_hold:.3}");
    assert!(
        r.ctl.records().len() <= 30,
        "{} batches",
        r.ctl.records().len()
    );
    assert_no_hunting(&r);
    // M036: within the safety limits, no false feedback trips, the ground-truth faults addressed
    let status = ground_truth_status(&r);
    for (fault, ok, detail) in &status {
        eprintln!(
            "  ground truth {fault:<26} {} ({detail})",
            if *ok { "addressed" } else { "MISSED" }
        );
    }
    assert_eq!(r.safety_refusals(), 0, "safety refusals");
    assert_eq!(r.feedback_trips(), 0, "feedback guard tripped on music");
    let missed: Vec<_> = status
        .iter()
        .filter(|s| !s.1 && !NOT_YET_ADDRESSED.contains(&s.0))
        .collect();
    assert!(missed.is_empty(), "missed ground truth: {missed:?}");
    // the ground truth is undone in the right direction
    let f = |p: &str| value(&r, p).as_f64().unwrap();
    assert!(
        f("input.7/fader") < 1.0 - 2.0,
        "bass {}",
        f("input.7/fader")
    );
    assert!(
        f("input.13/fader") > -10.0 + 2.0,
        "vox {}",
        f("input.13/fader")
    );
}

/// Sampled: the first batches of the same run (solo inspections, keep, objective falling).
#[test]
fn autopilot_first_batches_reduce_objective() {
    let fx = fixture();
    let mut c = config(Mode::Autopilot);
    c.max_batches = 2;
    let mut r = rig(&fx.pair.degraded, c);
    let done = r.run_until(60.0, holding);
    r.print_report("autopilot sampled (2 batches)");
    assert!(done);
    assert_eq!(r.ctl.holding(), Some(HoldReason::Budget));
    let curve = r.curve();
    assert!(curve.len() >= 3, "{curve:?}");
    assert!(
        curve.last().unwrap() < &(curve[0] - 0.1),
        "objective {curve:.3?}"
    );
    let kept = r
        .ctl
        .records()
        .iter()
        .filter(|b| b.outcome == Outcome::Kept)
        .count();
    assert!(kept >= 1, "{:?}", r.ctl.records());
    // solo phases were inspected through the actuator's callback, solo state restored
    assert!(r.count(|e| matches!(e, ControlEvent::Inspected { active: true, .. })) >= 1);
    assert!(r.sim.solo_state().unwrap().active.is_empty());
    assert_eq!(r.ctl.budget_used().0, 2);
    assert_no_hunting(&r);
    assert_eq!(
        r.count(|e| matches!(e, ControlEvent::SectionChanged { .. })),
        0
    );
}

#[test]
#[ignore = "slow: run with make check-full"]
fn stationary_matched_mix_does_not_hunt() {
    // the reference scene against its own profile: little or nothing to do, and nothing oscillates
    let fx = fixture();
    let mut r = rig(&fx.pair.reference, config(Mode::Autopilot));
    let done = r.run_until(300.0, holding);
    r.run(6.0 * SECONDS);
    r.print_report("autopilot on the reference scene");
    assert!(done);
    assert!(r.ctl.records().len() <= 6, "{:?}", r.ctl.records());
    assert_no_hunting(&r);
}

// -------------------------------------------------------------------------------------------------------------
// Modes

#[test]
fn suggest_mode_plans_but_writes_nothing() {
    let fx = fixture();
    let mut r = rig(&fx.pair.degraded, config(Mode::Suggest));
    let before: Vec<(ParamId, Value)> = fx
        .pair
        .degraded
        .values()
        .keys()
        .map(|p| (p.clone(), r.sim.read(p).unwrap()))
        .collect();
    let got = r.run_until(2.5 * SECONDS, |r| {
        r.log
            .iter()
            .any(|(_, e)| matches!(e, ControlEvent::Suggested { .. }))
    });
    r.run(SECONDS);
    assert!(got, "no suggestion");
    let moves = r
        .log
        .iter()
        .find_map(|(_, e)| match e {
            ControlEvent::Suggested { moves } => Some(moves.clone()),
            _ => None,
        })
        .unwrap();
    eprintln!("suggested:");
    for m in &moves {
        eprintln!(
            "  {:<14} {} (gain {:.3})",
            m.phase, m.explanation, m.predicted_gain
        );
    }
    assert!(moves.len() >= 3);
    assert!(moves
        .iter()
        .all(|m| !m.changes.is_empty() && m.predicted_gain > 0.0));
    // nothing submitted, nothing written
    assert!(r.actuator.write_log().is_empty());
    assert!(r.act_log.is_empty(), "{:?}", r.act_log);
    assert_eq!(
        r.count(|e| matches!(e, ControlEvent::BatchStarted { .. })),
        0
    );
    for (p, v) in before {
        assert_eq!(r.sim.read(&p).unwrap(), v, "{p}");
    }
    // observe: measures only
    r.ctl.set_mode(Mode::Observe);
    let n = r.log.len();
    r.run(1.5 * SECONDS);
    assert!(r.log[n..]
        .iter()
        .any(|(_, e)| matches!(e, ControlEvent::Measured { .. })));
    assert!(!r.log[n..].iter().any(|(_, e)| matches!(
        e,
        ControlEvent::Planned { .. } | ControlEvent::Suggested { .. }
    )));
    assert!(r.actuator.write_log().is_empty());
    assert_eq!(
        r.count(|e| matches!(e, ControlEvent::SectionChanged { .. })),
        0
    );
}

// -------------------------------------------------------------------------------------------------------------
// Misprediction

/// The WING transfer model with every EQ curve exaggerated 4x: the planner expects far more from an EQ move than
/// the console delivers.
struct ExaggeratedEq(WingTransfer);

impl TransferModel for ExaggeratedEq {
    fn eq_erb_db(&self, eq: &EqSettings, bus: bool) -> [f64; N_ERB] {
        self.0.eq_erb_db(eq, bus).map(|g| 4.0 * g)
    }
    fn cut_erb_db(&self, low: Option<&CutSettings>, high: Option<&CutSettings>) -> [f64; N_ERB] {
        self.0.cut_erb_db(low, high)
    }
    fn pan_gains(&self, kind: StripKind, pan: f64) -> (f64, f64) {
        self.0.pan_gains(kind, pan)
    }
    fn comp_gain_db(&self, d: &DynSettings, level_db: f64) -> f64 {
        self.0.comp_gain_db(d, level_db)
    }
    fn gate_gain_db(&self, g: &GateSettings, level_db: f64) -> f64 {
        self.0.gate_gain_db(g, level_db)
    }
}

#[test]
fn mispredicted_batch_is_reverted_and_penalised() {
    let fx = fixture();
    let mut c = config(Mode::Autopilot);
    c.inspect = false;
    c.max_batches = 3;
    let mut r = Rig::build(
        fx.console(&fx.pair.degraded),
        SECONDS,
        strips(),
        fx.target.clone(),
        c,
        default_policy(),
        |ctl| ctl.with_transfer(Box::new(ExaggeratedEq(WingTransfer::default()))),
    );
    // until the loop has reverted, re-measured and re-planned
    let reverted = r.run_until(80.0, |r| {
        r.count(|e| {
            matches!(
                e,
                ControlEvent::Planned {
                    reason: PlanReason::Reverted,
                    ..
                }
            )
        }) >= 1
    });
    r.print_report("exaggerated EQ model");
    assert!(reverted, "no revert: {:?}", r.ctl.records());
    let b = r
        .ctl
        .records()
        .iter()
        .find(|b| b.outcome == Outcome::Reverted)
        .unwrap();
    // it delivered far less than the (exaggerated) prediction
    let actual = b.before.unwrap() - b.after.unwrap();
    assert!(actual < 0.5 * b.summary.predicted_gain, "{b:?}");
    // the revert put every param of the batch back (journal undo)
    for c in &b.summary.changes {
        let from = c.from.clone().unwrap();
        let now = value(&r, c.param.as_str());
        assert!(now.approx_eq(&from, 0.05), "{}: {now} vs {from}", c.param);
        assert!(r.ctl.guard().reversals(&c.param) >= 1);
    }
    // the action type is penalised, the loop re-planned
    let pen = r
        .log
        .iter()
        .find_map(|(_, e)| match e {
            ControlEvent::BatchReverted { penalised, .. } => Some(penalised.clone()),
            _ => None,
        })
        .unwrap();
    assert!(pen.ends_with(":eq"), "{pen}");
    assert!(r.ctl.guard().type_reverts(&pen) >= 1);
    assert!(
        r.count(|e| matches!(
            e,
            ControlEvent::Planned {
                reason: PlanReason::Reverted,
                ..
            }
        )) >= 1
    );
    assert!(r.actuator.journal().entries().iter().any(|e| matches!(
        &e.entry,
        mix_surface::journal::Entry::Applied { batch } if batch.label.starts_with("undo")
    )));
    assert_no_hunting(&r);
}

// -------------------------------------------------------------------------------------------------------------
// Re-planning

#[test]
fn engineer_move_replans_and_is_respected() {
    let fx = fixture();
    let mut c = config(Mode::Autopilot);
    c.inspect = false;
    let mut r = rig(&fx.pair.degraded, c);
    // halfway through the first window the engineer pulls the vocal up a little
    r.run(4.0);
    let vox = ParamId::new("input.13/fader");
    r.sim.external_set(&vox, Value::Float(-8.0)).unwrap();
    let started = r.run_until(40.0, |r| {
        r.log
            .iter()
            .any(|(_, e)| matches!(e, ControlEvent::BatchStarted { .. }))
    });
    r.print_report("engineer move");
    assert!(started);
    assert_eq!(
        r.count(|e| matches!(e, ControlEvent::EngineerMoved { param } if *param == vox)),
        1
    );
    // the first plan came after (and because of) the move, measured on a window that started after it
    let first_plan = r
        .log
        .iter()
        .find_map(|(t, e)| match e {
            ControlEvent::Planned { reason, .. } => Some((*t, *reason)),
            _ => None,
        })
        .unwrap();
    assert_eq!(first_plan.1, PlanReason::EngineerMoved);
    assert!(first_plan.0 >= 4.0 + SECONDS, "{first_plan:?}");
    // the engineer's fader is left alone during the hold, even though the vocal is still low
    assert!(r.actuator.write_log().iter().all(|w| w.param != vox));
    assert_eq!(value(&r, "input.13/fader"), Value::Float(-8.0));
}

#[test]
fn section_change_replans() {
    let fx = fixture();
    let mut c = config(Mode::Autopilot);
    c.inspect = false;
    let mut r = rig(&fx.pair.degraded, c);
    // after the detector's warm-up, while the first batch is being verified
    r.run(14.0);
    assert_eq!(
        r.count(|e| matches!(e, ControlEvent::SectionChanged { .. })),
        0
    );
    assert!(r.count(|e| matches!(e, ControlEvent::BatchStarted { .. })) == 1);
    // the band drops to a quiet section: every source 10 dB down
    let mut quiet = fx.pair.song.clone();
    for t in &mut quiet.tracks {
        t.audio
            .iter_mut()
            .for_each(|x| *x *= 10f32.powf(-10.0 / 20.0));
    }
    r.sim.load_inputs(quiet).unwrap();
    let planned = r.run_until(25.0, |r| {
        r.log.iter().any(|(_, e)| {
            matches!(
                e,
                ControlEvent::Planned {
                    reason: PlanReason::SectionChanged,
                    ..
                }
            )
        })
    });
    r.print_report("section change");
    assert!(planned);
    let (t_sec, jump) = r
        .log
        .iter()
        .find_map(|(t, e)| match e {
            ControlEvent::SectionChanged {
                loudness_jump_lu, ..
            } => Some((*t, *loudness_jump_lu)),
            _ => None,
        })
        .expect("section change detected");
    assert!((14.0..24.0).contains(&t_sec), "{t_sec}");
    assert!(jump < -5.0, "{jump}");
    assert_eq!(
        r.count(|e| matches!(e, ControlEvent::SectionChanged { .. })),
        1
    );
    let t_plan = r
        .log
        .iter()
        .find_map(|(t, e)| match e {
            ControlEvent::Planned {
                reason: PlanReason::SectionChanged,
                ..
            } => Some(*t),
            _ => None,
        })
        .unwrap();
    assert!(t_plan >= t_sec + SECONDS);
    // the batch under verification when the music changed could not be judged
    assert!(
        r.ctl
            .records()
            .iter()
            .any(|b| b.outcome == Outcome::Inconclusive && b.note.contains("section")),
        "{:?}",
        r.ctl.records()
    );
}

// -------------------------------------------------------------------------------------------------------------
// Pending confirmations

#[test]
fn conditional_moves_wait_for_confirmation() {
    let mut policy = default_policy();
    policy
        .add_rules(
            Policy::rules_from_toml_str(
                r#"
                [[rule]]
                id = "test-confirm-faders"
                kinds = ["input"]
                classes = ["fader"]
                class = "conditional"
                priority = 1000
                "#,
            )
            .unwrap(),
        )
        .unwrap();
    let mut c = config(Mode::Autopilot);
    c.inspect = false;
    // bass too loud and vocal buried: fader work only
    let scene = partly_degraded(&["input.7/fader", "input.13/fader"]);
    let mut r = rig_with(&scene, c, policy);
    let asked = r.run_until(30.0, |r| {
        r.ctl
            .pending()
            .iter()
            .any(|p| p.key == "test-confirm-faders")
            && !r.ctl.is_busy()
    });
    r.print_report("pending confirmations");
    assert!(asked, "nothing pending");
    // (M036: compressor switch-ons wait too, under their `revive:` key)
    let key = r
        .ctl
        .pending()
        .iter()
        .find(|p| p.key == "test-confirm-faders")
        .expect("fader confirmation pending")
        .key
        .clone();
    assert!(
        r.count(|e| matches!(e, ControlEvent::ConfirmationNeeded { key: k, .. } if *k == key)) == 1
    );
    // no fader was written while unconfirmed
    assert!(r
        .actuator
        .write_log()
        .iter()
        .all(|w| !w.param.as_str().ends_with("/fader")));
    // confirm: the fader moves are planned and run
    r.ctl.confirm(&key, &mut r.actuator);
    assert!(r.ctl.pending().iter().all(|p| p.key != key));
    let moved = r.run_until(30.0, |r| {
        r.actuator
            .write_log()
            .iter()
            .any(|w| w.param.as_str().ends_with("/fader"))
    });
    r.print_report("pending confirmations (confirmed)");
    assert!(moved, "confirmed fader move never ran");
    assert!(r.count(|e| matches!(e, ControlEvent::Confirmed { .. })) == 1);
}

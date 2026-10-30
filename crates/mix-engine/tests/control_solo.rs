//! M062: the closed loop with only the main and the solo bus (one stereo stream + the monitor bus: the common real
//! setup). mix-sim's synthetic song through the degraded scene, as in `control_sim`, but the meter has no strip
//! taps: the controller estimates the strips (priors) to plan mix-level moves at once, measures them through solo
//! sweeps between batches (rate-limited, solo restored exactly) and plans on the measurements.
//!
//! Fast (make check): suggest plans from priors without writing or soloing; autopilot sweeps, books strips and
//! restores the solo state exactly every time. Slow (`make check-full`): the convergence comparison with the
//! multichannel run (`SOLO_REPORT=1` prints both).

mod common;
mod loop_harness;

use std::sync::OnceLock;

use common::{sources_of, Fixture, SECONDS};
use loop_harness::{default_policy, Rig, TapSpec};
use mix_actuator::ActuatorEvent;
use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_engine::control::{ControlConfig, ControlEvent, Mode, PlanReason};
use mix_knowledge::KnowledgeBase;
use mix_sim::scenes::Scene;
use mix_sim::synth::TRACKS;
use mix_surface::Surface;

const SEED: u64 = 7;

fn fixture() -> &'static Fixture {
    static F: OnceLock<Fixture> = OnceLock::new();
    F.get_or_init(|| Fixture::new(SEED, KnowledgeBase::embedded()))
}

fn all_strips() -> Vec<TapSpec> {
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

fn config(mode: Mode) -> ControlConfig {
    let mut c = ControlConfig::from_kb(KnowledgeBase::embedded())
        .scaled(SECONDS / 20.0)
        .with_mode(mode);
    c.window_s = SECONDS;
    c
}

/// A rig on `scene` whose console strips carry the track names (as at a soundcheck), tapping `strips` only.
fn rig(scene: &Scene, strips: Vec<TapSpec>, cfg: ControlConfig) -> Rig {
    let fx = fixture();
    let sim = fx.console(scene);
    for (i, n) in TRACKS.iter().enumerate() {
        sim.write(
            &ParamId::new(format!("input.{}/meta/name", i + 1)),
            Value::Text(n.to_string()),
        )
        .unwrap();
    }
    Rig::new(
        sim,
        SECONDS,
        strips,
        fx.target.clone(),
        cfg,
        default_policy(),
    )
}

fn solo_restores(r: &Rig) -> (usize, usize) {
    let all = r
        .act_log
        .iter()
        .filter(|(_, e)| matches!(e, ActuatorEvent::SoloRestored { .. }))
        .count();
    let exact = r
        .act_log
        .iter()
        .filter(|(_, e)| matches!(e, ActuatorEvent::SoloRestored { exact: true, .. }))
        .count();
    (all, exact)
}

fn sweeps(r: &Rig) -> Vec<(f64, String, bool)> {
    r.log
        .iter()
        .filter_map(|(t, e)| match e {
            ControlEvent::StripMeasured { strip, active, .. } => {
                Some((*t, strip.as_str().to_string(), *active))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn solo_only_suggest_plans_from_priors_and_writes_nothing() {
    let fx = fixture();
    let mut r = rig(&fx.pair.degraded, Vec::new(), config(Mode::Suggest));
    let ok = r.run_until(3.0 * SECONDS, |r| {
        r.log
            .iter()
            .any(|(_, e)| matches!(e, ControlEvent::Suggested { moves } if !moves.is_empty()))
    });
    let cov = r.ctl.coverage().clone();
    assert!(ok, "no plan from main + solo only: {:?} {:?}", cov, r.log);
    assert!(cov.estimated >= 10 && cov.tapped == 0, "{cov:?}");
    // every suggested strip move is a fader (priors carry no processing to judge)
    let moves = r
        .log
        .iter()
        .find_map(|(_, e)| match e {
            ControlEvent::Suggested { moves } => Some(moves.clone()),
            _ => None,
        })
        .unwrap();
    for m in &moves {
        if m.strip.kind_name() == "input" {
            assert!(
                m.changes
                    .iter()
                    .all(|c| c.param.as_str().ends_with("/fader")),
                "{m:?}"
            );
        }
    }
    // suggest: no write, no solo
    assert!(
        !r.act_log.iter().any(|(_, e)| matches!(
            e,
            ActuatorEvent::Started { .. } | ActuatorEvent::Soloed { .. }
        )),
        "{:?}",
        r.act_log
    );
    assert!(sweeps(&r).is_empty());
}

#[test]
fn solo_only_autopilot_sweeps_strips_and_restores_solo_exactly() {
    let fx = fixture();
    let mut cfg = config(Mode::Autopilot);
    cfg.strips.max_per_min = 30;
    cfg.strips.min_gap_s = 0.5;
    cfg.strips.dwell_s = 1.5;
    let mut r = rig(&fx.pair.degraded, Vec::new(), cfg);
    // the engineer has one strip soloed: sweeps wait for it ...
    let eng = mix_surface::surface::SoloTarget::strip(mix_core::ids::StripId::new("input", 16));
    r.sim.set_solo(&eng, true).unwrap();
    r.run(2.0 * SECONDS);
    assert!(sweeps(&r).is_empty(), "swept while the engineer soloed");
    // ... and go ahead once it is cleared
    r.sim.set_solo(&eng, false).unwrap();
    r.run_until(4.0 * SECONDS, |r| {
        sweeps(r).iter().filter(|s| s.2).count() >= 3 && !r.ctl.is_sweeping()
    });
    let sw = sweeps(&r);
    assert!(sw.iter().filter(|s| s.2).count() >= 3, "{sw:?}");
    // most valuable first: the lead vocal or a low-end source
    let first = &sw[0].1;
    assert!(
        ["input.12", "input.13", "input.1", "input.2", "input.7"].contains(&first.as_str()),
        "{sw:?}"
    );
    // every sweep restored the solo state exactly and left nothing soloed
    let (all, exact) = solo_restores(&r);
    assert!(
        all >= sw.len() && all == exact,
        "{all} restores, {exact} exact"
    );
    assert!(r.sim.solo_state().unwrap().active.is_empty());
    // the measurements joined the basis after a verification/baseline and triggered a re-plan
    r.run_until(3.0 * SECONDS, |r| {
        r.log.iter().any(|(_, e)| {
            matches!(
                e,
                ControlEvent::Planned {
                    reason: PlanReason::StripsMeasured,
                    ..
                }
            )
        })
    });
    assert!(
        r.ctl.coverage().solo_measured >= 3,
        "{:?}",
        r.ctl.coverage()
    );
    // rate limit: never two sweeps within the gap, at most 30 per rolling minute
    for w in sw.windows(2) {
        assert!(w[1].0 - w[0].0 >= 0.5, "{sw:?}");
    }
}

/// Distance reduction with main + solo only vs every strip tapped, same scene and budget (report).
#[test]
#[ignore = "slow: run with make check-full"]
fn solo_only_converges_comparably_to_multichannel() {
    let fx = fixture();
    let run = |strips: Vec<TapSpec>, label: &str| {
        // M067: no batch cap: the change budget (net deviation + pacing) applies
        let cfg = config(Mode::Autopilot);
        let mut r = rig(&fx.pair.degraded, strips, cfg);
        r.auto_confirm = true;
        r.run_until(400.0, |r| {
            r.ctl.holding().is_some() && !r.ctl.is_busy() && !r.ctl.is_sweeping()
        });
        if std::env::var("SOLO_REPORT").is_ok() {
            r.print_report(label);
            eprintln!("  coverage {:?}", r.ctl.coverage());
        }
        let c = r.ctl.curve();
        let d0 = c.first().unwrap().score.distance;
        let d1 = c.last().unwrap().score.distance;
        let (all, exact) = solo_restores(&r);
        assert_eq!(all, exact, "{label}: solo not restored exactly");
        assert!(r.sim.solo_state().unwrap().active.is_empty());
        assert_ne!(
            r.ctl.holding(),
            Some(mix_engine::control::HoldReason::Budget),
            "{label}: {:?}",
            r.ctl.budget_status()
        );
        (d0, d1, sweeps(&r).len(), r.ctl.records().len())
    };
    let multi = run(all_strips(), "multichannel");
    let solo = run(Vec::new(), "main + solo only");
    eprintln!(
        "main distance: multichannel {:.3} -> {:.3} ({} batches); main + solo only {:.3} -> {:.3} ({} batches, {} sweeps)",
        multi.0, multi.1, multi.3, solo.0, solo.1, solo.3, solo.2
    );
    assert!(solo.1 < solo.0, "solo-only run did not reduce the distance");
    let red_m = (multi.0 - multi.1) / multi.0;
    let red_s = (solo.0 - solo.1) / solo.0;
    assert!(
        red_s >= 0.5 * red_m,
        "solo-only reduction {red_s:.2} vs multichannel {red_m:.2}"
    );
}

/// M067: a console without solo (or a session that may not solo): main only, no sweeps. Priors alone vs priors
/// refined by the console's meters (level, activity, gain reduction).
fn no_solo_run(meters: bool, label: &str, until_s: f64) -> Rig {
    let fx = fixture();
    let mut cfg = config(Mode::Autopilot);
    cfg.inspect = false;
    cfg.strips.sweep = false;
    cfg.strips.meters = meters;
    let mut r = rig(&fx.pair.degraded, Vec::new(), cfg);
    r.auto_confirm = true;
    r.run_until(until_s, |r| r.ctl.holding().is_some() && !r.ctl.is_busy());
    if std::env::var("SOLO_REPORT").is_ok() {
        r.print_report(label);
        eprintln!("  coverage {:?}", r.ctl.coverage());
    }
    assert!(
        !r.act_log
            .iter()
            .any(|(_, e)| matches!(e, ActuatorEvent::Soloed { .. })),
        "{label}: soloed"
    );
    r
}

#[test]
fn no_solo_meters_refine_the_estimates() {
    let fx = fixture();
    let mut cfg = config(Mode::Suggest);
    cfg.inspect = false;
    cfg.strips.sweep = false;
    let mut r = rig(&fx.pair.degraded, Vec::new(), cfg);
    r.run_until(3.0 * SECONDS, |r| r.ctl.measurement().is_some());
    let cov = r.ctl.coverage().clone();
    assert!(cov.estimated >= 10 && cov.meter_refined >= 10, "{cov:?}");
    let m = r.ctl.measurement().unwrap();
    let st = r.ctl.meter_stats().unwrap();
    assert!(st.frames > 0);
    // the degraded bass (+9 dB) reads loud and the kick's compressor is metered
    let ml = mix_engine::diagnosis::window_loudness(&m.main);
    let lvl = |c: u32| {
        m.strips
            .iter()
            .find(|t| t.strip.index() == Some(c))
            .map(|t| mix_engine::diagnosis::window_loudness(&t.features) - ml)
            .unwrap()
    };
    assert!(lvl(7) > lvl(13), "bass {} vs vocal {}", lvl(7), lvl(13));
    // level checks run on the meter-refined strips (with reduced confidence); spectra are not judged
    let d = r.ctl.diagnosis().unwrap();
    let strip_level = d.problems.iter().filter(|p| {
        matches!(p.scope.subject, mix_engine::diagnosis::Subject::Strip { .. })
            && p.kind == mix_engine::diagnosis::ProblemKind::Level
    });
    assert!(strip_level.clone().count() > 0, "{:?}", d.problems);
    assert!(strip_level.clone().all(|p| p.confidence <= 0.5));
    assert!(!d.problems.iter().any(|p| matches!(
        p.scope.subject,
        mix_engine::diagnosis::Subject::Strip { .. }
    ) && p.kind == mix_engine::diagnosis::ProblemKind::Tonal));
}

/// Distance reduction without solo: priors alone vs priors refined by the console meters (report + check).
#[test]
#[ignore = "slow: run with make check-full"]
fn no_solo_meters_improve_over_priors_alone() {
    let run = |meters: bool, label: &str| {
        let r = no_solo_run(meters, label, 400.0);
        let c = r.ctl.curve();
        let d0 = c.first().unwrap().score.distance;
        let d1 = c.last().unwrap().score.distance;
        (d0, d1, r.ctl.records().len(), r.ctl.holding())
    };
    let pri = run(false, "no solo, priors only");
    let met = run(true, "no solo, priors + meters");
    eprintln!(
        "main distance without solo: priors {:.3} -> {:.3} ({} batches, {:?}); priors + meters {:.3} -> {:.3} ({} batches, {:?})",
        pri.0, pri.1, pri.2, pri.3, met.0, met.1, met.2, met.3
    );
    assert!(met.1 < met.0);
    assert!(
        met.1 <= pri.1 + 0.01,
        "meters {:.3} vs priors {:.3}",
        met.1,
        pri.1
    );
    assert_ne!(met.3, Some(mix_engine::control::HoldReason::Budget));
}

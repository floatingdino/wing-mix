//! Planner unit tests on the generic demo console with synthetic features (the closed-loop sim validation is in
//! `tests/plan_sim.rs`), including the random-policy remit property test.

use std::collections::BTreeMap;

use mix_core::features::Window;
use mix_core::ids::{ParamId, StripId};
use mix_core::profile::StemKind;
use mix_core::value::Value;
use mix_knowledge::KnowledgeBase;
use mix_surface::model::{ParamClass, Role, StripKind};
use mix_surface::remit::{
    detect_topology, effective_roles, evaluate, evaluate_inspection, Envelope, InspectionKind,
    Policy, RemitContext, RemitSession, Rule, ScoringDetector, Verdict, WriteRequest,
};
use mix_surface::testkit::FakeSurface;
use mix_surface::Surface;

use super::*;
use crate::diagnosis::synth;

fn policy() -> Policy {
    Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/remit/wing-default.toml"),
    )
    .unwrap()
}

fn context(policy: &Policy, s: &FakeSurface) -> RemitContext {
    let topo = s.topology().unwrap();
    let det = ScoringDetector::new(policy.detection.clone());
    let found = detect_topology(&topo, &policy.detection, &det);
    let roles = effective_roles(&found, &BTreeMap::new());
    RemitContext::from_topology(&topo, &roles, policy, &s.info().capabilities, false)
}

/// Demo console strips for the synthetic stems: Vocals → input.3 (VOX), Bass → input.1, Guitar → input.4.
fn strip_for(k: StemKind) -> StripId {
    match k {
        StemKind::Vocals => StripId::new("input", 3),
        StemKind::Bass => StripId::new("input", 1),
        _ => StripId::new("input", 4),
    }
}

struct Setup {
    surface: FakeSurface,
    target: mix_core::TargetProfile,
    main: Features,
    taps: Vec<StripTap>,
}

/// Synthetic mix on target, then the vocal fader pulled down 6 dB (main and strip features follow through the
/// effect model, so they stay consistent).
fn setup(kb: &KnowledgeBase) -> Setup {
    degrade(kb, &[ParamChange::new("input.3/fader", -6.0)])
}

/// [`on_target`] with `changes` applied to the console and, through the effect model, to the features.
fn degrade(kb: &KnowledgeBase, changes: &[ParamChange]) -> Setup {
    let s = on_target(kb);
    let state = ConsoleState::capture(&s.surface).unwrap();
    let t = WingTransfer::default();
    let model = MixModel::new(&s.main, &s.taps, &state, &t);
    let mut st2 = state.clone();
    st2.apply(changes);
    let pred = model.predict(&st2);
    for c in changes {
        s.surface.write(&c.param, c.to.clone()).unwrap();
    }
    Setup {
        main: pred.main,
        taps: pred.strips,
        ..s
    }
}

/// Synthetic mix exactly on target.
fn on_target(kb: &KnowledgeBase) -> Setup {
    let surface = FakeSurface::demo();
    let target = synth::profile_with_stems();
    let main = synth::features_matching(&target, Window::Session);
    let mut taps = synth::strips_matching(&target, Window::Session, kb);
    for (t, k) in taps.iter_mut().zip(synth::STEMS) {
        t.strip = strip_for(k);
    }
    Setup {
        surface,
        target,
        main,
        taps,
    }
}

/// Buried vocal plus a guitar EQ boost of 6 dB at 400 Hz (muddy guitar).
fn muddy_guitar(kb: &KnowledgeBase) -> Setup {
    degrade(
        kb,
        &[
            ParamChange::new("input.3/fader", -6.0),
            ParamChange::new("input.4/eq/band.1.freq", 400.0),
            ParamChange::new("input.4/eq/band.1.q", 1.0),
            ParamChange::new("input.4/eq/band.1.gain", 9.0),
        ],
    )
}

fn run(
    s: &Setup,
    policy: &Policy,
    session: &RemitSession,
    cfg: &PlanConfig,
    kb: &KnowledgeBase,
) -> Plan {
    let state = ConsoleState::capture(&s.surface).unwrap();
    let ctx = context(policy, &s.surface);
    let input = PlanInput {
        main: &s.main,
        strips: &s.taps,
        target: &s.target,
        state: &state,
        remit: Remit {
            policy,
            ctx: &ctx,
            session,
        },
    };
    plan(&input, kb, cfg).unwrap()
}

#[test]
fn raises_the_buried_vocal() {
    let kb = KnowledgeBase::embedded();
    let s = setup(kb);
    let p = run(
        &s,
        &policy(),
        &RemitSession::default(),
        &PlanConfig::from_kb(kb),
        kb,
    );
    let first = p.moves().next().unwrap_or_else(|| panic!("a move: {p:#?}"));
    assert!(
        p.after.objective < p.before.objective - 0.05,
        "{:?} -> {:?}",
        p.before,
        p.after
    );
    let vox_up: f64 = p
        .changes()
        .iter()
        .filter(|c| c.param.as_str() == "input.3/fader")
        .map(|c| c.to.as_f64().unwrap() - c.from.as_ref().unwrap().as_f64().unwrap())
        .sum();
    assert!(
        vox_up > 3.0,
        "vocal fader +{vox_up:.1} dB; plan {:#?}",
        p.moves().collect::<Vec<_>>()
    );
    assert!(!first.explanation.is_empty() && !first.problem.is_empty());
    assert!(first.predicted.objective_after < first.predicted.objective_before);
    // serialisable for the GUI
    let j = serde_json::to_string(&p).unwrap();
    let back: Plan = serde_json::from_str(&j).unwrap();
    assert_eq!(back, p);
}

#[test]
fn on_target_mix_needs_no_moves() {
    let kb = KnowledgeBase::embedded();
    let s = on_target(kb);
    let p = run(
        &s,
        &policy(),
        &RemitSession::default(),
        &PlanConfig::from_kb(kb),
        kb,
    );
    assert_eq!(p.moves().count(), 0, "{:#?}", p.moves().collect::<Vec<_>>());
}

#[test]
fn phases_are_ordered_and_solo_precedes_strip_work() {
    let kb = KnowledgeBase::embedded();
    let s = muddy_guitar(kb);
    let p = run(
        &s,
        &policy(),
        &RemitSession::default(),
        &PlanConfig::from_kb(kb),
        kb,
    );
    assert!(
        p.steps().any(|st| matches!(st, PlanStep::Inspect { .. })),
        "the guitar EQ work is inspected soloed: {:#?}",
        p.moves().map(|m| &m.action.label).collect::<Vec<_>>()
    );
    let orders: Vec<u32> = p.phases.iter().map(|x| x.order).collect();
    assert!(orders.windows(2).all(|w| w[0] < w[1]), "{orders:?}");
    for ph in &p.phases {
        let idx = PHASES.iter().position(|x| *x == ph.phase).unwrap();
        let mut soloed: Vec<StripId> = Vec::new();
        for st in &ph.steps {
            match st {
                PlanStep::Inspect { strip, .. } => soloed.push(strip.clone()),
                PlanStep::Move(m) if SOLO_PHASES.contains(&idx) => {
                    assert!(
                        soloed.contains(&m.strip),
                        "{} without inspect in {}",
                        m.action.label,
                        ph.phase
                    )
                }
                PlanStep::Move(_) => {}
            }
        }
    }
}

#[test]
fn solo_forbidden_means_no_inspect_steps() {
    let kb = KnowledgeBase::embedded();
    let s = muddy_guitar(kb);
    let mut pol = policy();
    pol.inspection.solo = Verdict::Forbidden;
    let p = run(
        &s,
        &pol,
        &RemitSession::default(),
        &PlanConfig::from_kb(kb),
        kb,
    );
    assert!(p.steps().all(|st| !matches!(st, PlanStep::Inspect { .. })));
    assert!(p
        .moves()
        .any(|m| SOLO_PHASES.contains(&PHASES.iter().position(|x| *x == m.phase).unwrap())));
    assert!(
        p.notes.iter().any(|n| n.contains("solo inspection")),
        "{:?}",
        p.notes
    );
}

#[test]
fn envelope_clamps_moves() {
    let spec = ParamSpec::float("input.1/fader", ParamClass::Fader, Unit::Db, -144.0, 10.0);
    let env = Envelope {
        max_delta_db: Some(3.0),
        ..Default::default()
    };
    assert_eq!(
        clamp_envelope(&spec, &Value::Float(-10.0), &Value::Float(-2.0), &env),
        Some(Value::Float(-7.0))
    );
    let eq = ParamSpec::float(
        "input.1/eq/band.1.gain",
        ParamClass::Eq,
        Unit::Db,
        -15.0,
        15.0,
    );
    let env = Envelope {
        max_boost_db: Some(2.0),
        max_cut_db: Some(4.0),
        ..Default::default()
    };
    assert_eq!(
        clamp_envelope(&eq, &Value::Float(0.0), &Value::Float(5.0), &env),
        Some(Value::Float(2.0))
    );
    assert_eq!(
        clamp_envelope(&eq, &Value::Float(0.0), &Value::Float(-6.0), &env),
        Some(Value::Float(-4.0))
    );
    let pan = ParamSpec::float("input.1/pan", ParamClass::Pan, Unit::Pan, -100.0, 100.0);
    let env = Envelope {
        max_delta_pct: Some(0.0),
        ..Default::default()
    };
    assert_eq!(
        clamp_envelope(&pan, &Value::Float(0.0), &Value::Float(50.0), &env),
        None
    );
}

#[test]
fn same_param_moves_keep_selection_order_across_phases() {
    // a boost (tonal_eq) selected before a cut (corrective_eq) of the same band must not be reordered
    let mk = |label: &str, to: f64| PlannedMove {
        action: Action {
            label: label.into(),
            changes: vec![ParamChange {
                param: ParamId::new("input.1/eq/band.1.gain"),
                from: Some(Value::Float(0.0)),
                to: Value::Float(to),
            }],
        },
        strip: StripId::new("input", 1),
        phase: String::new(),
        problem: String::new(),
        rules: vec![],
        why: String::new(),
        explanation: String::new(),
        predicted: PredictedEffect {
            objective_before: 1.0,
            objective_after: 0.9,
            distance_before: 1.0,
            distance_after: 0.9,
            problem: None,
            strip_level_db: BTreeMap::new(),
            main_level_db: 0.0,
        },
        cost: 1.0,
        benefit: 0.1,
        needs_confirmation: vec![],
    };
    let kb = KnowledgeBase::embedded();
    let s = setup(kb);
    let state = ConsoleState::capture(&s.surface).unwrap();
    let pol = policy();
    let ctx = context(&pol, &s.surface);
    let session = RemitSession::default();
    let input = PlanInput {
        main: &s.main,
        strips: &s.taps,
        target: &s.target,
        state: &state,
        remit: Remit {
            policy: &pol,
            ctx: &ctx,
            session: &session,
        },
    };
    let chosen = vec![(5usize, mk("boost", 2.0)), (3usize, mk("cut", -2.0))];
    let mut notes = Vec::new();
    let phases = order(&chosen, kb, &input, &PlanConfig::from_kb(kb), &mut notes);
    let labels: Vec<String> = phases
        .iter()
        .flat_map(|p| p.steps.iter())
        .filter_map(|s| match s {
            PlanStep::Move(m) => Some(m.action.label.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        labels.len(),
        1,
        "merged into one move of the band: {labels:?}"
    );
    let last = phases
        .iter()
        .flat_map(|p| p.steps.iter())
        .find_map(|s| match s {
            PlanStep::Move(m) => Some(m.action.changes[0].to.clone()),
            _ => None,
        });
    assert_eq!(
        last,
        Some(Value::Float(-2.0)),
        "final value is the later selection"
    );
}

#[test]
fn state_reads_typed_settings() {
    let s = FakeSurface::demo();
    let st = ConsoleState::capture(&s).unwrap();
    let v = st.settings(&StripId::new("input", 3));
    assert_eq!(v.fader_db, Some(0.0));
    assert!(v.eq.as_ref().is_some_and(|e| e.on && e.bands.len() == 2));
    assert!(v.lowcut.is_some_and(|c| !c.on && c.freq_hz == 80.0));
    assert!(v.dynamics.is_some_and(|d| !d.on && d.ratio == 3.0));
    let soul = {
        let s2 = FakeSurface::demo();
        s2.write(
            &ParamId::new("input.3/eq/model"),
            Value::Enum("SOUL".into()),
        )
        .unwrap();
        ConsoleState::capture(&s2).unwrap()
    };
    assert!(
        soul.settings(&StripId::new("input", 3)).eq.is_none(),
        "non-standard EQ model is not modelled"
    );
}

// ---------------------------------------------------------------------------------------------------------
// Property test: random remit policies never yield a forbidden param (nor a conditional one when disallowed).

/// SplitMix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn chance(&mut self, p: f64) -> bool {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64 <= p
    }
    fn subset<T: Clone>(&mut self, items: &[T]) -> Option<Vec<T>> {
        if self.chance(0.4) {
            return None;
        }
        let v: Vec<T> = items.iter().filter(|_| self.chance(0.5)).cloned().collect();
        (!v.is_empty()).then_some(v)
    }
}

const VERDICTS: [Verdict; 3] = [Verdict::Forbidden, Verdict::Conditional, Verdict::Allowed];

fn random_policy(rng: &mut Rng) -> Policy {
    let mut p = policy();
    p.rules.clear();
    p.inspection.solo = VERDICTS[rng.below(3)];
    if rng.chance(0.3) {
        p.locks.roles.push(Role::Foh);
    }
    let classes = [
        ParamClass::Fader,
        ParamClass::Pan,
        ParamClass::Eq,
        ParamClass::Filter,
        ParamClass::Dynamics,
        ParamClass::Trim,
        ParamClass::SendLevel,
        ParamClass::Mute,
        ParamClass::Width,
    ];
    let kinds = [
        StripKind::Input,
        StripKind::Aux,
        StripKind::Bus,
        StripKind::Main,
    ];
    let globs = [
        "input.3/**",
        "input.*/eq/**",
        "**/fader",
        "input.1/**",
        "**/filter/*",
        "main.1/**",
    ];
    let n = rng.below(8);
    let rules: Vec<Rule> = (0..n)
        .map(|i| Rule {
            id: format!("r{i}"),
            kinds: rng.subset(&kinds),
            roles: if rng.chance(0.3) {
                Some(vec![Role::Foh])
            } else {
                None
            },
            classes: rng.subset(&classes),
            params: if rng.chance(0.4) {
                Some(vec![globs[rng.below(globs.len())].to_string()])
            } else {
                None
            },
            class: Some(VERDICTS[rng.below(3)]),
            priority: rng.below(4) as i64,
            envelope: rng.chance(0.3).then(|| Envelope {
                max_delta_db: Some(1.0 + rng.below(6) as f64),
                max_boost_db: Some(rng.below(4) as f64),
                max_cut_db: Some(rng.below(8) as f64),
                max_delta_pct: Some(rng.below(40) as f64),
                max_delta_oct: Some(0.5),
            }),
            ..Default::default()
        })
        .collect();
    p.add_rules(rules).unwrap();
    p
}

#[test]
fn random_policies_never_yield_forbidden_params() {
    let kb = KnowledgeBase::embedded();
    let s = muddy_guitar(kb);
    let state = ConsoleState::capture(&s.surface).unwrap();
    let mut rng = Rng(0x5eed);
    let mut proposed = 0usize;
    let mut cfg = PlanConfig::from_kb(kb);
    cfg.max_moves = 6;
    for round in 0..120 {
        let pol = random_policy(&mut rng);
        let ctx = context(&pol, &s.surface);
        let mut session = RemitSession::default();
        if rng.chance(0.3) {
            for r in &pol.rules {
                if rng.chance(0.5) {
                    session.confirm(r.id.clone());
                }
            }
        }
        cfg.allow_conditional = rng.chance(0.5);
        let input = PlanInput {
            main: &s.main,
            strips: &s.taps,
            target: &s.target,
            state: &state,
            remit: Remit {
                policy: &pol,
                ctx: &ctx,
                session: &session,
            },
        };
        let p = plan(&input, kb, &cfg).unwrap();
        // replay: every change is evaluated against the console as it is when the move runs
        let mut st = state.clone();
        for m in p.moves() {
            for c in &m.action.changes {
                let spec = st.spec(&c.param).unwrap();
                let mut req = WriteRequest::new(c.param.clone(), spec.class);
                req.from = st.get(&c.param).cloned();
                req.to = Some(c.to.clone());
                let d = evaluate(&pol, &ctx, &session, &req);
                assert_ne!(
                    d.class,
                    Verdict::Forbidden,
                    "round {round}: {} proposed: {}",
                    c.param,
                    d.detail
                );
                if !cfg.allow_conditional {
                    assert_eq!(
                        d.class,
                        Verdict::Allowed,
                        "round {round}: {} conditional",
                        c.param
                    );
                } else if d.class == Verdict::Conditional {
                    if let Some(k) = &d.confirm_key {
                        assert!(
                            m.needs_confirmation.contains(k),
                            "round {round}: {} not flagged",
                            c.param
                        );
                    }
                }
                proposed += 1;
            }
            st.apply(&m.action.changes);
        }
        for step in p.steps() {
            if let PlanStep::Inspect { strip, .. } = step {
                let d = evaluate_inspection(&pol, &ctx, InspectionKind::Solo, strip);
                assert_ne!(d.class, Verdict::Forbidden, "round {round}: solo {strip}");
            }
        }
    }
    assert!(
        proposed > 20,
        "the random policies allowed something at least sometimes ({proposed})"
    );
}

/// M036 plan hygiene: one move per param (split parts of one move labelled `(k/n)`, consecutive), titles name the
/// move's actual change, per-move predictions chain in execution order, boosts inside the safety limits.
#[test]
fn plan_hygiene_one_move_per_param_and_consistent_labels() {
    let kb = KnowledgeBase::embedded();
    let s = muddy_guitar(kb);
    let cfg = PlanConfig::from_kb(kb);
    let p = run(&s, &policy(), &RemitSession::default(), &cfg, kb);
    let moves: Vec<&PlannedMove> = p.moves().collect();
    assert!(!moves.is_empty());
    let mut seen: BTreeMap<ParamId, usize> = BTreeMap::new();
    let sc = cfg.safety.clone().unwrap();
    let st = ConsoleState::capture(&s.surface).unwrap();
    for (i, m) in moves.iter().enumerate() {
        let part = m.action.label.ends_with(')') && m.action.label.contains('/');
        for c in &m.action.changes {
            if let Some(&j) = seen.get(&c.param) {
                assert!(
                    part && j + 1 == i,
                    "{} in moves {j} and {i}: {:?}",
                    c.param,
                    moves.iter().map(|m| &m.action.label).collect::<Vec<_>>()
                );
            }
            seen.insert(c.param.clone(), i);
            // the label names the change
            let (a, b) = (
                c.from.as_ref().and_then(Value::as_f64).unwrap_or(0.0),
                c.to.as_f64().unwrap_or(0.0),
            );
            if c.param.leaf() == "fader" {
                assert!(
                    m.action.label.contains(&format!("{:+.1} dB", b - a)),
                    "{} vs {:?}",
                    m.action.label,
                    c
                );
                // inside the per-move boost limit
                let spec = st.spec(&c.param).unwrap();
                assert!(b - a <= sc.move_limit_db(spec, false).unwrap() + 1e-9);
            }
        }
        // the explanation starts with the label
        assert!(
            m.explanation.starts_with(&m.action.label),
            "{}",
            m.explanation
        );
        if i > 0 {
            let prev = &moves[i - 1].predicted;
            assert!((prev.objective_after - m.predicted.objective_before).abs() < 1e-9);
        }
    }
    assert!((moves.last().unwrap().predicted.objective_after - p.after.objective).abs() < 1e-9);
}

#[test]
fn split_moves_respect_the_per_move_limit() {
    // vocal 6 dB down: the plan raises it in parts of at most +3 dB
    let kb = KnowledgeBase::embedded();
    let s = setup(kb);
    let p = run(
        &s,
        &policy(),
        &RemitSession::default(),
        &PlanConfig::from_kb(kb),
        kb,
    );
    let parts: Vec<&PlannedMove> = p
        .moves()
        .filter(|m| {
            m.action
                .changes
                .iter()
                .any(|c| c.param.as_str() == "input.3/fader")
        })
        .collect();
    let total: f64 = parts
        .iter()
        .flat_map(|m| &m.action.changes)
        .filter(|c| c.param.as_str() == "input.3/fader")
        .map(|c| c.to.as_f64().unwrap() - c.from.as_ref().unwrap().as_f64().unwrap())
        .sum();
    assert!(total > 3.0, "vocal +{total:.1}");
    assert!(parts.len() >= 2, "{parts:#?}");
    for (k, m) in parts.iter().enumerate() {
        assert!(m
            .action
            .label
            .ends_with(&format!("({}/{})", k + 1, parts.len())));
    }
}

/// A console without solo: no inspect steps and no "solo not allowed" note (nothing was denied; it does not exist).
#[test]
fn no_solo_capability_means_no_inspect_steps() {
    let kb = KnowledgeBase::embedded();
    let s = muddy_guitar(kb);
    let mut caps = s.surface.info().capabilities;
    caps.solo = false;
    s.surface.set_capabilities(caps);
    let p = run(
        &s,
        &policy(),
        &RemitSession::default(),
        &PlanConfig::from_kb(kb),
        kb,
    );
    assert!(p.moves().count() > 0, "moves are still planned");
    assert!(p.steps().all(|st| !matches!(st, PlanStep::Inspect { .. })));
    assert!(p.notes.iter().all(|n| !n.contains("solo")), "{:?}", p.notes);
}

/// The planner runs on a non-WING transfer model (cookbook EQ shapes) and still fixes the muddy guitar.
#[test]
fn plans_with_a_non_wing_eq_transfer() {
    let kb = KnowledgeBase::embedded();
    let s = muddy_guitar(kb);
    let state = ConsoleState::capture(&s.surface).unwrap();
    let pol = policy();
    let ctx = context(&pol, &s.surface);
    let session = RemitSession::default();
    let input = PlanInput {
        main: &s.main,
        strips: &s.taps,
        target: &s.target,
        state: &state,
        remit: Remit {
            policy: &pol,
            ctx: &ctx,
            session: &session,
        },
    };
    let t = GenericTransfer::new(48_000.0);
    let p = plan_with(&input, kb, &PlanConfig::from_kb(kb), &t).unwrap();
    assert!(p.after.objective < p.before.objective, "{:?}", p.before);
    assert!(
        p.changes()
            .iter()
            .any(|c| c.param.as_str().starts_with("input.4/eq/band.")
                && c.param.leaf().ends_with("gain")),
        "{:#?}",
        p.moves().map(|m| &m.action.label).collect::<Vec<_>>()
    );
}

/// EQ band shape is read per band from `eq/band.N.type` (X32-style consoles: any band can be a shelf or a cut).
#[test]
fn band_kinds_come_from_the_type_param() {
    use mix_surface::model::{Strip, Topology};
    let id = StripId::new("input", 1);
    let mut params = vec![(
        ParamSpec::boolean("input.1/eq/on", ParamClass::Eq),
        Value::Bool(true),
    )];
    for (k, ty) in ["LCUT", "LSHV", "VEQ", "HSHV"].iter().enumerate() {
        let k = k + 1;
        params.push((
            ParamSpec::enumeration(
                format!("input.1/eq/band.{k}.type"),
                ParamClass::EqModel,
                &["LCUT", "LSHV", "PEQ", "VEQ", "HSHV", "HCUT"],
            ),
            Value::Enum((*ty).into()),
        ));
        for (leaf, u, v) in [
            ("gain", Unit::Db, 3.0),
            ("freq", Unit::Hz, 100.0 * k as f64),
            ("q", Unit::Q, 1.0),
        ] {
            params.push((
                ParamSpec::float(
                    format!("input.1/eq/band.{k}.{leaf}"),
                    ParamClass::Eq,
                    u,
                    0.1,
                    20000.0,
                ),
                Value::Float(v),
            ));
        }
    }
    let topo = Topology {
        strips: vec![Strip::new(id.clone(), StripKind::Input, "x")],
    };
    let eq = ConsoleState::from_parts(topo, params)
        .settings(&id)
        .eq
        .unwrap();
    let kinds: Vec<state::BandKind> = eq.bands.iter().map(|(_, b)| b.kind).collect();
    use state::BandKind::*;
    assert_eq!(kinds, vec![LowCut, LowShelf, Peq, HighShelf]);
    assert!(eq.band(state::BandSlot::Low).is_none());
}

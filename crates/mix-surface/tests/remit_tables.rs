//! Drives the M004 test tables (data/remit/tests/*.toml) against the default WING policy.

use mix_surface::model::{Block, ParamClass, Role, StripKind};
use mix_surface::remit::detect::{
    confidence_band, BusEvidence, Evidence, InputEvidence, MainEvidence,
};
use mix_surface::remit::{
    detect_pairs, evaluate, evaluate_inspection, InspectionKind, Policy, RemitContext, RemitSend,
    RemitSession, RemitStrip, RoleDetector, Rule, ScoringDetector, WriteRequest,
};
use mix_surface::{ParamId, StripId, Value};
use serde::Deserialize;
use std::path::PathBuf;

fn data(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/remit")
        .join(rel)
}

fn default_policy() -> Policy {
    Policy::load(&data("wing-default.toml")).expect("default policy loads")
}

// ---------------------------------------------------------------- remit-eval.toml

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EvalTable {
    format: String,
    surface: SurfaceFlags,
    strip: Vec<TStrip>,
    case: Vec<TCase>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SurfaceFlags {
    solo_destructive: bool,
    external_fx_models: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TStrip {
    id: String,
    kind: StripKind,
    role: Option<Role>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    chain: Vec<Block>,
    #[serde(default)]
    sends: Vec<TSend>,
    #[serde(default)]
    feeds_protected: bool,
    #[serde(default)]
    hosts: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TSend {
    dest: String,
    tap_after: Block,
    active: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TCase {
    id: String,
    param: String,
    class: Option<ParamClass>,
    from: Option<toml::Value>,
    to: Option<toml::Value>,
    #[serde(default)]
    confirmed: Vec<String>,
    #[serde(default)]
    overlay: Vec<Rule>,
    action: Option<String>,
    solo_destructive: Option<bool>,
    expect: TExpect,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TExpect {
    class: String,
    reason: String,
    rule: Option<String>,
    coupled: Option<bool>,
    max_delta_pct: Option<f64>,
}

fn to_value(v: &toml::Value) -> Value {
    match v {
        toml::Value::Float(f) => Value::Float(*f),
        toml::Value::Integer(i) => Value::Float(*i as f64),
        toml::Value::Boolean(b) => Value::Bool(*b),
        toml::Value::String(s) => Value::Enum(s.clone()),
        other => panic!("unsupported value {other}"),
    }
}

fn build_ctx(t: &EvalTable) -> RemitContext {
    let mut ctx = RemitContext {
        solo_destructive: t.surface.solo_destructive,
        solo_available: true,
        external_fx_models: t.surface.external_fx_models.clone(),
        ..Default::default()
    };
    for s in &t.strip {
        let id = StripId::parse(&s.id).expect("strip id");
        let mut r = RemitStrip::new(id, s.kind, s.role.unwrap_or(Role::default_for(s.kind)));
        r.tags = s.tags.clone();
        r.chain = s.chain.clone();
        r.sends = s
            .sends
            .iter()
            .map(|x| RemitSend {
                dest: StripId::parse(&x.dest).unwrap(),
                tap_after: x.tap_after.clone(),
                active: x.active,
            })
            .collect();
        r.feeds_protected = s.feeds_protected;
        r.hosts = s
            .hosts
            .iter()
            .map(|h| {
                let (strip, block) = h.split_once('/').expect("host = strip/insert.N");
                (StripId::parse(strip).unwrap(), Block::parse(block))
            })
            .collect();
        ctx.insert(r);
    }
    // stereo pairs from the strip names (M039)
    let ids: Vec<(StripId, StripKind, &str)> = t
        .strip
        .iter()
        .map(|s| (StripId::parse(&s.id).unwrap(), s.kind, s.name.as_str()))
        .collect();
    for (id, pair) in detect_pairs(ids.iter().map(|(i, k, n)| (i, *k, *n))) {
        ctx.strips.get_mut(&id).unwrap().pair = Some(pair);
    }
    ctx
}

fn eval_table() -> EvalTable {
    let text = std::fs::read_to_string(data("tests/remit-eval.toml")).unwrap();
    let table: EvalTable = toml::from_str(&text).expect("table parses");
    assert_eq!(table.format, "wing-mix-remit-tests/1");
    table
}

#[test]
fn remit_eval_table() {
    let table = eval_table();
    let failures = run_eval(&table, &default_policy());
    assert!(
        table.case.len() >= 79,
        "expected the full table, got {}",
        table.case.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        table.case.len(),
        failures.join("\n")
    );
}

/// The table catches policy changes (remit.md 9: a mutation of the policy makes expected cases fail).
#[test]
fn remit_eval_table_detects_policy_mutation() {
    let text = std::fs::read_to_string(data("wing-default.toml")).unwrap();
    let mutated = text.replacen("eq = \"allowed\"", "eq = \"conditional\"", 1);
    assert_ne!(text, mutated);
    let failures = run_eval(&eval_table(), &Policy::from_toml_str(&mutated).unwrap());
    assert!(
        failures
            .iter()
            .any(|f| f.starts_with("eq-before-iem-tap-is-coupled-but-allowed")),
        "{failures:?}"
    );
}

/// M039: the stereo-pair cases catch a pair rule that forgets its selector (every pan would get +-100) or loses its
/// priority (pairs would stay at +-50).
#[test]
fn remit_eval_table_detects_pair_rule_mutation() {
    let text = std::fs::read_to_string(data("wing-default.toml")).unwrap();
    for (from, to, case) in [
        ("stereo_pair = true\n", "", "mono-source-keeps-50"),
        (
            "stereo_pair = true\nclass = \"allowed\"\npriority = 1",
            "stereo_pair = true\nclass = \"allowed\"",
            "pair-left-spreads-to-full-left",
        ),
    ] {
        let mutated = text.replacen(from, to, 1);
        assert_ne!(text, mutated, "{from:?}");
        let failures = run_eval(&eval_table(), &Policy::from_toml_str(&mutated).unwrap());
        assert!(
            failures.iter().any(|f| f.starts_with(case)),
            "{case}: {failures:?}"
        );
    }
}

/// A user-confirmed pair replaces the detected one; unpairing returns both strips to the mono envelope.
#[test]
fn confirmed_pairs_override_detection() {
    let table = eval_table();
    let policy = default_policy();
    let mut ctx = build_ctx(&table);
    let session = RemitSession::default();
    let id = |s: &str| StripId::parse(s).unwrap();
    let pan = |ctx: &RemitContext, strip: &str, to: f64| {
        let req = WriteRequest::new(ParamId::new(format!("{strip}/pan")), ParamClass::Pan)
            .change(0.0, to);
        evaluate(&policy, ctx, &session, &req).rule.unwrap()
    };
    assert_eq!(pan(&ctx, "input.11", -100.0), "input-pair-spread");
    assert_eq!(pan(&ctx, "input.1", -100.0), "input-level-pan");
    // the user says KICK (input.1) and KEYS R (input.12) are a pair: KEYS L becomes mono
    ctx.confirm_pair(&id("input.1"), &id("input.12"));
    assert_eq!(pan(&ctx, "input.1", -100.0), "input-pair-spread");
    assert_eq!(pan(&ctx, "input.12", 100.0), "input-pair-spread");
    assert_eq!(pan(&ctx, "input.11", -100.0), "input-level-pan");
    // a pair confirmed in the session (planner proposal accepted by the user): unnamed channels 1 + 9
    let mut confirmed = RemitSession::default();
    confirmed.confirm(RemitSession::pair_key(&id("input.9"), &id("input.30")));
    let pan_s = |session: &RemitSession, strip: &str, to: f64| {
        let req = WriteRequest::new(ParamId::new(format!("{strip}/pan")), ParamClass::Pan)
            .change(0.0, to);
        evaluate(&policy, &ctx, session, &req)
    };
    assert_eq!(
        pan_s(&confirmed, "input.9", -100.0).rule.unwrap(),
        "input-pair-spread"
    );
    assert_eq!(
        pan_s(&confirmed, "input.9", 100.0).rule.unwrap(),
        "input-level-pan"
    );
    // input.30 has a post-pan wedge send: still capped by coupling
    let d = pan_s(&confirmed, "input.30", 100.0);
    assert_eq!(
        (d.rule.unwrap(), d.class.as_str()),
        ("input-pair-spread".to_string(), "conditional")
    );
    // a monitor strip stays locked whatever the session says
    confirmed.confirm(RemitSession::pair_key(&id("input.32"), &id("input.9")));
    assert_eq!(
        pan_s(&confirmed, "input.32", -100.0).class.as_str(),
        "forbidden"
    );
    ctx.unpair(&id("input.12"));
    assert_eq!(pan(&ctx, "input.1", -100.0), "input-level-pan");
    assert_eq!(pan(&ctx, "input.12", 100.0), "input-level-pan");
}

fn run_eval(table: &EvalTable, base: &Policy) -> Vec<String> {
    let base_ctx = build_ctx(table);
    let mut failures = Vec::new();
    for c in &table.case {
        let mut policy = base.clone();
        if !c.overlay.is_empty() {
            policy.add_rules(c.overlay.clone()).expect("overlay valid");
        }
        let mut ctx = base_ctx.clone();
        if let Some(sd) = c.solo_destructive {
            ctx.solo_destructive = sd;
        }
        let mut session = RemitSession::default();
        for k in &c.confirmed {
            session.confirm(k.clone());
        }
        let d = match c.action.as_deref() {
            Some("solo") => evaluate_inspection(
                &policy,
                &ctx,
                InspectionKind::Solo,
                &StripId::parse(&c.param).unwrap(),
            ),
            Some(other) => panic!("unknown action {other}"),
            None => {
                let req = WriteRequest {
                    param: ParamId::new(c.param.clone()),
                    class: c.class.expect("class"),
                    from: c.from.as_ref().map(to_value),
                    to: c.to.as_ref().map(to_value),
                };
                evaluate(&policy, &ctx, &session, &req)
            }
        };
        let mut bad = Vec::new();
        if d.class.as_str() != c.expect.class {
            bad.push(format!("class {} != {}", d.class, c.expect.class));
        }
        if d.reason.as_str() != c.expect.reason {
            bad.push(format!(
                "reason {} != {}",
                d.reason.as_str(),
                c.expect.reason
            ));
        }
        if let Some(r) = &c.expect.rule {
            if d.rule.as_deref() != Some(r.as_str()) {
                bad.push(format!("rule {:?} != {r}", d.rule));
            }
        }
        if let Some(m) = c.expect.max_delta_pct {
            let got = d.envelope.and_then(|e| e.max_delta_pct);
            if got.is_none_or(|g| (g - m).abs() > 1e-9) {
                bad.push(format!("max_delta_pct {got:?} != {m}"));
            }
        }
        if let Some(cp) = c.expect.coupled {
            if d.coupled != cp {
                bad.push(format!("coupled {} != {cp}", d.coupled));
            }
        }
        if !bad.is_empty() {
            failures.push(format!("{}: {} ({})", c.id, bad.join(", "), d.detail));
        }
    }
    failures
}

// ---------------------------------------------------------------- role-detection.toml

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DetTable {
    format: String,
    bus: Vec<DetCase<BusEvidence>>,
    input: Vec<DetCase<InputEvidence>>,
    main: Vec<DetCase<MainEvidence>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DetCase<E> {
    id: String,
    input: E,
    expect: DetExpect,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DetExpect {
    role: Option<Role>,
    confidence: Option<String>,
    effective: Option<Role>,
    usable: Option<bool>,
    name_hit: Option<bool>,
}

#[test]
fn role_detection_table() {
    let text = std::fs::read_to_string(data("tests/role-detection.toml")).unwrap();
    let t: DetTable = toml::from_str(&text).expect("table parses");
    assert_eq!(t.format, "wing-mix-remit-tests/1");
    let det = ScoringDetector::new(default_policy().detection);
    let cases: Vec<(String, Evidence, DetExpect)> = t
        .bus
        .into_iter()
        .map(|c| (c.id, Evidence::Bus(c.input), c.expect))
        .chain(
            t.input
                .into_iter()
                .map(|c| (c.id, Evidence::Input(c.input), c.expect)),
        )
        .chain(
            t.main
                .into_iter()
                .map(|c| (c.id, Evidence::Main(c.input), c.expect)),
        )
        .collect();
    let mut failures = Vec::new();
    for (id, ev, e) in &cases {
        let d = det.detect(ev);
        let mut bad = Vec::new();
        if let Some(r) = e.role {
            if d.role != r {
                bad.push(format!("role {} != {r}", d.role));
            }
        }
        if let Some(c) = &e.confidence {
            if confidence_band(d.confidence) != c {
                bad.push(format!(
                    "confidence {:.3} ({}) != {c}",
                    d.confidence,
                    confidence_band(d.confidence)
                ));
            }
        }
        if let Some(r) = e.effective {
            if d.effective != r {
                bad.push(format!("effective {} != {r}", d.effective));
            }
        }
        if let Some(u) = e.usable {
            if d.effective.is_usable() != u {
                bad.push(format!("usable {} != {u}", d.effective.is_usable()));
            }
        }
        if let Some(n) = e.name_hit {
            if d.name_hit != n {
                bad.push(format!("name_hit {} != {n}", d.name_hit));
            }
        }
        if !bad.is_empty() {
            failures.push(format!("{id}: {} [{:?}]", bad.join(", "), d.probabilities));
        }
    }
    assert!(
        cases.len() >= 45,
        "expected the full table, got {}",
        cases.len()
    );
    assert!(
        failures.is_empty(),
        "{} of {} cases failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

#[test]
fn default_policy_warnings_are_only_hard_classes() {
    let p = default_policy();
    assert_eq!(p.name, "WING default (FOH assistant)");
    // never-metadata lists link, solo_config and out_delay, which the hard list already forbids.
    assert_eq!(p.warnings.len(), 3, "{:?}", p.warnings);
    assert!(p.warnings.iter().all(|w| w.contains("never-metadata")));
}

/// A console without solo (`solo_available = false`): solo/listen are forbidden, RTA and writes are not affected.
#[test]
fn no_solo_forbids_solo_and_listen_only() {
    let policy = default_policy();
    let mut ctx = build_ctx(&eval_table());
    ctx.solo_available = false;
    let id = StripId::parse("input.1").unwrap();
    for k in [InspectionKind::Solo, InspectionKind::Listen] {
        let d = evaluate_inspection(&policy, &ctx, k, &id);
        assert_eq!(
            d.class,
            mix_surface::remit::Verdict::Forbidden,
            "{k:?}: {d:?}"
        );
    }
    assert_ne!(
        evaluate_inspection(&policy, &ctx, InspectionKind::Rta, &id).class,
        mix_surface::remit::Verdict::Forbidden
    );
}

/// F5: a policy names its console; the connected surface must match (or be an emulation of that family).
#[test]
fn policy_surface_is_checked_against_the_connected_surface() {
    use mix_surface::surface::{Capabilities, SurfaceInfo};
    let info = |surface: &str, model: &str| SurfaceInfo {
        surface: surface.into(),
        name: "n".into(),
        model: model.into(),
        firmware: "1".into(),
        capabilities: Capabilities::default(),
    };
    let wing = default_policy();
    assert_eq!(wing.surface.as_deref(), Some("wing"));
    assert!(wing.check_surface(&info("wing", "WING")).is_ok());
    assert!(
        wing.check_surface(&info("sim", "WING")).is_ok(),
        "the sim is a WING"
    );
    let e = wing.check_surface(&info("x32", "X32")).unwrap_err();
    assert!(
        e.to_string().contains("`wing`") && e.to_string().contains("x32"),
        "{e}"
    );
    let mut any = wing.clone();
    any.surface = None;
    assert!(any.check_surface(&info("x32", "X32")).is_ok());
}

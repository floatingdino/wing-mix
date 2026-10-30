//! Drives the M004 test tables (data/remit/tests/*.toml) against the default WING policy.

use mix_surface::model::{Block, ParamClass, Role, StripKind};
use mix_surface::remit::detect::{
    confidence_band, BusEvidence, Evidence, InputEvidence, MainEvidence,
};
use mix_surface::remit::{
    evaluate, evaluate_inspection, InspectionKind, Policy, RemitContext, RemitSend, RemitSession,
    RemitStrip, RoleDetector, Rule, ScoringDetector, WriteRequest,
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

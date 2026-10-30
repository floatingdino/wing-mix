//! Audits: full-state diffs, the remit (every param the run changed, evaluated independently with the same policy
//! and roles), ramp smoothness (max step per write vs the per-kind rate), solo restore, wing-sim snapshot diffs.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use mix_actuator::config::ramp_for;
use mix_actuator::{ActuatorConfig, PolicyGate, RemitGate};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::remit::{Policy, Verdict, WriteRequest};
use mix_surface::{Role, Surface};
use serde_json::Value as J;

use crate::observer::Change;

pub type State = BTreeMap<ParamId, Value>;

/// Every writable param of every strip, as the generic surface names it.
pub fn param_ids(s: &dyn Surface) -> Vec<ParamId> {
    let topo = s.topology().expect("topology");
    let mut out = Vec::new();
    for st in &topo.strips {
        if let Ok(ps) = s.params(&st.id) {
            out.extend(ps.into_iter().filter(|p| !p.read_only).map(|p| p.id));
        }
    }
    out
}

/// Read every param of `ids` from a surface.
pub fn read_state(s: &dyn Surface, ids: &[ParamId]) -> State {
    ids.iter()
        .filter_map(|p| s.read(p).ok().map(|v| (p.clone(), v)))
        .collect()
}

fn differs(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if matches!(a, Value::Float(_)) => {
            (x - y).abs() > 1e-3 * (1.0 + x.abs())
        }
        _ => a != b,
    }
}

/// Params whose value differs between two states: (param, before, after).
pub fn diff(before: &State, after: &State) -> Vec<(ParamId, Value, Value)> {
    before
        .iter()
        .filter_map(|(p, b)| {
            let a = after.get(p)?;
            differs(b, a).then(|| (p.clone(), b.clone(), a.clone()))
        })
        .collect()
}

#[derive(Debug, Default, Clone)]
pub struct RemitAudit {
    /// Params the run changed (excluding the harness's own engineer moves).
    pub changed: usize,
    pub allowed: usize,
    /// Conditional params whose confirmation key the operator (harness) gave.
    pub confirmed: usize,
    pub violations: Vec<String>,
    /// Changes by strip kind/role ("input foh: 12").
    pub by_role: BTreeMap<String, usize>,
}

/// Evaluate every change with an independent `PolicyGate` built from the same policy and confirmed roles on `s`
/// (the observer's connection, or an in-process twin of the sim console), plus the confirmation keys given.
pub fn remit(
    s: &dyn Surface,
    policy: &Policy,
    roles: &BTreeMap<StripId, Role>,
    confirmed_keys: &BTreeSet<String>,
    changes: &[(ParamId, Value, Value)],
) -> RemitAudit {
    let mut gate = PolicyGate::with_roles(policy.clone(), s, roles.clone()).expect("gate");
    let topo = s.topology().expect("topology");
    let mut a = RemitAudit::default();
    for (p, from, to) in changes {
        a.changed += 1;
        let strip = p.strip();
        let kind = strip
            .as_ref()
            .and_then(|id| topo.get(id))
            .map(|st| format!("{:?}", st.kind).to_lowercase())
            .unwrap_or_else(|| "system".into());
        let role = gate
            .context()
            .get(strip.as_ref().unwrap_or(&StripId::new("none", 0)))
            .map(|r| format!("{:?}", r.role).to_lowercase())
            .unwrap_or_default();
        *a.by_role.entry(format!("{kind} {role}")).or_default() += 1;
        let Ok(spec) = s.param_spec(p) else {
            a.violations
                .push(format!("{p}: unknown param ({from:?} -> {to:?})"));
            continue;
        };
        let req = WriteRequest::new(p.clone(), spec.class).change(from.clone(), to.clone());
        let d = gate.check_write(&req, from);
        match d.class {
            Verdict::Allowed => a.allowed += 1,
            Verdict::Conditional
                if d.confirm_key
                    .as_ref()
                    .is_some_and(|k| confirmed_keys.contains(k)) =>
            {
                a.confirmed += 1
            }
            _ => a.violations.push(format!(
                "{p} ({kind} {role}): {:?} {:?} {} ({from:?} -> {to:?})",
                d.class, d.reason, d.detail
            )),
        }
    }
    let _ = &mut gate;
    a
}

#[derive(Debug, Default, Clone)]
pub struct RampAudit {
    /// Continuous params observed moving.
    pub params: usize,
    pub writes: usize,
    pub discrete_writes: usize,
    /// Max step per write / (rate / tick) (1.0 = exactly the per-kind peak rate for one 40 ms scheduler tick; a
    /// late tick legitimately writes a bigger step at the same rate).
    pub max_ratio: f64,
    pub worst: String,
    /// Max speed over any >= 0.2 s stretch of observed writes / the per-kind peak rate (1.0 = at the peak rate;
    /// undo/PANIC/reverts run at `restore_speed` = 2).
    pub max_rate_ratio: f64,
    pub worst_rate: String,
    /// Stretches faster than `limit` x the per-kind rate.
    pub over: Vec<String>,
}

/// Ramp smoothness as a second client sees it, in the actuator's ramp space (dB / octaves / pan units): the max
/// step between consecutive observed values relative to the per-kind rate per scheduler tick, and the max speed
/// over >= 0.2 s stretches relative to the per-kind rate. `limit` = allowed speed ratio (restores run at
/// `restore_speed` = 2 x; observer timestamps jitter by a few ms).
pub fn ramps(
    s: &dyn Surface,
    changes: &[Change],
    skip: &BTreeSet<ParamId>,
    before: &State,
    limit: f64,
) -> RampAudit {
    let cfg = ActuatorConfig::default();
    let per_tick = 1.0 / cfg.tick_hz;
    let mut by: BTreeMap<&ParamId, Vec<&Change>> = BTreeMap::new();
    for c in changes {
        if !skip.contains(&c.param) {
            by.entry(&c.param).or_default().push(c);
        }
    }
    let mut a = RampAudit::default();
    for (p, cs) in by {
        let Ok(spec) = s.param_spec(p) else { continue };
        let Some((space, rate)) = ramp_for(&spec, &cfg.rates, cfg.fader_floor_db) else {
            a.discrete_writes += cs.len();
            continue;
        };
        a.params += 1;
        let mut prev = before.get(p).and_then(Value::as_f64);
        let mut pts: Vec<(Instant, f64)> = Vec::new();
        for c in cs {
            a.writes += 1;
            let Some(v) = c.value.as_f64() else { continue };
            if let Some(pv) = prev {
                let step = (space.to_x(v) - space.to_x(pv)).abs();
                let ratio = step / (rate * per_tick);
                if ratio > a.max_ratio {
                    a.max_ratio = ratio;
                    a.worst = format!("{p}: {pv:.3} -> {v:.3} ({ratio:.2} x rate/tick)");
                }
            }
            prev = Some(v);
            pts.push((c.at, space.to_x(v)));
        }
        // windowed speed: from each point back to the latest earlier point >= 0.2 s before it (and <= 0.5 s)
        for i in 0..pts.len() {
            let Some(j) = (0..i)
                .rev()
                .find(|&j| (pts[i].0 - pts[j].0).as_secs_f64() >= 0.2)
            else {
                continue;
            };
            let dt = (pts[i].0 - pts[j].0).as_secs_f64();
            if dt > 0.5 {
                continue;
            }
            let ratio = (pts[i].1 - pts[j].1).abs() / dt / rate;
            if ratio > a.max_rate_ratio {
                a.max_rate_ratio = ratio;
                a.worst_rate = format!("{p}: {ratio:.2} x {rate} /s over {dt:.2} s");
            }
            if ratio > limit {
                a.over
                    .push(format!("{p}: {ratio:.2} x rate over {dt:.2} s"));
            }
        }
    }
    a
}

/// Changes observed in `[from, to]` by anyone but the harness.
pub fn window(changes: &[Change], from: Instant, to: Instant) -> Vec<Change> {
    changes
        .iter()
        .filter(|c| c.at >= from && c.at <= to)
        .cloned()
        .collect()
}

/// Flatten a wing-sim `.snap` (JSON) into `path -> value` for the console data (`ae_data`).
pub fn flatten_snap(v: &J) -> BTreeMap<String, J> {
    fn go(prefix: &str, v: &J, out: &mut BTreeMap<String, J>) {
        match v {
            J::Object(m) => {
                for (k, x) in m {
                    let p = if prefix.is_empty() {
                        k.clone()
                    } else {
                        format!("{prefix}/{k}")
                    };
                    go(&p, x, out);
                }
            }
            other => {
                out.insert(prefix.to_string(), other.clone());
            }
        }
    }
    let mut out = BTreeMap::new();
    go("", &v["ae_data"], &mut out);
    out
}

/// Paths whose value differs between two flattened snapshots (floats with a small tolerance).
pub fn snap_diff(a: &BTreeMap<String, J>, b: &BTreeMap<String, J>) -> Vec<(String, J, J)> {
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter_map(|k| {
            let (x, y) = (
                a.get(k).cloned().unwrap_or(J::Null),
                b.get(k).cloned().unwrap_or(J::Null),
            );
            let same = match (x.as_f64(), y.as_f64()) {
                (Some(p), Some(q)) => (p - q).abs() <= 1e-3 * (1.0 + p.abs()),
                _ => x == y,
            };
            (!same).then(|| (k.clone(), x, y))
        })
        .collect()
}

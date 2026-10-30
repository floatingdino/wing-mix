//! Hunting/oscillation guard, per-param cool-downs and action-type penalties.
//!
//! - Every change the loop makes (moves and reverts) is recorded per param with its direction. A change whose
//!   direction is opposite to the previous one is a **reversal** (a revert always is); a param that reversed
//!   `max_reversals` times is frozen for the session, and after each reversal it is left alone for
//!   `reversal_cooldown_s`. A planned move that would reverse a param is only allowed while a spare reversal is
//!   left for its possible revert, so no param ever reverses more than `max_reversals` times.
//! - Params the engineer moved are held for `engineer_hold_s`.
//! - A move the remit/safety gate refused (e.g. the session boost limit) is not proposed again in that direction.
//! - A reverted batch penalises its **action type** (`phase:block`, e.g. `tonal_eq:eq`): later moves of that type
//!   need `benefit × penalty_factor^reverts ≥ min_benefit`, and the type is banned on that strip after
//!   `max_type_reverts` reverts there.

use std::collections::BTreeMap;

use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;

use super::config::ControlConfig;
use crate::plan::PlannedMove;

/// The block a param belongs to: the path segment after the strip (`input.3/eq/band.1.gain` → `eq`).
pub fn block_of(p: &ParamId) -> &str {
    let s = p.as_str();
    let rest = s.split_once('/').map_or(s, |(_, r)| r);
    rest.split('/').next().unwrap_or(rest)
}

/// Action type of a move: `phase:block` of its first param.
pub fn action_type(mv: &PlannedMove) -> String {
    let block = mv
        .action
        .changes
        .first()
        .map(|c| block_of(&c.param).to_string())
        .unwrap_or_default();
    format!("{}:{}", mv.phase, block)
}

/// Whether a param is a level in dB (counted in the change budget).
pub fn is_db_param(p: &ParamId) -> bool {
    let s = p.as_str();
    ["fader", "gain", "level", "trim", "makeup"]
        .iter()
        .any(|k| s.ends_with(k))
}

/// Direction of a change: +1 up, -1 down, 0 none; a non-numeric change counts as a toggle (`+1` when it differs).
fn direction(from: &Value, to: &Value) -> i8 {
    match (from.as_f64(), to.as_f64()) {
        (Some(a), Some(b)) if matches!(to, Value::Float(_)) => {
            if b > a + 1e-6 {
                1
            } else if b < a - 1e-6 {
                -1
            } else {
                0
            }
        }
        _ => i8::from(from != to),
    }
}

#[derive(Clone, Debug, Default)]
struct ParamTrack {
    last_dir: i8,
    reversals: u32,
    changes: u32,
    hold_until: f64,
}

/// Why a batch may not run now.
#[derive(Clone, Debug, PartialEq)]
pub enum Block {
    /// Try again at `until` (cool-down / engineer hold).
    Wait { until: f64, param: ParamId },
    /// Not this session (frozen param, banned type, penalised below `min_benefit`).
    Never(String),
}

/// See the module docs.
#[derive(Clone, Debug, Default)]
pub struct Guard {
    params: BTreeMap<ParamId, ParamTrack>,
    type_reverts: BTreeMap<String, u32>,
    /// Params the remit/safety gate refused to move in a direction (0 = any): not proposed again this session.
    refused: BTreeMap<ParamId, i8>,
    strip_type_reverts: BTreeMap<(StripId, String), u32>,
}

impl Guard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a change. Returns `Some(reversals)` when it froze the param.
    pub fn record(
        &mut self,
        param: &ParamId,
        from: &Value,
        to: &Value,
        now: f64,
        cfg: &ControlConfig,
    ) -> Option<u32> {
        let dir = direction(from, to);
        if dir == 0 {
            return None;
        }
        let t = self.params.entry(param.clone()).or_default();
        let numeric = matches!(to, Value::Float(_));
        let reversed = t.changes > 0 && (!numeric || t.last_dir == -dir);
        t.changes += 1;
        t.last_dir = dir;
        if reversed {
            t.reversals += 1;
            t.hold_until = t.hold_until.max(now + cfg.reversal_cooldown_s);
            if t.reversals == cfg.max_reversals {
                return Some(t.reversals);
            }
        }
        None
    }

    /// The engineer moved `param`.
    pub fn engineer(&mut self, param: &ParamId, now: f64, cfg: &ControlConfig) {
        let t = self.params.entry(param.clone()).or_default();
        t.hold_until = t.hold_until.max(now + cfg.engineer_hold_s);
    }

    /// A batch of `mv`'s type was reverted.
    pub fn penalise(&mut self, mv: &PlannedMove) -> String {
        let ty = action_type(mv);
        *self.type_reverts.entry(ty.clone()).or_default() += 1;
        *self
            .strip_type_reverts
            .entry((mv.strip.clone(), ty.clone()))
            .or_default() += 1;
        ty
    }

    /// The gate refused moving `param` from `from` to `to` (e.g. the safety layer's session boost limit).
    pub fn refuse(&mut self, param: &ParamId, from: Option<&Value>, to: &Value) {
        let dir = from.map_or(0, |f| direction(f, to));
        let d = self.refused.entry(param.clone()).or_insert(dir);
        if *d != dir {
            *d = 0;
        }
    }

    pub fn reversals(&self, param: &ParamId) -> u32 {
        self.params.get(param).map_or(0, |t| t.reversals)
    }

    pub fn is_frozen(&self, param: &ParamId, cfg: &ControlConfig) -> bool {
        self.reversals(param) >= cfg.max_reversals
    }

    /// Reverts recorded for an action type (all strips).
    pub fn type_reverts(&self, ty: &str) -> u32 {
        self.type_reverts.get(ty).copied().unwrap_or(0)
    }

    /// Can `mv` run at `now`?
    pub fn check(&self, mv: &PlannedMove, now: f64, cfg: &ControlConfig) -> Result<(), Block> {
        let ty = action_type(mv);
        let on_strip = self
            .strip_type_reverts
            .get(&(mv.strip.clone(), ty.clone()))
            .copied()
            .unwrap_or(0);
        if on_strip >= cfg.max_type_reverts {
            return Err(Block::Never(format!(
                "{ty} banned on {} ({on_strip} reverts)",
                mv.strip
            )));
        }
        let gain = mv.predicted.objective_before - mv.predicted.objective_after;
        let eff = gain * cfg.penalty_factor.powi(self.type_reverts(&ty) as i32);
        if eff < cfg.min_benefit {
            return Err(Block::Never(format!(
                "{ty}: penalised benefit {eff:.4} < {}",
                cfg.min_benefit
            )));
        }
        let mut wait: Option<(f64, ParamId)> = None;
        for c in &mv.action.changes {
            if let Some(&d) = self.refused.get(&c.param) {
                let dir = c.from.as_ref().map_or(0, |f| direction(f, &c.to));
                if d == 0 || d == dir {
                    return Err(Block::Never(format!("{} refused by the gate", c.param)));
                }
            }
            let Some(t) = self.params.get(&c.param) else {
                continue;
            };
            if t.reversals >= cfg.max_reversals {
                return Err(Block::Never(format!(
                    "{} frozen ({} reversals)",
                    c.param, t.reversals
                )));
            }
            // a reversal needs one spare: a revert of this move would be another one
            let dir = c.from.as_ref().map_or(0, |f| direction(f, &c.to));
            let reverses = t.changes > 0 && dir != 0 && (dir == -t.last_dir || !matches!(c.to, Value::Float(_)));
            if reverses && t.reversals + 1 >= cfg.max_reversals {
                return Err(Block::Never(format!(
                    "{} would reverse again ({} reversals)",
                    c.param, t.reversals
                )));
            }
            if t.hold_until > now && wait.as_ref().is_none_or(|w| w.0 < t.hold_until) {
                wait = Some((t.hold_until, c.param.clone()));
            }
        }
        match wait {
            Some((until, param)) => Err(Block::Wait { until, param }),
            None => Ok(()),
        }
    }
}

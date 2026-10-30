//! M067: the session change budget.
//!
//! Before M067 the budget was total travel (Σ |Δ| of every dB change, reverts included) plus a batch count, so a
//! loop that kept a few clear moves and reverted a few mispredicted ones ran out long before it converged (every
//! e2e and real-song run ended on it). What the budget is for is different: the engineer hands over a mix and the
//! tool must not take it further from that starting point than agreed, nor change it faster than an engineer
//! would. So it limits
//!
//! - **net deviation**: Σ over params of `weight × share × |our net change|`, where *our net change* is the sum of
//!   the loop's own changes to the param (moves and reverts; an engineer's moves are theirs and do not count). A
//!   move the loop later reverts costs nothing once reverted. A move that brings a param back towards where the
//!   engineer left it always fits.
//! - **rate**: weighted travel of forward moves (reverts and undos are restorative and not charged) in a rolling
//!   window of `rate_window_s` (60 s live): a batch that would exceed `max_db_per_min` waits (pacing), it does not
//!   stop the session.
//! - a hard **batch count** (`max_batches`) as a backstop.
//!
//! Weights ([`weight`]): compressor/gate makeup 0 (it restores the level the threshold/ratio took, so counting it
//! would charge one decision twice); every other dB param 1. Shares: the members of a group move (one batch moving
//! several strips' same param by the same amount: a fader group, both sides of a stereo pair, the same EQ cut on a
//! linked pair) share one count: `1 / n` each, so the group costs what its largest member moved.

use std::collections::{BTreeMap, VecDeque};

use mix_core::action::Action;
use mix_core::ids::ParamId;
use mix_surface::journal::Change;
use serde::{Deserialize, Serialize};

use super::guard::is_db_param;

/// Budget weight of a dB param (see the module docs). Non-dB params weigh 0.
pub fn weight(p: &ParamId) -> f64 {
    if !is_db_param(p) {
        return 0.0;
    }
    if p.as_str().ends_with("makeup") {
        return 0.0;
    }
    1.0
}

/// Equal-delta tolerance (dB) for group members.
const GROUP_TOL_DB: f64 = 0.05;

/// Per-param share of an action's cost: the same param of several strips (path after the strip) moved by the same
/// amount share one count.
pub fn shares(action: &Action) -> BTreeMap<ParamId, f64> {
    let deltas: Vec<(ParamId, String, f64)> = action
        .changes
        .iter()
        .filter_map(|c| {
            let d = c.to.as_f64()? - c.from.as_ref()?.as_f64()?;
            let key = c
                .param
                .as_str()
                .split_once('/')
                .map_or("", |x| x.1)
                .to_string();
            Some((c.param.clone(), key, d))
        })
        .collect();
    deltas
        .iter()
        .map(|(p, leaf, d)| {
            let n = deltas
                .iter()
                .filter(|(_, l, e)| l == leaf && (e - d).abs() <= GROUP_TOL_DB)
                .count()
                .max(1);
            (p.clone(), 1.0 / n as f64)
        })
        .collect()
}

/// Weighted cost of an action's changes (dB, before the action runs).
pub fn cost(action: &Action) -> f64 {
    let sh = shares(action);
    action
        .changes
        .iter()
        .filter_map(|c| {
            let d = c.to.as_f64()? - c.from.as_ref()?.as_f64()?;
            Some(weight(&c.param) * sh.get(&c.param).copied().unwrap_or(1.0) * d.abs())
        })
        .sum()
}

/// Limits (see [`super::ControlConfig`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BudgetLimits {
    pub max_batches: u32,
    /// Net weighted deviation from the session baseline (dB).
    pub max_net_db: f64,
    /// Weighted forward travel per `rate_window_s` (dB).
    pub max_db_per_min: f64,
    /// Rolling window of the rate limit (s; 60 live, scaled with test clocks).
    pub rate_window_s: f64,
}

/// What the budget shows (daemon `session.change_budget`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetStatus {
    pub batches: u32,
    pub max_batches: u32,
    /// Weighted net deviation from the session baseline now (dB).
    pub net_db: f64,
    pub max_net_db: f64,
    /// Weighted forward travel in the current rate window (dB).
    pub rate_db: f64,
    pub max_db_per_min: f64,
    /// Total weighted forward travel this session (dB; information only).
    pub travel_db: f64,
    /// Pacing: no new batch before this time (s, controller clock).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub paced_until_s: Option<f64>,
}

/// Why a batch does not fit now.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fit {
    Ok,
    /// Over the net limit (and it would increase the deviation): not this batch.
    OverNet,
    /// The rate window is full: try again at `until`.
    Pace {
        until: f64,
    },
    /// Batch count reached.
    Batches,
}

/// Change-budget bookkeeping. See the module docs.
#[derive(Clone, Debug, Default)]
pub struct ChangeBudget {
    /// Our net change per param (dB).
    net: BTreeMap<ParamId, f64>,
    /// Share of the param's last group move.
    share: BTreeMap<ParamId, f64>,
    /// `(time, cost)` of forward batches.
    recent: VecDeque<(f64, f64)>,
    pub batches: u32,
    travel: f64,
}

impl ChangeBudget {
    fn param_cost(&self, p: &ParamId, net: f64) -> f64 {
        weight(p) * self.share.get(p).copied().unwrap_or(1.0) * net.abs()
    }

    /// Weighted net deviation (dB).
    pub fn net_db(&self) -> f64 {
        self.net.iter().map(|(p, &n)| self.param_cost(p, n)).sum()
    }

    /// Net deviation after `action` ran.
    pub fn net_after(&self, action: &Action) -> f64 {
        let sh = shares(action);
        let mut net = self.net.clone();
        let mut share = self.share.clone();
        for c in &action.changes {
            let (Some(a), Some(b)) = (c.from.as_ref().and_then(|v| v.as_f64()), c.to.as_f64())
            else {
                continue;
            };
            *net.entry(c.param.clone()).or_default() += b - a;
            if let Some(s) = sh.get(&c.param) {
                share.insert(c.param.clone(), *s);
            }
        }
        net.iter()
            .map(|(p, &n)| weight(p) * share.get(p).copied().unwrap_or(1.0) * n.abs())
            .sum()
    }

    fn rate(&mut self, now: f64, window: f64) -> f64 {
        while self.recent.front().is_some_and(|&(t, _)| now - t >= window) {
            self.recent.pop_front();
        }
        self.recent.iter().map(|x| x.1).sum()
    }

    /// Whether `action` may run now.
    pub fn fit(&mut self, action: &Action, now: f64, lim: &BudgetLimits) -> Fit {
        if self.batches >= lim.max_batches {
            return Fit::Batches;
        }
        let before = self.net_db();
        let after = self.net_after(action);
        if after > lim.max_net_db && after > before + 1e-9 {
            return Fit::OverNet;
        }
        let c = cost(action);
        let used = self.rate(now, lim.rate_window_s);
        if c > 0.0 && used > 0.0 && used + c > lim.max_db_per_min {
            // the oldest entries leave the window first: wait until enough has left
            let mut left = used;
            for &(t, x) in &self.recent {
                left -= x;
                if left + c <= lim.max_db_per_min {
                    return Fit::Pace {
                        until: t + lim.rate_window_s,
                    };
                }
            }
            return Fit::Pace {
                until: now + lim.rate_window_s,
            };
        }
        Fit::Ok
    }

    /// A batch with `action` is submitted at `now`.
    pub fn submitted(&mut self, action: &Action, now: f64) {
        self.batches += 1;
        for (p, s) in shares(action) {
            self.share.insert(p, s);
        }
        let c = cost(action);
        self.travel += c;
        self.recent.push_back((now, c));
    }

    /// A change the loop made (move or revert) reached the console.
    pub fn changed(&mut self, c: &Change) {
        if let (Some(a), Some(b)) = (c.from.as_f64(), c.to.as_f64()) {
            if is_db_param(&c.param) {
                *self.net.entry(c.param.clone()).or_default() += b - a;
            }
        }
    }

    pub fn status(&mut self, now: f64, lim: &BudgetLimits, paced: Option<f64>) -> BudgetStatus {
        BudgetStatus {
            batches: self.batches,
            max_batches: lim.max_batches,
            net_db: self.net_db(),
            max_net_db: lim.max_net_db,
            rate_db: self.rate(now, lim.rate_window_s),
            max_db_per_min: lim.max_db_per_min,
            travel_db: self.travel,
            paced_until_s: paced.filter(|&t| t > now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mix_core::action::ParamChange;
    use mix_core::value::Value;

    fn ch(p: &str, from: f64, to: f64) -> ParamChange {
        ParamChange {
            param: ParamId::new(p),
            from: Some(Value::Float(from)),
            to: Value::Float(to),
        }
    }

    fn act(changes: Vec<ParamChange>) -> Action {
        Action {
            label: String::new(),
            changes,
        }
    }

    fn apply(b: &mut ChangeBudget, a: &Action, now: f64) {
        b.submitted(a, now);
        for c in &a.changes {
            b.changed(&Change {
                param: c.param.clone(),
                from: c.from.clone().unwrap(),
                to: c.to.clone(),
            });
        }
    }

    fn revert(b: &mut ChangeBudget, a: &Action) {
        for c in &a.changes {
            b.changed(&Change {
                param: c.param.clone(),
                from: c.to.clone(),
                to: c.from.clone().unwrap(),
            });
        }
    }

    const LIM: BudgetLimits = BudgetLimits {
        max_batches: 100,
        max_net_db: 20.0,
        max_db_per_min: 10.0,
        rate_window_s: 60.0,
    };

    #[test]
    fn makeup_is_free_and_groups_count_their_largest_member() {
        let comp = act(vec![
            ch("input.1/dyn/threshold", 0.0, -20.0),
            ch("input.1/dyn/makeup", 0.0, 6.0),
        ]);
        assert_eq!(cost(&comp), 0.0, "threshold is not a level, makeup is free");
        let group = act((1..=8)
            .map(|i| ch(&format!("input.{i}/fader"), -10.0, -13.0))
            .collect());
        assert!((cost(&group) - 3.0).abs() < 1e-9);
        let pair = act(vec![
            ch("input.28/eq/band.2.gain", 0.0, -3.0),
            ch("input.29/eq/band.2.gain", 0.0, -3.0),
            ch("input.5/fader", 0.0, 2.0),
        ]);
        assert!((cost(&pair) - 5.0).abs() < 1e-9);
    }

    #[test]
    fn reverts_restore_the_net_and_are_not_charged_twice() {
        let mut b = ChangeBudget::default();
        let a = act(vec![ch("input.1/fader", -10.0, -7.0)]);
        apply(&mut b, &a, 0.0);
        assert!((b.net_db() - 3.0).abs() < 1e-9);
        revert(&mut b, &a);
        assert!(b.net_db().abs() < 1e-9);
        let s = b.status(1.0, &LIM, None);
        assert!((s.travel_db - 3.0).abs() < 1e-9 && (s.rate_db - 3.0).abs() < 1e-9);
        // a move back towards the start always fits, even over the limit
        let mut b = ChangeBudget::default();
        let up = act(vec![ch("input.1/fader", -30.0, -5.0)]);
        apply(&mut b, &up, 0.0);
        assert!(b.net_db() > LIM.max_net_db);
        let more = act(vec![ch("input.2/fader", 0.0, 1.0)]);
        assert_eq!(b.fit(&more, 100.0, &LIM), Fit::OverNet);
        let back = act(vec![ch("input.1/fader", -5.0, -8.0)]);
        assert_eq!(b.fit(&back, 100.0, &LIM), Fit::Ok);
    }

    #[test]
    fn rate_paces_instead_of_stopping() {
        let mut b = ChangeBudget::default();
        let m = |i: u32| act(vec![ch(&format!("input.{i}/fader"), 0.0, -3.0)]);
        apply(&mut b, &m(1), 0.0);
        apply(&mut b, &m(2), 10.0);
        apply(&mut b, &m(3), 20.0);
        // 9 dB in the window: a 3 dB move must wait until the first leaves it
        assert_eq!(b.fit(&m(4), 30.0, &LIM), Fit::Pace { until: 60.0 });
        assert_eq!(b.fit(&m(4), 60.0, &LIM), Fit::Ok);
        let s = b.status(30.0, &LIM, Some(60.0));
        assert_eq!(s.paced_until_s, Some(60.0));
        assert_eq!(s.batches, 3);
        let lim = BudgetLimits {
            max_batches: 3,
            ..LIM
        };
        assert_eq!(b.fit(&m(4), 100.0, &lim), Fit::Batches);
    }
}

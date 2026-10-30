//! Actuator configuration: tick rate, global write budget, per-param-kind max rates, cool-downs.

use crate::ramp::Space;
use mix_core::units::Unit;
use mix_surface::model::{ParamClass, ParamSpec, Taper, ValueKind};
use serde::{Deserialize, Serialize};

/// Max rates per param kind, in ramp-space units per second (dB/s, octaves/s, %/s ...). These are *peak* rates: the
/// eased ramp never exceeds them between two consecutive writes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rates {
    /// Faders and send levels (dB/s).
    pub fader_db_per_s: f64,
    /// Trim, preamp gain, balance, EQ/filter gains (dB/s).
    pub gain_db_per_s: f64,
    /// Dynamics levels: thresholds, make-up, range (dB/s).
    pub dyn_db_per_s: f64,
    /// Frequencies, moved in log2 space (octaves/s).
    pub freq_oct_per_s: f64,
    /// Q, moved in log2 space (doublings/s).
    pub q_oct_per_s: f64,
    /// Other positive log-tapered values: times, ratios (doublings/s).
    pub log_oct_per_s: f64,
    /// Pan/balance positions (-100..100 units/s).
    pub pan_per_s: f64,
    /// Percent params (mix, width) (%/s).
    pub percent_per_s: f64,
    /// Any other continuous param: fraction of its range per second.
    pub range_fraction_per_s: f64,
}

impl Default for Rates {
    fn default() -> Self {
        Rates {
            fader_db_per_s: 6.0,
            gain_db_per_s: 3.0,
            dyn_db_per_s: 6.0,
            freq_oct_per_s: 1.0,
            q_oct_per_s: 1.0,
            log_oct_per_s: 1.0,
            pan_per_s: 40.0,
            percent_per_s: 25.0,
            range_fraction_per_s: 0.25,
        }
    }
}

/// How a param is moved: `None` = discrete (one write), else the ramp space and its max rate.
pub fn ramp_for(spec: &ParamSpec, rates: &Rates, fader_floor_db: f64) -> Option<(Space, f64)> {
    if !matches!(spec.kind, ValueKind::Float) || spec.taper == Taper::Discrete {
        return None;
    }
    let (lo, hi) = spec.range.unwrap_or((f64::NEG_INFINITY, f64::INFINITY));
    let positive = lo > 0.0;
    let level = matches!(spec.class, ParamClass::Fader | ParamClass::SendLevel)
        || spec.taper == Taper::Fader;
    let r = match spec.unit {
        Unit::Db if level => {
            return Some((
                Space::Level {
                    floor: fader_floor_db,
                },
                rates.fader_db_per_s,
            ))
        }
        Unit::Db if spec.class == ParamClass::Dynamics => (Space::Linear, rates.dyn_db_per_s),
        Unit::Db => (Space::Linear, rates.gain_db_per_s),
        Unit::Hz if positive => (Space::Log2, rates.freq_oct_per_s),
        Unit::Q if positive => (Space::Log2, rates.q_oct_per_s),
        Unit::Ms | Unit::Seconds | Unit::Ratio if positive => (Space::Log2, rates.log_oct_per_s),
        Unit::Pan => (Space::Linear, rates.pan_per_s),
        Unit::Percent => (Space::Linear, rates.percent_per_s),
        _ if spec.taper == Taper::Log && positive => (Space::Log2, rates.log_oct_per_s),
        _ if (hi - lo).is_finite() => (Space::Linear, rates.range_fraction_per_s * (hi - lo)),
        _ => return None,
    };
    Some(r)
}

/// Actuator settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActuatorConfig {
    /// Scheduler tick rate (Hz) used by the thread runner. 20..50 Hz is typical.
    pub tick_hz: f64,
    /// Global cap on param writes sent to the console (writes/s, token bucket).
    pub max_writes_per_s: f64,
    /// Token-bucket capacity (writes). Raised to the batch size if smaller, so a batch can always move together.
    pub write_burst: f64,
    /// Largest connected batch accepted.
    pub max_batch: usize,
    pub rates: Rates,
    /// Global speed factor (<= 1 moves slower than the max rates).
    pub speed: f64,
    /// Speed factor for PANIC and undo (restoring the engineer's values may go faster).
    pub restore_speed: f64,
    /// Shortest continuous move (s): even tiny moves are eased, never stepped.
    pub min_move_s: f64,
    /// Level ramps treat values below this as this (dB): a fader leaves -inf by stepping to the floor first.
    pub fader_floor_db: f64,
    /// After the engineer moves a param, the actuator leaves it alone this long (s); a move touching it pauses.
    pub external_cooldown_s: f64,
    /// Pause between two jobs (s): lets analysis settle before the next move.
    pub settle_s: f64,
    /// Longest wait (s) for the surface's `ParamsRemapped` after a model switch before the block's other params of
    /// the same move are written (they are never written in the same ramp as the switch).
    #[serde(default = "default_remap_wait_s")]
    pub remap_wait_s: f64,
    /// Most write records kept in memory (`Actuator::write_log`); the oldest are dropped first and counted
    /// (`write_log_dropped`). 0 = unbounded. M050 persists the full audit trail via the event stream.
    #[serde(default = "default_write_log_cap")]
    pub write_log_cap: usize,
    /// Most completed moves kept for undo (oldest dropped first). 0 = unbounded.
    #[serde(default = "default_history_cap")]
    pub history_cap: usize,
    /// Most params one slot-recall snap may write (M064; a snap is one console batch, not a ramp).
    #[serde(default = "default_max_snap")]
    pub max_snap: usize,
}

fn default_max_snap() -> usize {
    4096
}

fn default_remap_wait_s() -> f64 {
    1.0
}

fn default_write_log_cap() -> usize {
    10_000
}

fn default_history_cap() -> usize {
    256
}

impl Default for ActuatorConfig {
    fn default() -> Self {
        ActuatorConfig {
            tick_hz: 25.0,
            max_writes_per_s: 100.0,
            write_burst: 10.0,
            max_batch: 8,
            rates: Rates::default(),
            speed: 1.0,
            restore_speed: 2.0,
            min_move_s: 0.2,
            fader_floor_db: -80.0,
            external_cooldown_s: 5.0,
            settle_s: 0.0,
            remap_wait_s: default_remap_wait_s(),
            write_log_cap: default_write_log_cap(),
            history_cap: default_history_cap(),
            max_snap: default_max_snap(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mix_surface::model::ParamClass;

    #[test]
    fn spaces_by_kind() {
        let r = Rates::default();
        let fader = ParamSpec::float("input.1/fader", ParamClass::Fader, Unit::Db, -144.0, 10.0)
            .with_taper(Taper::Fader);
        assert_eq!(
            ramp_for(&fader, &r, -80.0),
            Some((Space::Level { floor: -80.0 }, 6.0))
        );
        let f = ParamSpec::float(
            "input.1/eq/band.1.freq",
            ParamClass::Eq,
            Unit::Hz,
            20.0,
            2e4,
        );
        assert_eq!(ramp_for(&f, &r, -80.0), Some((Space::Log2, 1.0)));
        let g = ParamSpec::float(
            "input.1/eq/band.1.gain",
            ParamClass::Eq,
            Unit::Db,
            -15.0,
            15.0,
        );
        assert_eq!(ramp_for(&g, &r, -80.0), Some((Space::Linear, 3.0)));
        let att = ParamSpec::float(
            "input.1/dyn/attack",
            ParamClass::Dynamics,
            Unit::Ms,
            0.0,
            120.0,
        );
        assert_eq!(ramp_for(&att, &r, -80.0), Some((Space::Linear, 30.0)));
        let on = ParamSpec::boolean("input.1/eq/on", ParamClass::Eq);
        assert_eq!(ramp_for(&on, &r, -80.0), None);
    }
}

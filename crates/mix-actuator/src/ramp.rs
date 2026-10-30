//! Ramp math: ramp spaces, easing, and the per-param [`Track`] that turns a move into a sequence of writes.
//!
//! A move runs on a shared progress `p` in 0..=1 (all params of a connected batch arrive together). Each param's
//! desired position is `x0 + (x1 - x0) * ease(p)` in its ramp space; between two writes the position may change by
//! at most `rate * dt` (so stalls caused by the global write budget never produce a jump). The last write is the
//! exact requested value.

use mix_core::value::Value;
use mix_surface::model::ParamSpec;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Coordinate space a continuous param is ramped in.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "space")]
pub enum Space {
    /// Engineering value as is (dB gains, pan, %).
    Linear,
    /// log2 of the value (frequency in octaves, Q, times, ratios).
    Log2,
    /// Levels in dB with a floor: values below `floor` (down to -inf) are treated as `floor`, so a fader leaving
    /// -inf steps to the floor first and a fader going to -inf ramps to the floor and then drops.
    Level { floor: f64 },
}

impl Space {
    pub fn to_x(self, v: f64) -> f64 {
        match self {
            Space::Linear => v,
            Space::Log2 => v.max(f64::MIN_POSITIVE).log2(),
            Space::Level { floor } => v.max(floor),
        }
    }

    pub fn from_x(self, x: f64) -> f64 {
        match self {
            Space::Linear | Space::Level { .. } => x,
            Space::Log2 => x.exp2(),
        }
    }
}

/// Raised-cosine ease-in/ease-out on 0..=1. Peak slope is `PI / 2` (at `t = 0.5`); `ease(0) = 0`, `ease(1) = 1`.
pub fn ease(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    0.5 - 0.5 * (PI * t).cos()
}

/// Peak slope of [`ease`]: a ramp over distance `d` at peak rate `r` needs `d * EASE_PEAK / r` seconds.
pub const EASE_PEAK: f64 = PI / 2.0;

/// One param of a move.
#[derive(Debug, Clone)]
pub struct Track {
    pub spec: ParamSpec,
    /// Value read before the move.
    pub from: Value,
    /// Requested (coerced) value.
    pub to: Value,
    /// `Some((space, max rate))` for continuous params; `None` = one discrete write.
    pub ramp: Option<(Space, f64)>,
    pub x0: f64,
    pub x1: f64,
    /// Position and time (s) of the last write.
    pub last_x: f64,
    pub last_t: f64,
    /// Value the surface stored on the last write (or `from`).
    pub last_written: Value,
    pub wrote: bool,
    pub finished: bool,
    /// The engineer took this param over: no more writes.
    pub yielded: bool,
}

/// A write a track wants this tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub x: f64,
    pub value: Value,
    pub last: bool,
}

impl Track {
    pub fn new(spec: ParamSpec, from: Value, to: Value, ramp: Option<(Space, f64)>, t0: f64) -> Self {
        let (x0, x1) = match (ramp, from.as_f64(), to.as_f64()) {
            (Some((s, _)), Some(a), Some(b)) => (s.to_x(a), s.to_x(b)),
            _ => (0.0, 0.0),
        };
        // A non-numeric current value cannot be ramped: treat as discrete.
        let ramp = ramp.filter(|_| from.as_f64().is_some() && to.as_f64().is_some());
        Track {
            spec,
            last_written: from.clone(),
            from,
            to,
            ramp,
            x0,
            x1,
            last_x: x0,
            last_t: t0,
            wrote: false,
            finished: false,
            yielded: false,
        }
    }

    /// Seconds this track needs at peak rate `rate * speed`.
    pub fn min_duration(&self, speed: f64) -> f64 {
        match self.ramp {
            Some((_, rate)) if rate > 0.0 => (self.x1 - self.x0).abs() * EASE_PEAK / (rate * speed),
            _ => 0.0,
        }
    }

    pub fn is_active(&self) -> bool {
        !self.finished && !self.yielded
    }

    /// The write this track wants at shared progress `p` and time `t` (`None`: nothing new to write).
    pub fn desired(&self, p: f64, t: f64, speed: f64) -> Option<Pending> {
        if !self.is_active() {
            return None;
        }
        let Some((space, rate)) = self.ramp else {
            return Some(Pending {
                x: 0.0,
                value: self.to.clone(),
                last: true,
            });
        };
        let want = self.x0 + (self.x1 - self.x0) * ease(p);
        let max_dx = rate * speed * (t - self.last_t).max(0.0);
        let x = self.last_x + (want - self.last_x).clamp(-max_dx, max_dx);
        if x == self.x1 {
            return Some(Pending {
                x,
                value: self.to.clone(),
                last: true,
            });
        }
        let value = self
            .spec
            .coerce(&Value::Float(space.from_x(x)))
            .unwrap_or_else(|_| Value::Float(space.from_x(x)));
        if value.approx_eq(&self.last_written, self.spec.tolerance()) {
            return None;
        }
        Some(Pending {
            x,
            value,
            last: false,
        })
    }

    /// Record a successful write.
    pub fn wrote(&mut self, pending: &Pending, stored: Value, t: f64) {
        self.last_x = pending.x;
        self.last_t = t;
        self.last_written = stored;
        self.wrote = true;
        if pending.last {
            self.finished = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mix_core::units::Unit;
    use mix_surface::model::ParamClass;

    #[test]
    fn ease_shape() {
        assert_eq!(ease(0.0), 0.0);
        assert_eq!(ease(1.0), 1.0);
        assert!((ease(0.5) - 0.5).abs() < 1e-12);
        // Monotonic, slow at both ends.
        let mut prev = 0.0;
        for i in 1..=100 {
            let e = ease(i as f64 / 100.0);
            assert!(e >= prev);
            prev = e;
        }
        assert!(ease(0.1) < 0.1 && ease(0.9) > 0.9);
    }

    #[test]
    fn spaces_round_trip() {
        assert!((Space::Log2.from_x(Space::Log2.to_x(250.0)) - 250.0).abs() < 1e-9);
        let l = Space::Level { floor: -80.0 };
        assert_eq!(l.to_x(-144.0), -80.0);
        assert_eq!(l.to_x(-6.0), -6.0);
    }

    #[test]
    fn track_reaches_exact_target() {
        let spec = ParamSpec::float("input.1/eq/band.1.freq", ParamClass::Eq, Unit::Hz, 20.0, 2e4);
        let mut tr = Track::new(
            spec,
            Value::Float(500.0),
            Value::Float(250.0),
            Some((Space::Log2, 1.0)),
            0.0,
        );
        let dur = tr.min_duration(1.0);
        assert!((dur - EASE_PEAK).abs() < 1e-9, "1 octave at 1 oct/s peak");
        let mut t = 0.0;
        while !tr.finished {
            t += 0.04;
            if let Some(p) = tr.desired((t / dur).min(1.0), t, 1.0) {
                let v = p.value.clone();
                tr.wrote(&p, v, t);
            }
            assert!(t < 10.0);
        }
        assert_eq!(tr.last_written, Value::Float(250.0));
    }
}

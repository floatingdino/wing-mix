//! X32 value laws: the console exchanges floats in 0.0..1.0 on a fixed grid per parameter; the generic model uses
//! engineering units. Every law snaps to the console's grid, so `decode(encode(v))` is exactly what the console
//! stores. (Laws and step counts: "Unofficial X32/M32 OSC Remote Protocol", P.-G. Maillot.)

use crate::osc::Arg;
use mix_core::value::Value;

/// How one native argument maps to a generic value.
#[derive(Debug, Clone, PartialEq)]
pub enum Codec {
    /// 4-segment fader law over 1024 steps: -90 (off) .. +10 dB, 0 dB at 0.75.
    Fader,
    /// Bus-send level: same law, 161 steps.
    SendLevel,
    /// Linear in engineering units.
    Lin { lo: f64, hi: f64, steps: u32 },
    /// Logarithmic (frequencies): `lo * (hi/lo)^x`.
    Log { lo: f64, hi: f64, steps: u32 },
    /// EQ Q: 10 (x = 0) down to 0.3 (x = 1), logarithmic, 72 steps.
    Q,
    /// int 0/1 -> bool.
    Bool,
    /// int 1/0 -> bool inverted (`mix/on` = 1 means *unmuted*).
    BoolInv,
    /// int index -> enum option.
    Enum(&'static [&'static str]),
}

fn snap(x: f64, steps: u32) -> (u32, f64) {
    let max = (steps - 1) as f64;
    let n = (x.clamp(0.0, 1.0) * max).round();
    (n as u32, n / max)
}

fn fader_db(f: f64) -> f64 {
    if f >= 0.5 {
        f * 40.0 - 30.0
    } else if f >= 0.25 {
        f * 80.0 - 50.0
    } else if f >= 0.0625 {
        f * 160.0 - 70.0
    } else {
        f * 480.0 - 90.0
    }
}

fn fader_pos(db: f64) -> f64 {
    if db <= -90.0 {
        0.0
    } else if db < -60.0 {
        (db + 90.0) / 480.0
    } else if db < -30.0 {
        (db + 70.0) / 160.0
    } else if db < -10.0 {
        (db + 50.0) / 80.0
    } else {
        (db + 30.0) / 40.0
    }
}

impl Codec {
    /// Native argument -> generic value (snapped to the grid). `None` if the argument has the wrong type.
    pub fn decode(&self, a: &Arg) -> Option<Value> {
        match self {
            Codec::Bool | Codec::BoolInv => {
                let Arg::Int(i) = a else { return None };
                Some(Value::Bool((*i != 0) != matches!(self, Codec::BoolInv)))
            }
            Codec::Enum(opts) => {
                let Arg::Int(i) = a else { return None };
                opts.get(usize::try_from(*i).ok()?)
                    .map(|s| Value::Enum(s.to_string()))
            }
            _ => {
                let Arg::Float(f) = a else { return None };
                let (steps, lo, hi) = match self {
                    Codec::Lin { lo, hi, steps } | Codec::Log { lo, hi, steps } => (*steps, *lo, *hi),
                    Codec::Fader => (1024, 0.0, 1.0),
                    Codec::SendLevel => (161, 0.0, 1.0),
                    _ => (72, 0.0, 1.0),
                };
                let (_, x) = snap(*f as f64, steps);
                let v = match self {
                    Codec::Fader | Codec::SendLevel => fader_db(x),
                    Codec::Lin { .. } => lo + (hi - lo) * x,
                    Codec::Log { .. } => lo * (hi / lo).powf(x),
                    _ => 10.0 * 0.03f64.powf(x),
                };
                Some(Value::Float(v))
            }
        }
    }

    /// Generic value (already validated against the spec) -> native argument, snapped to the grid.
    pub fn encode(&self, v: &Value) -> Option<Arg> {
        match self {
            Codec::Bool | Codec::BoolInv => {
                let b = v.as_bool()? != matches!(self, Codec::BoolInv);
                Some(Arg::Int(b as i32))
            }
            Codec::Enum(opts) => {
                let s = v.as_str()?;
                Some(Arg::Int(opts.iter().position(|o| *o == s)? as i32))
            }
            _ => {
                let db = v.as_f64()?;
                let (steps, x) = match self {
                    Codec::Fader => (1024, fader_pos(db)),
                    Codec::SendLevel => (161, fader_pos(db)),
                    Codec::Lin { lo, hi, steps } => (*steps, (db - lo) / (hi - lo)),
                    Codec::Log { lo, hi, steps } => (*steps, (db / lo).ln() / (hi / lo).ln()),
                    Codec::Q => (72, (db / 10.0).ln() / 0.03f64.ln()),
                    _ => unreachable!(),
                };
                Some(Arg::Float(snap(x, steps).1 as f32))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db(a: f32) -> f64 {
        Codec::Fader.decode(&Arg::Float(a)).unwrap().as_f64().unwrap()
    }

    #[test]
    fn fader_law_anchor_points() {
        assert_eq!(db(0.0), -90.0);
        assert!((db(0.75) - 0.0).abs() < 0.05, "{}", db(0.75));
        assert!((db(1.0) - 10.0).abs() < 1e-9);
        assert!((db(0.5) + 10.0).abs() < 0.05);
        assert!((db(0.25) + 30.0).abs() < 0.05);
        assert!((db(0.0625) + 60.0).abs() < 0.1);
    }

    #[test]
    fn encode_decode_on_grid() {
        for c in [Codec::Fader, Codec::SendLevel] {
            for d in [-90.0, -45.0, -10.0, 0.0, 6.0, 10.0] {
                let a = c.encode(&Value::Float(d)).unwrap();
                let back = c.decode(&a).unwrap().as_f64().unwrap();
                assert!((back - d).abs() < 0.5, "{c:?} {d} -> {back}");
                // idempotent: what the console stores re-encodes to itself
                assert_eq!(c.encode(&Value::Float(back)).unwrap(), a);
            }
        }
        let c = Codec::Lin {
            lo: -15.0,
            hi: 15.0,
            steps: 121,
        };
        assert_eq!(c.encode(&Value::Float(0.0)), Some(Arg::Float(0.5)));
        assert_eq!(
            c.decode(&Arg::Float(0.75)),
            Some(Value::Float(7.5)),
            "gain 0.75 -> +7.5 dB"
        );
        let hz = Codec::Log {
            lo: 20.0,
            hi: 20000.0,
            steps: 201,
        };
        let a = hz.encode(&Value::Float(1000.0)).unwrap();
        let Value::Float(f) = hz.decode(&a).unwrap() else {
            panic!()
        };
        assert!((f - 1000.0).abs() < 18.0, "{f} (grid step is 3.5%)");
        assert_eq!(
            Codec::Q.decode(&Arg::Float(0.0)).unwrap(),
            Value::Float(10.0)
        );
        let Value::Float(q) = Codec::Q.decode(&Arg::Float(1.0)).unwrap() else {
            panic!()
        };
        assert!((q - 0.3).abs() < 1e-9);
    }

    #[test]
    fn bool_and_enum() {
        assert_eq!(
            Codec::BoolInv.decode(&Arg::Int(1)),
            Some(Value::Bool(false)),
            "mix/on = 1 is not muted"
        );
        assert_eq!(
            Codec::BoolInv.encode(&Value::Bool(true)),
            Some(Arg::Int(0))
        );
        let e = Codec::Enum(&["A", "B"]);
        assert_eq!(e.decode(&Arg::Int(1)), Some(Value::Enum("B".into())));
        assert_eq!(e.decode(&Arg::Int(7)), None);
        assert_eq!(e.decode(&Arg::Float(1.0)), None);
    }
}

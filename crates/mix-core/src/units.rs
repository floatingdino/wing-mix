//! Units and unit conversions used across the workspace (dB, Hz, ms, %, octaves).

use serde::{Deserialize, Serialize};

/// Engineering unit of a parameter or measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Unitless (bools, enums, indices, ratios without a better unit).
    #[default]
    None,
    Db,
    Hz,
    Ms,
    Percent,
    /// Filter quality factor.
    Q,
    /// Compression ratio (x:1).
    Ratio,
    /// Pan/balance position (-100 = left .. +100 = right).
    Pan,
    Octave,
    Seconds,
}

impl Unit {
    pub fn symbol(self) -> &'static str {
        match self {
            Unit::None => "",
            Unit::Db => "dB",
            Unit::Hz => "Hz",
            Unit::Ms => "ms",
            Unit::Percent => "%",
            Unit::Q => "Q",
            Unit::Ratio => ":1",
            Unit::Pan => "",
            Unit::Octave => "oct",
            Unit::Seconds => "s",
        }
    }
}

/// A level in decibels.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Db(pub f64);

/// A frequency in hertz.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Hz(pub f64);

/// A duration in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct Ms(pub f64);

/// dB -> linear amplitude.
pub fn db_to_gain(db: f64) -> f64 {
    10f64.powf(db / 20.0)
}

/// Linear amplitude -> dB (`-inf` for 0).
pub fn gain_to_db(gain: f64) -> f64 {
    20.0 * gain.abs().log10()
}

/// Distance between two frequencies in octaves (signed, `b` relative to `a`).
pub fn octaves_between(a: Hz, b: Hz) -> f64 {
    (b.0 / a.0).log2()
}

impl Db {
    pub fn to_gain(self) -> f64 {
        db_to_gain(self.0)
    }
}

impl Hz {
    /// Shift by `oct` octaves.
    pub fn shift_octaves(self, oct: f64) -> Hz {
        Hz(self.0 * 2f64.powf(oct))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions() {
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert!((gain_to_db(2.0) - 6.0206).abs() < 1e-3);
        assert!(gain_to_db(0.0).is_infinite());
        assert!((octaves_between(Hz(100.0), Hz(400.0)) - 2.0).abs() < 1e-12);
        assert!((Hz(1000.0).shift_octaves(-1.0).0 - 500.0).abs() < 1e-9);
        assert!((Db(0.0).to_gain() - 1.0).abs() < 1e-12);
        assert_eq!(Unit::Db.symbol(), "dB");
    }
}

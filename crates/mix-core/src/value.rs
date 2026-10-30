//! Parameter values exchanged with a surface.
//!
//! Floats are always in engineering units (dB, Hz, ms, %, ...) as described by the parameter's spec; never
//! normalised positions. Enum values are surface-independent tokens chosen by the adapter (`fx.5`, `HALL`, `PRE`).

use serde::{Deserialize, Serialize};
use std::fmt;

/// A parameter value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    Float(f64),
    Int(i64),
    Bool(bool),
    /// One option of an enumerated parameter.
    Enum(String),
    /// Free text (names).
    Text(String),
}

impl Value {
    /// Numeric view: floats, ints, and bools (0/1).
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(v) => Some(*v),
            Value::Int(v) => Some(*v as f64),
            Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
            _ => None,
        }
    }

    /// Boolean view: bools and ints (non-zero = true).
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            Value::Int(v) => Some(*v != 0),
            _ => None,
        }
    }

    /// String view of enum and text values.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Enum(s) | Value::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Equality with an absolute tolerance for numeric values (surfaces quantise to a step grid).
    pub fn approx_eq(&self, other: &Value, tol: f64) -> bool {
        match (self, other) {
            (Value::Enum(a) | Value::Text(a), Value::Enum(b) | Value::Text(b)) => a == b,
            _ => match (self.as_f64(), other.as_f64()) {
                (Some(a), Some(b)) => (a - b).abs() <= tol || (a.is_infinite() && a == b),
                _ => false,
            },
        }
    }

    /// Short type name, for errors.
    pub fn type_name(&self) -> &'static str {
        match self {
            Value::Float(_) => "float",
            Value::Int(_) => "int",
            Value::Bool(_) => "bool",
            Value::Enum(_) => "enum",
            Value::Text(_) => "text",
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Float(v) => write!(f, "{v}"),
            Value::Int(v) => write!(f, "{v}"),
            Value::Bool(v) => write!(f, "{v}"),
            Value::Enum(s) | Value::Text(s) => f.write_str(s),
        }
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}
impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::Int(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views() {
        assert_eq!(Value::Float(-3.0).as_f64(), Some(-3.0));
        assert_eq!(Value::Bool(true).as_f64(), Some(1.0));
        assert_eq!(Value::Int(0).as_bool(), Some(false));
        assert_eq!(Value::Enum("HALL".into()).as_str(), Some("HALL"));
        assert_eq!(Value::Float(1.0).as_str(), None);
    }

    #[test]
    fn approx() {
        assert!(Value::Float(80.0).approx_eq(&Value::Float(80.196), 0.2));
        assert!(!Value::Float(80.0).approx_eq(&Value::Float(81.0), 0.2));
        assert!(Value::Enum("a".into()).approx_eq(&Value::Text("a".into()), 0.0));
        assert!(!Value::Bool(true).approx_eq(&Value::Enum("true".into()), 0.0));
        assert!(Value::Float(f64::NEG_INFINITY).approx_eq(&Value::Float(f64::NEG_INFINITY), 0.0));
    }

    #[test]
    fn serde_round_trip() {
        let v = Value::Enum("fx.5".into());
        let s = serde_json::to_string(&v).unwrap();
        assert_eq!(s, r#"{"type":"enum","value":"fx.5"}"#);
        assert_eq!(serde_json::from_str::<Value>(&s).unwrap(), v);
    }
}

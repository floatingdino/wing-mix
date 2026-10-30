//! Actions: what the engine asks the actuator to do to a surface.
//!
//! Skeleton for M032/M040: an [`Action`] is a small connected batch of [`ParamChange`]s (e.g. EQ band freq + gain +
//! Q) applied and journaled together. Planning metadata (problem, predicted effect) is added by later tasks.

use crate::ids::ParamId;
use crate::value::Value;
use serde::{Deserialize, Serialize};

/// One parameter move.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamChange {
    pub param: ParamId,
    /// Value read before the move (filled by the actuator; `None` while planning).
    pub from: Option<Value>,
    pub to: Value,
}

impl ParamChange {
    pub fn new(param: impl Into<ParamId>, to: impl Into<Value>) -> Self {
        ParamChange {
            param: param.into(),
            from: None,
            to: to.into(),
        }
    }
}

/// A batch of changes applied as one undoable step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Action {
    /// Short human-readable label ("cut 250 Hz on input.3").
    pub label: String,
    pub changes: Vec<ParamChange>,
}

impl Action {
    pub fn new(label: impl Into<String>) -> Self {
        Action {
            label: label.into(),
            changes: Vec::new(),
        }
    }

    pub fn with(mut self, change: ParamChange) -> Self {
        self.changes.push(change);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_batch() {
        let a = Action::new("eq band 2")
            .with(ParamChange::new("input.3/eq/band.2.freq", 250.0))
            .with(ParamChange::new("input.3/eq/band.2.gain", -3.0));
        assert_eq!(a.changes.len(), 2);
        assert_eq!(a.changes[1].to, Value::Float(-3.0));
        assert!(a.changes[0].from.is_none());
    }
}

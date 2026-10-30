//! Jobs the actuator accepts and the event stream it publishes (for the GUI / daemon).

use mix_core::action::Action;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::journal::{Change, Conflict};
use mix_surface::remit::{Reason, Verdict};
use mix_surface::surface::{SoloState, SoloTarget, Surface};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::Duration;

/// Id of a submitted job (moves, inspections, undo, panic and the sub-moves of an inspection).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MoveId(pub u64);

impl fmt::Display for MoveId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// What the measurement callback of an inspection sees.
pub struct InspectView<'a> {
    pub id: MoveId,
    pub target: &'a SoloTarget,
    pub surface: &'a dyn Surface,
    /// Scheduler time (s).
    pub now_s: f64,
}

/// Called once the target has been soloed for `dwell`; returns adjustments to make while it is still soloed.
pub type MeasureFn = Box<dyn FnMut(&InspectView<'_>) -> Vec<Action> + Send>;

/// Solo-inspect workflow: save solo state, clear, solo `target`, dwell, measure, adjust, restore the prior state.
pub struct InspectRequest {
    pub label: String,
    pub target: SoloTarget,
    pub dwell: Duration,
    pub measure: Option<MeasureFn>,
}

impl InspectRequest {
    pub fn new(target: SoloTarget, dwell: Duration) -> Self {
        InspectRequest {
            label: format!("inspect {}", target.strip_id()),
            target,
            dwell,
            measure: None,
        }
    }

    pub fn strip(strip: StripId, dwell: Duration) -> Self {
        Self::new(SoloTarget::strip(strip), dwell)
    }

    pub fn measure(
        mut self,
        f: impl FnMut(&InspectView<'_>) -> Vec<Action> + Send + 'static,
    ) -> Self {
        self.measure = Some(Box::new(f));
        self
    }
}

/// A unit of work, executed one at a time in submission order.
pub enum Job {
    /// One param or a connected batch, ramped together.
    Move(Action),
    Inspect(InspectRequest),
    /// Ramp the last completed move back (params the engineer moved since are left alone).
    Undo,
}

impl Job {
    pub fn label(&self) -> String {
        match self {
            Job::Move(a) => a.label.clone(),
            Job::Inspect(r) => r.label.clone(),
            Job::Undo => "undo".into(),
        }
    }
}

impl From<Action> for Job {
    fn from(a: Action) -> Self {
        Job::Move(a)
    }
}

/// Why a job was not started.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Refusal {
    /// STOP/PANIC is in effect; `resume()` first.
    Stopped,
    Empty,
    TooLarge {
        len: usize,
        max: usize,
    },
    Duplicate {
        param: ParamId,
    },
    /// A model change and params of the same block in one batch (never ramp across a model change).
    MixedModelChange {
        param: ParamId,
    },
    /// The surface rejected the param or value (unknown, read-only, out of range, type).
    Surface {
        param: ParamId,
        error: String,
    },
    /// The remit does not allow the move (now). `confirm_key` lifts a conditional decision.
    Remit {
        param: ParamId,
        verdict: Verdict,
        reason: Reason,
        detail: String,
        confirm_key: Option<String>,
    },
    /// Inspection refused by the remit or the surface.
    Inspection {
        detail: String,
    },
    /// Solo mode is destructive (solo-in-place): inspecting would mute the PA.
    DestructiveSolo,
    NothingToUndo,
}

/// Why a running job ended early. Params already written stay where they are (journaled, undoable).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AbortReason {
    Stopped,
    Panic,
    /// Re-check at write time failed.
    Remit {
        param: ParamId,
        verdict: Verdict,
        reason: Reason,
        detail: String,
    },
    /// The block's model changed under the move.
    ModelChanged {
        strip: StripId,
        block: String,
    },
    Surface {
        param: ParamId,
        error: String,
    },
    /// Solo mode became destructive during an inspection.
    DestructiveSolo,
}

/// Kind of a started job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    Move,
    Inspect,
    Undo,
    Panic,
}

/// Event stream (subscribe with `Actuator::subscribe` / `ActuatorHandle::subscribe`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActuatorEvent {
    Queued {
        id: MoveId,
        label: String,
    },
    Refused {
        id: MoveId,
        label: String,
        reason: Refusal,
    },
    Started {
        id: MoveId,
        label: String,
        kind: JobKind,
        params: Vec<ParamId>,
        duration_s: f64,
    },
    Progress {
        id: MoveId,
        fraction: f64,
    },
    /// The engineer moved `param`: the actuator leaves it alone until `until_s`.
    Yielded {
        param: ParamId,
        value: Value,
        until_s: f64,
    },
    Paused {
        id: MoveId,
    },
    Resumed {
        id: MoveId,
    },
    Done {
        id: MoveId,
        changes: Vec<Change>,
    },
    Aborted {
        id: MoveId,
        reason: AbortReason,
        changes: Vec<Change>,
    },
    /// Inspection: solo state saved and the target soloed.
    Soloed {
        id: MoveId,
        target: SoloTarget,
        saved: SoloState,
    },
    /// Inspection: dwell over, measurement callback ran and asked for `adjustments` sub-moves.
    Measured {
        id: MoveId,
        adjustments: usize,
    },
    /// Inspection: prior solo state restored (`exact`: read back equal to the expected state).
    SoloRestored {
        id: MoveId,
        exact: bool,
    },
    /// Undo/PANIC result: params restored and params left alone because someone else moved them.
    Reverted {
        id: MoveId,
        restored: Vec<ParamId>,
        conflicts: Vec<Conflict>,
        /// Params not restored because the remit or the surface refused them now.
        skipped: Vec<Refusal>,
    },
    /// STOP (or the end of a PANIC): frozen, no job runs until `resume`.
    Stopped,
    /// STOP lifted: queued jobs run again.
    Ready,
}

/// One write sent to the surface (audit log, `Actuator::write_log`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteRecord {
    /// Scheduler time (s).
    pub at_s: f64,
    pub id: MoveId,
    pub param: ParamId,
    /// Value as stored by the surface.
    pub value: Value,
}

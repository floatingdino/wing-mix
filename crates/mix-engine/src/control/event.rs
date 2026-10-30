//! Controller event stream (for the GUI / daemon) and the session report.

use mix_actuator::MoveId;
use mix_core::action::ParamChange;
use mix_core::ids::{ParamId, StripId};
use serde::{Deserialize, Serialize};

use super::config::Mode;
use crate::plan::{PlanScore, PlannedMove};

/// Why the controller (re-)planned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanReason {
    /// First plan of the session (or after a mode change).
    Start,
    /// The previous plan was used up (or nothing in it was executable) and the target is not reached.
    Exhausted,
    /// A batch was reverted (its prediction was wrong).
    Reverted,
    /// The measurement after a kept batch drifted from the plan's prediction.
    Drift,
    /// The music changed section.
    SectionChanged,
    /// The engineer moved something.
    EngineerMoved,
    /// A pending confirmation was given.
    Confirmed,
    /// The actuator was stopped and resumed, or a batch failed.
    Resumed,
}

/// Why the closed loop holds (keeps measuring, makes no moves).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HoldReason {
    /// Within `target_objective`. Resumes when the objective rises past `resume_factor ×` it, the section changes
    /// or the engineer moves something.
    Converged,
    /// The per-session change budget is used up. Never resumes by itself.
    Budget,
    /// No improvement over `no_progress_cycles` cycles or nothing executable left. Resumes on a section change or
    /// an engineer move.
    NoProgress,
}

/// One suggested (or executed) move, flattened for display.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MoveSummary {
    pub label: String,
    pub strip: StripId,
    pub phase: String,
    pub problem: String,
    pub explanation: String,
    pub changes: Vec<ParamChange>,
    /// Objective reduction the planner predicts for this move.
    pub predicted_gain: f64,
    /// Remit confirmation keys this move needs (empty = allowed now).
    pub needs_confirmation: Vec<String>,
    /// A solo inspection precedes the move.
    pub inspect: bool,
}

impl MoveSummary {
    pub fn of(mv: &PlannedMove, inspect: bool) -> Self {
        MoveSummary {
            label: mv.action.label.clone(),
            strip: mv.strip.clone(),
            phase: mv.phase.clone(),
            problem: mv.problem.clone(),
            explanation: mv.explanation.clone(),
            changes: mv.action.changes.clone(),
            predicted_gain: mv.predicted.objective_before - mv.predicted.objective_after,
            needs_confirmation: mv.needs_confirmation.clone(),
            inspect,
        }
    }
}

/// Outcome of a verified batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Kept,
    Reverted,
    /// Could not be judged (section change / engineer move during verification): kept, re-planned.
    Inconclusive,
    /// Refused, aborted or skipped (nothing or only part written).
    Failed,
}

/// One executed batch, for the report.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BatchRecord {
    pub seq: u32,
    pub at_s: f64,
    pub summary: MoveSummary,
    pub outcome: Outcome,
    /// Measured objective before / after (None when not measured).
    pub before: Option<f64>,
    pub after: Option<f64>,
    pub note: String,
}

/// One measurement cycle (the distance curve).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CyclePoint {
    pub at_s: f64,
    pub score: PlanScore,
    /// Batches kept so far.
    pub kept: u32,
}

/// Event stream (subscribe with [`super::Controller::subscribe`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    ModeChanged {
        mode: Mode,
    },
    /// A measurement window closed and was diagnosed.
    Measured {
        at_s: f64,
        score: PlanScore,
        /// The ids of the top problems (most important first).
        top_problems: Vec<String>,
    },
    Planned {
        reason: PlanReason,
        moves: usize,
        predicted: PlanScore,
    },
    /// Suggest mode: the current plan, nothing written.
    Suggested {
        moves: Vec<MoveSummary>,
    },
    BatchStarted {
        seq: u32,
        job: MoveId,
        summary: MoveSummary,
        /// Measured objective before the batch.
        before: f64,
    },
    /// Solo inspection measured the strip (through the actuator's inspect callback).
    Inspected {
        strip: StripId,
        loudness_lufs: Option<f32>,
        active: bool,
    },
    BatchKept {
        seq: u32,
        before: f64,
        after: f64,
        predicted_gain: f64,
    },
    BatchReverted {
        seq: u32,
        before: f64,
        after: f64,
        predicted_gain: f64,
        /// Action type penalised (`phase:block`).
        penalised: String,
    },
    BatchInconclusive {
        seq: u32,
        reason: String,
    },
    BatchFailed {
        seq: u32,
        reason: String,
    },
    /// The hunting guard froze a param for the session.
    ParamFrozen {
        param: ParamId,
        reversals: u32,
    },
    /// A planned move needs the engineer's confirmation (remit conditional); it waits in the pending queue.
    ConfirmationNeeded {
        key: String,
        summary: MoveSummary,
    },
    Confirmed {
        key: String,
    },
    SectionChanged {
        at_s: f64,
        loudness_jump_lu: f32,
        spectral_jump_db: f32,
    },
    EngineerMoved {
        param: ParamId,
    },
    Holding {
        reason: HoldReason,
        objective: f64,
    },
    Resumed {
        reason: PlanReason,
    },
    /// The actuator stopped (STOP, PANIC, safety trip, link loss): the loop waits until it is ready again.
    Paused {
        detail: String,
    },
}

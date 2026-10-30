//! mix-engine: diagnosis, planning, effect prediction, closed-loop controller.
//!
//! - [`diagnosis`] (M031): current [`Features`](mix_core::Features) vs [`TargetProfile`](mix_core::TargetProfile)
//!   → ranked, serialisable [`Problem`]s with evidence, severity, confidence and suggested blocks.
//! - [`plan`] (M032): problems → remit-checked candidate moves scored by predicted effect (WING transfer functions
//!   on the measured features, re-diagnosed) → an ordered plan of batches grouped by workflow phase.
//! - [`control`] (M033): the closed loop: diagnose → plan → execute the next batch through the actuator (solo
//!   inspections measured through its callback) → settle → re-measure → keep or revert; hunting guard, change
//!   budget, section/engineer re-planning, observe/suggest/autopilot modes, an event stream for the GUI.

pub mod control;
pub mod diagnosis;
pub mod plan;

pub use control::{ControlConfig, ControlEvent, Controller, Mode};
pub use diagnosis::{diagnose, Diagnosis, DiagnosisConfig, DiagnosisInput, Problem, ProblemKind};
pub use plan::{plan, Plan, PlanConfig, PlanInput, PlanStep, Remit};

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-engine";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-engine");
    }
}

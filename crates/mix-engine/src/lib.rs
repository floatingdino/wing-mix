//! mix-engine: diagnosis, planning, effect prediction, closed-loop controller.
//!
//! - [`diagnosis`] (M031): current [`Features`](mix_core::Features) vs [`TargetProfile`](mix_core::TargetProfile)
//!   → ranked, serialisable [`Problem`]s with evidence, severity, confidence and suggested blocks.
//! - [`plan`] (M032): problems → remit-checked candidate moves scored by predicted effect (WING transfer functions
//!   on the measured features, re-diagnosed) → an ordered plan of batches grouped by workflow phase.
//! - The closed-loop controller (M033) builds on both.

pub mod diagnosis;
pub mod plan;

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

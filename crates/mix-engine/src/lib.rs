//! mix-engine: diagnosis, planning, effect prediction, closed-loop controller.
//!
//! - [`diagnosis`] (M031): current [`Features`](mix_core::Features) vs [`TargetProfile`](mix_core::TargetProfile)
//!   → ranked, serialisable [`Problem`]s with evidence, severity, confidence and suggested blocks.
//! - Planning (M032) and the closed-loop controller (M033) build on [`diagnosis::Diagnosis`].

pub mod diagnosis;

pub use diagnosis::{diagnose, Diagnosis, DiagnosisConfig, DiagnosisInput, Problem, ProblemKind};

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-engine";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-engine");
    }
}

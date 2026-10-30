//! mix-core: shared types: TargetProfile, Features, Problem, Action, units, time.
//!
//! Scaffold only (M001); implemented by later backlog tasks.

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-core";

// M010: console-facing shared types (ids, values, units, actions).
pub mod action;
pub mod ids;
pub mod units;
pub mod value;

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-core");
    }
}

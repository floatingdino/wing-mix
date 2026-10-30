//! mix-actuator: move scheduler (ramps, batches, solo workflow), safety limits, undo/panic.
//!
//! Scaffold only (M001); implemented by later backlog tasks.

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-actuator";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-actuator");
    }
}

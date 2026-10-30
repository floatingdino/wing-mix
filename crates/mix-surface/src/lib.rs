//! mix-surface: generic console model, Surface trait, Remit, journal.
//!
//! Scaffold only (M001); implemented by later backlog tasks.

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-surface";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-surface");
    }
}

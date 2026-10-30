//! mix-knowledge: mixing knowledge base (data/knowledge/*.toml) with typed access.
//!
//! Scaffold only (M001); implemented by later backlog tasks.

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-knowledge";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-knowledge");
    }
}

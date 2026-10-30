//! mix-capture: audio input enumeration/capture, file source, SPSC ring.
//!
//! Scaffold only (M001); implemented by later backlog tasks.

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-capture";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-capture");
    }
}

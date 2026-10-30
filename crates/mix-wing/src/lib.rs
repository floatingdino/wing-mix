//! mix-wing: WING adapter: OSC client, tree mapping, meters/RTA.
//!
//! Scaffold only (M001); implemented by later backlog tasks.

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-wing";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-wing");
    }
}

#[cfg(test)]
mod proto_tests {
    use wing_proto::osc::{Arg, Message};

    #[test]
    fn wing_proto_codec_roundtrip() {
        let msg = Message::new("/ch/1/fdr", vec![Arg::Float(-6.0)]);
        let back = Message::decode(&msg.to_vec()).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn wing_model_schema_loads() {
        // wing-model embeds its schema blob via include_bytes!; touching a path proves it built as a git dep.
        assert!(wing_model::Schema::get().param_count() > 1000);
    }
}

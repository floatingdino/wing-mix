//! Remit: what the assistant may touch (docs/research/remit.md).
//!
//! - [`policy`]: `wing-mix-remit/1` TOML loader and validation (default-deny, unknown keys/classes/roles rejected).
//! - [`eval`]: the eight-step evaluator for param writes, plus inspection (solo/listen/RTA) decisions.
//! - [`detect`]: role detection (pluggable [`detect::RoleDetector`]; default scorer with policy vocabulary).
//! - [`evidence`]: generic evidence extraction from a [`crate::model::Topology`].
//!
//! Typical use:
//! ```ignore
//! let policy = Policy::load(path)?;
//! let topo = surface.topology()?;
//! let det = ScoringDetector::new(policy.detection.clone());
//! let found = detect_topology(&topo, &policy.detection, &det);
//! let roles = effective_roles(&found, &user_confirmed_roles);
//! let ctx = RemitContext::from_topology(&topo, &roles, &policy, &surface.info().capabilities, solo.is_destructive());
//! let decision = evaluate(&policy, &ctx, &session, &WriteRequest::new(id, spec.class).change(from, to));
//! ```

pub mod detect;
pub mod eval;
pub mod evidence;
pub mod glob;
pub mod policy;

pub use detect::{Detection, Evidence, RoleDetector, ScoringDetector};
pub use eval::{
    evaluate, evaluate_inspection, Decision, InspectionKind, Reason, RemitContext, RemitSend,
    RemitSession, RemitStrip, WriteRequest,
};
pub use evidence::{detect_topology, effective_roles};
pub use policy::{Envelope, Policy, PolicyError, Rule, Verdict};

//! The remit gate: every planned move and every single write asks it first.
//!
//! [`RemitGate`] is the seam between the actuator and policy. [`PolicyGate`] is the real one (policy + live
//! [`RemitContext`] + session confirmations); M041's safety layer can wrap it (it gets the session baseline of the
//! param with every write, for envelopes).

use mix_core::ids::StripId;
use mix_core::value::Value;
use mix_surface::model::Role;
use mix_surface::remit::{
    detect_topology, effective_roles, evaluate, evaluate_inspection, Decision, InspectionKind,
    Policy, RemitContext, RemitSession, ScoringDetector, WriteRequest,
};
use mix_surface::surface::{Surface, SurfaceResult};
use std::collections::BTreeMap;

/// Decides whether a write or an inspection may happen *now*.
pub trait RemitGate: Send {
    /// Decision for one write (`req.from` = current value, `req.to` = value about to be written). `baseline` is the
    /// value the param had before the session first touched it.
    fn check_write(&self, req: &WriteRequest, baseline: &Value) -> Decision;

    /// As [`check_write`](RemitGate::check_write) for a write that *restores* a value the session or the engineer
    /// had before (undo, PANIC, safety revert). Defaults to `check_write`; the safety layer relaxes its size limits
    /// here (never its holds).
    fn check_restore(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        self.check_write(req, baseline)
    }

    /// Decision for a solo/listen of `strip`.
    fn check_inspection(&self, kind: InspectionKind, strip: &StripId) -> Decision;

    /// The surface reported a topology, solo-mode or connection change: rebuild whatever the decisions depend on.
    /// Implementations must fail closed (deny) if they cannot rebuild.
    fn refresh(&mut self, _surface: &dyn Surface) {}

    /// Record a session confirmation (`<rule>`, `coupling:<rule>`, `revive:<rule>`).
    fn confirm(&mut self, _key: &str) {}

    fn revoke(&mut self, _key: &str) {}
}

/// Policy-backed gate: [`evaluate`] over a [`RemitContext`] rebuilt from the surface on every refresh.
pub struct PolicyGate {
    policy: Policy,
    ctx: RemitContext,
    session: RemitSession,
    confirmed_roles: BTreeMap<StripId, Role>,
}

impl PolicyGate {
    /// Build from the surface's current topology (role detection with the policy's vocabulary).
    pub fn new(policy: Policy, surface: &dyn Surface) -> SurfaceResult<Self> {
        Self::with_roles(policy, surface, BTreeMap::new())
    }

    /// As [`PolicyGate::new`], with roles the user confirmed in the GUI.
    pub fn with_roles(
        policy: Policy,
        surface: &dyn Surface,
        confirmed_roles: BTreeMap<StripId, Role>,
    ) -> SurfaceResult<Self> {
        let mut g = PolicyGate {
            policy,
            ctx: RemitContext::default(),
            session: RemitSession::default(),
            confirmed_roles,
        };
        g.ctx = g.build(surface)?;
        Ok(g)
    }

    fn build(&self, surface: &dyn Surface) -> SurfaceResult<RemitContext> {
        let topo = surface.topology()?;
        let solo = surface.solo_state()?;
        let det = ScoringDetector::new(self.policy.detection.clone());
        let found = detect_topology(&topo, &self.policy.detection, &det);
        let roles = effective_roles(&found, &self.confirmed_roles);
        Ok(RemitContext::from_topology(
            &topo,
            &roles,
            &self.policy,
            &surface.info().capabilities,
            solo.is_destructive(),
        ))
    }

    /// Replace the confirmed roles and rebuild the context (session confirmations are kept). Fails closed: if the
    /// surface cannot be read the context is emptied (every write forbidden) until the next refresh. (M053)
    pub fn set_roles(&mut self, confirmed_roles: BTreeMap<StripId, Role>, surface: &dyn Surface) {
        self.confirmed_roles = confirmed_roles;
        self.ctx = self.build(surface).unwrap_or_default();
    }

    pub fn context(&self) -> &RemitContext {
        &self.ctx
    }

    pub fn session(&self) -> &RemitSession {
        &self.session
    }

    pub fn policy(&self) -> &Policy {
        &self.policy
    }
}

impl RemitGate for PolicyGate {
    fn check_write(&self, req: &WriteRequest, _baseline: &Value) -> Decision {
        evaluate(&self.policy, &self.ctx, &self.session, req)
    }

    fn check_inspection(&self, kind: InspectionKind, strip: &StripId) -> Decision {
        evaluate_inspection(&self.policy, &self.ctx, kind, strip)
    }

    fn refresh(&mut self, surface: &dyn Surface) {
        // Fail closed: an empty context resolves no strip, so every write is forbidden until the next refresh.
        self.ctx = self.build(surface).unwrap_or_default();
    }

    fn confirm(&mut self, key: &str) {
        self.session.confirm(key);
    }

    fn revoke(&mut self, key: &str) {
        self.session.revoke(key);
    }
}

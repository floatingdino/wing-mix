//! A remit gate whose confirmed roles can change while the actuator thread runs it (M053).
//!
//! The actuator owns its gate on its own thread, so the daemon cannot reach into a [`PolicyGate`] after the
//! session started. [`LiveRolesGate`] wraps one and shares a [`RolesHandle`] with the daemon: `confirm_roles`
//! stores the new roles in the handle, and the gate rebuilds its remit context (keeping the session
//! confirmations) before answering the next question, so a bus just confirmed as `monitor` is refused on the very
//! next write. It fails closed like [`PolicyGate::refresh`] if the surface cannot be read.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use mix_actuator::{PolicyGate, RemitGate};
use mix_core::ids::StripId;
use mix_core::value::Value;
use mix_surface::remit::{Decision, InspectionKind, WriteRequest};
use mix_surface::{Role, Surface};

type Roles = BTreeMap<StripId, Role>;

/// The daemon's end: publish new confirmed roles to the gate.
#[derive(Clone, Default)]
pub struct RolesHandle(Arc<Mutex<(u64, Roles)>>);

impl RolesHandle {
    pub fn new(roles: Roles) -> Self {
        RolesHandle(Arc::new(Mutex::new((1, roles))))
    }

    /// Replace the roles; the gate picks them up on its next decision.
    pub fn set(&self, roles: Roles) {
        let mut g = self.0.lock().unwrap_or_else(|e| e.into_inner());
        g.0 += 1;
        g.1 = roles;
    }

    fn current(&self) -> (u64, Roles) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

struct Inner {
    gate: PolicyGate,
    gen: u64,
}

/// See the module docs.
pub struct LiveRolesGate {
    inner: Mutex<Inner>,
    roles: RolesHandle,
    surface: Arc<dyn Surface>,
}

impl LiveRolesGate {
    pub fn new(gate: PolicyGate, roles: RolesHandle, surface: Arc<dyn Surface>) -> Self {
        let gen = roles.current().0;
        LiveRolesGate {
            inner: Mutex::new(Inner { gate, gen }),
            roles,
            surface,
        }
    }

    /// Run `f` on the gate after applying any roles published since the last call.
    fn with<R>(&self, f: impl FnOnce(&mut PolicyGate) -> R) -> R {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let (gen, roles) = self.roles.current();
        if gen != g.gen {
            g.gen = gen;
            g.gate.set_roles(roles, &*self.surface);
        }
        f(&mut g.gate)
    }
}

impl RemitGate for LiveRolesGate {
    fn check_write(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        self.with(|g| g.check_write(req, baseline))
    }

    fn check_restore(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        self.with(|g| g.check_restore(req, baseline))
    }

    fn check_inspection(&self, kind: InspectionKind, strip: &StripId) -> Decision {
        self.with(|g| g.check_inspection(kind, strip))
    }

    fn refresh(&mut self, surface: &dyn Surface) {
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        g.gate.refresh(surface);
    }

    fn confirm(&mut self, key: &str) {
        self.with(|g| g.confirm(key));
    }

    fn revoke(&mut self, key: &str) {
        self.with(|g| g.revoke(key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mix_sim::{SimConfig, SimSurface};
    use mix_surface::model::ParamClass;
    use mix_surface::remit::{Policy, Verdict};

    /// A bus confirmed as `monitor` after the gate was built is refused immediately (no new session).
    #[test]
    fn confirmed_monitor_bus_is_refused_on_the_next_write() {
        let sim: Arc<dyn Surface> = Arc::new(SimSurface::new(SimConfig::default()).unwrap());
        let policy = Policy::from_toml_str(crate::daemon::DEFAULT_POLICY).unwrap();
        // bus 5 starts as a confirmed FOH group bus (an unconfirmed bus of unknown role is refused)
        let foh = Roles::from([(StripId::new("bus", 5), Role::FohGroup)]);
        let handle = RolesHandle::new(foh.clone());
        let inner = PolicyGate::with_roles(policy, &*sim, foh.clone()).unwrap();
        let mut gate = LiveRolesGate::new(inner, handle.clone(), sim.clone());
        let bus = StripId::new("bus", 5);
        let req = WriteRequest::new("input.1/send/bus.5/level", ParamClass::SendLevel).change(-10.0, -8.0);
        let base = Value::Float(-10.0);
        let before = gate.check_write(&req, &base);
        assert_ne!(
            before.class,
            Verdict::Forbidden,
            "a send into a FOH group is writable: {}",
            before.detail
        );
        handle.set(Roles::from([(bus, Role::Monitor)]));
        let after = gate.check_write(&req, &base);
        assert_eq!(
            after.class,
            Verdict::Forbidden,
            "confirmed monitor: {}",
            after.detail
        );
        // and back
        handle.set(foh);
        assert_ne!(gate.check_write(&req, &base).class, Verdict::Forbidden);
        gate.confirm("x"); // trait methods still reach the inner gate
    }
}

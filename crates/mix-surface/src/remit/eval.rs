//! The remit evaluator (remit.md section 4): eight ordered steps, first decisive step wins.
//!
//! 1. unknown/read-only class;
//! 2. hard list (+ value predicates);
//! 3. unknown strip/destination/host;
//! 4. locks (owning strip, send destination, fx hosts), unconfirmed, deny tag;
//! 5. rules (highest priority, then most restrictive);
//! 6. unlock (rule tag or session confirmation);
//! 7. coupling cap;
//! 8. revive cap.

use super::glob::glob_match;
use super::policy::{Envelope, Policy, Rule, Verdict};
use crate::model::{Block, ParamClass, Role, StripKind, Topology};
use crate::surface::Capabilities;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Which step decided the final class (names match `data/remit/tests/remit-eval.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    Unknown,
    Readonly,
    Hard,
    Locked,
    Unconfirmed,
    DenyTag,
    DefaultDeny,
    Rule,
    Coupling,
    Revive,
    Inspection,
    /// Not the remit: the actuator's safety layer (envelope, ceiling, feedback guard, watchdog) refused.
    Safety,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Unknown => "unknown",
            Reason::Readonly => "readonly",
            Reason::Hard => "hard",
            Reason::Locked => "locked",
            Reason::Unconfirmed => "unconfirmed",
            Reason::DenyTag => "deny_tag",
            Reason::DefaultDeny => "default_deny",
            Reason::Rule => "rule",
            Reason::Coupling => "coupling",
            Reason::Revive => "revive",
            Reason::Inspection => "inspection",
            Reason::Safety => "safety",
        }
    }
}

/// Result of an evaluation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Decision {
    pub class: Verdict,
    pub reason: Reason,
    /// Deciding rule (steps 5-8).
    pub rule: Option<String>,
    /// The param audibly changes a protected mix (consumed by M041's feedback guard).
    pub coupled: bool,
    /// Limits for an allowed/conditional move (scaled when coupled).
    pub envelope: Option<Envelope>,
    /// The change cannot be ramped (actuator fades the path or acts while inaudible).
    pub discrete: bool,
    /// For a conditional result: the session confirmation key that would allow it (`<rule>`, `coupling:<rule>`,
    /// `revive:<rule>`), if confirming can lift it.
    pub confirm_key: Option<String>,
    /// Human-readable explanation.
    pub detail: String,
}

impl Decision {
    fn deny(reason: Reason, detail: impl Into<String>) -> Self {
        Decision {
            class: Verdict::Forbidden,
            reason,
            rule: None,
            coupled: false,
            envelope: None,
            discrete: false,
            confirm_key: None,
            detail: detail.into(),
        }
    }

    pub fn is_allowed(&self) -> bool {
        self.class == Verdict::Allowed
    }
}

/// A strip as the remit sees it: kind, effective role, tags, chain, sends, fx hosts, re-input coupling.
#[derive(Debug, Clone, PartialEq)]
pub struct RemitStrip {
    pub id: StripId,
    pub kind: StripKind,
    pub role: Role,
    pub tags: Vec<String>,
    pub chain: Vec<Block>,
    pub sends: Vec<RemitSend>,
    /// FX slots: insert points that hold this slot (`(host strip, insert block)`); empty = free.
    pub hosts: Vec<(StripId, Block)>,
    /// The strip's output reaches a protected mix by re-input after all of its processing.
    pub feeds_protected: bool,
}

impl RemitStrip {
    pub fn new(id: StripId, kind: StripKind, role: Role) -> Self {
        RemitStrip {
            id,
            kind,
            role,
            tags: Vec::new(),
            chain: Vec::new(),
            sends: Vec::new(),
            hosts: Vec::new(),
            feeds_protected: false,
        }
    }

    fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }
}

/// A send as the remit sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemitSend {
    pub dest: StripId,
    pub tap_after: Block,
    pub active: bool,
}

/// Everything the evaluator needs about the console at evaluation time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RemitContext {
    pub strips: BTreeMap<StripId, RemitStrip>,
    /// Surface reports a destructive solo mode (solo-in-place).
    pub solo_destructive: bool,
    /// FX models that are external hardware loops.
    pub external_fx_models: Vec<String>,
}

impl RemitContext {
    pub fn insert(&mut self, s: RemitStrip) {
        self.strips.insert(s.id.clone(), s);
    }

    pub fn get(&self, id: &StripId) -> Option<&RemitStrip> {
        self.strips.get(id)
    }

    /// Build from a surface topology and effective roles (missing roles: [`Role::default_for`]).
    pub fn from_topology(
        topo: &Topology,
        roles: &BTreeMap<StripId, Role>,
        policy: &Policy,
        caps: &Capabilities,
        solo_destructive: bool,
    ) -> Self {
        let floor = policy.detection.active_send_floor_db;
        let role_of = |id: &StripId| {
            roles
                .get(id)
                .copied()
                .or_else(|| topo.get(id).map(|s| Role::default_for(s.kind)))
                .unwrap_or(Role::Unknown)
        };
        let protected = |r: Role| policy.locks.roles.contains(&r);
        // feeds_protected: some strip re-inputs this strip's output and is itself protected or sends (actively)
        // into a protected mix.
        let mut feeds_protected: BTreeSet<StripId> = BTreeSet::new();
        for r in &topo.strips {
            if let Some(src) = &r.reinput_of {
                let into_protected = protected(role_of(&r.id))
                    || r.sends
                        .iter()
                        .any(|s| s.is_active(floor) && protected(role_of(&s.dest)));
                if into_protected {
                    feeds_protected.insert(src.clone());
                }
            }
        }
        let mut ctx = RemitContext {
            strips: BTreeMap::new(),
            solo_destructive,
            external_fx_models: caps.external_fx_models.clone(),
        };
        for s in &topo.strips {
            ctx.insert(RemitStrip {
                id: s.id.clone(),
                kind: s.kind,
                role: role_of(&s.id),
                tags: s.tags.clone(),
                chain: s.chain.clone(),
                sends: s
                    .sends
                    .iter()
                    .map(|x| RemitSend {
                        dest: x.dest.clone(),
                        tap_after: x.tap_after.clone(),
                        active: x.is_active(floor),
                    })
                    .collect(),
                hosts: if s.kind == StripKind::Fx {
                    topo.hosts_of(&s.id)
                } else {
                    Vec::new()
                },
                feeds_protected: feeds_protected.contains(&s.id),
            });
        }
        ctx
    }
}

/// Per-session confirmations (journaled by the caller). Keys: rule ids, `coupling:<rule>`, `revive:<rule>`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RemitSession {
    pub confirmed: BTreeSet<String>,
}

impl RemitSession {
    pub fn confirm(&mut self, key: impl Into<String>) {
        self.confirmed.insert(key.into());
    }
    pub fn revoke(&mut self, key: &str) {
        self.confirmed.remove(key);
    }
    pub fn is_confirmed(&self, key: &str) -> bool {
        self.confirmed.contains(key)
    }
}

/// A proposed param write.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteRequest {
    pub param: ParamId,
    /// Class the surface reports for the param (`ParamSpec::class`).
    pub class: ParamClass,
    /// Current value (needed for revive and value predicates).
    pub from: Option<Value>,
    /// Requested value.
    pub to: Option<Value>,
}

impl WriteRequest {
    pub fn new(param: impl Into<ParamId>, class: ParamClass) -> Self {
        WriteRequest {
            param: param.into(),
            class,
            from: None,
            to: None,
        }
    }
    pub fn change(mut self, from: impl Into<Value>, to: impl Into<Value>) -> Self {
        self.from = Some(from.into());
        self.to = Some(to.into());
        self
    }
}

/// Non-write inspection actions (remit.md 4.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionKind {
    Solo,
    Listen,
    Rta,
}

/// Evaluate an inspection action on a strip.
pub fn evaluate_inspection(
    policy: &Policy,
    ctx: &RemitContext,
    kind: InspectionKind,
    strip: &StripId,
) -> Decision {
    if ctx.solo_destructive {
        return Decision::deny(
            Reason::Hard,
            "inspection while the solo mode is destructive (solo-in-place)",
        );
    }
    if ctx.get(strip).is_none() {
        return Decision::deny(
            Reason::Unknown,
            format!("strip {strip} is not in the topology"),
        );
    }
    let class = match kind {
        InspectionKind::Solo => policy.inspection.solo,
        InspectionKind::Listen => policy.inspection.listen,
        InspectionKind::Rta => policy.inspection.rta,
    };
    Decision {
        class,
        reason: Reason::Inspection,
        rule: None,
        coupled: false,
        envelope: None,
        discrete: false,
        confirm_key: None,
        detail: format!("[inspection] {kind:?} = {class}"),
    }
}

/// Evaluate a param write (remit.md section 4).
pub fn evaluate(
    policy: &Policy,
    ctx: &RemitContext,
    session: &RemitSession,
    req: &WriteRequest,
) -> Decision {
    let class = req.class;
    let param = &req.param;

    // 1. class resolution
    match class {
        ParamClass::Unknown => {
            return Decision::deny(Reason::Unknown, "unclassified param (default-deny)")
        }
        ParamClass::Readonly => return Decision::deny(Reason::Readonly, "read-only param"),
        c if c.is_inspection() => {
            return Decision::deny(
                Reason::Readonly,
                "inspection params are not written; use Surface::set_solo",
            )
        }
        _ => {}
    }

    // 2. hard list + value predicates
    if class.is_hard_forbidden() {
        return Decision::deny(Reason::Hard, format!("class {class} is hard-forbidden"));
    }
    let strip_id = param.strip();
    let kind = strip_id.as_ref().and_then(StripKind::of);
    if class == ParamClass::Mute && kind == Some(StripKind::Main) {
        return Decision::deny(Reason::Hard, "main mute is hard-forbidden");
    }
    let to_str = req.to.as_ref().and_then(|v| v.as_str());
    if class == ParamClass::FxModel {
        if let Some(m) = to_str {
            if ctx
                .external_fx_models
                .iter()
                .any(|x| x.eq_ignore_ascii_case(m))
            {
                return Decision::deny(
                    Reason::Hard,
                    format!("fx model {m} is an external hardware loop"),
                );
            }
        }
    }
    if class == ParamClass::InsertSlot {
        if let Some(slot) = to_str.and_then(StripId::parse) {
            match ctx.get(&slot) {
                Some(fx) => {
                    let this_point = (strip_id.clone(), Block::of(param));
                    let elsewhere = fx
                        .hosts
                        .iter()
                        .any(|(h, b)| (Some(h.clone()), Some(b.clone())) != this_point);
                    if elsewhere {
                        return Decision::deny(
                            Reason::Hard,
                            format!("{slot} already sits on another insert"),
                        );
                    }
                }
                None => {
                    return Decision::deny(
                        Reason::Unknown,
                        format!("fx slot {slot} is not in the topology"),
                    )
                }
            }
        }
    }

    // 3. strip resolution
    let Some(strip) = strip_id.as_ref().and_then(|id| ctx.get(id)) else {
        return Decision::deny(
            Reason::Unknown,
            format!("{param}: owning strip is not in the topology"),
        );
    };
    let dest = if param.is_send() {
        match param.send_dest().and_then(|d| ctx.get(&d)) {
            Some(d) => Some(d),
            None => {
                return Decision::deny(
                    Reason::Unknown,
                    format!("{param}: unknown send destination"),
                )
            }
        }
    } else {
        None
    };
    let mut hosts = Vec::new();
    for (h, _) in &strip.hosts {
        match ctx.get(h) {
            Some(s) => hosts.push(s),
            None => {
                return Decision::deny(
                    Reason::Unknown,
                    format!("{}: unknown fx host {h}", strip.id),
                )
            }
        }
    }

    // 4. locks
    let involved: Vec<&RemitStrip> = std::iter::once(strip)
        .chain(dest)
        .chain(hosts.iter().copied())
        .collect();
    if let Some(s) = involved
        .iter()
        .find(|s| policy.locks.roles.contains(&s.role) || policy.locks.kinds.contains(&s.kind))
    {
        return Decision::deny(
            Reason::Locked,
            format!("{} is locked ({} / {})", s.id, s.kind, s.role),
        );
    }
    if let Some(s) = involved.iter().find(|s| s.role == Role::Unknown) {
        return Decision::deny(
            Reason::Unconfirmed,
            format!("{} has an unconfirmed role", s.id),
        );
    }
    if let Some(tag) = &policy.locks.deny_tag {
        if strip.has_tag(tag) {
            return Decision::deny(Reason::DenyTag, format!("{} carries tag {tag}", strip.id));
        }
    }

    // 5. rules
    let matching: Vec<&Rule> = policy
        .rules
        .iter()
        .filter(|r| rule_matches(r, strip, dest, &hosts, param, class))
        .collect();
    let Some(top) = matching.iter().map(|r| r.priority).max() else {
        return Decision::deny(
            Reason::DefaultDeny,
            "no rule allows this param (default-deny)",
        );
    };
    let rule = matching
        .iter()
        .filter(|r| r.priority == top)
        .min_by_key(|r| r.verdict())
        .copied()
        .expect("non-empty");
    let mut d = Decision {
        class: rule.verdict(),
        reason: Reason::Rule,
        rule: Some(rule.id.clone()),
        coupled: false,
        envelope: rule.envelope,
        discrete: rule.discrete,
        confirm_key: None,
        detail: format!("rule {}", rule.id),
    };

    // 6. unlock
    if d.class == Verdict::Conditional {
        let tag_unlock = rule.unlock_tags.iter().any(|t| strip.has_tag(t));
        if tag_unlock || session.is_confirmed(&rule.id) {
            d.class = Verdict::Allowed;
            d.detail = format!(
                "rule {} unlocked by {}",
                rule.id,
                if tag_unlock {
                    "strip tag"
                } else {
                    "session confirmation"
                }
            );
        } else {
            d.confirm_key = Some(rule.id.clone());
        }
    }

    // 7. coupling
    d.coupled = is_coupled(policy, ctx, strip, dest, &hosts, param);
    if d.coupled && d.class > Verdict::Forbidden {
        let cap = policy.coupling.cap(class);
        let key = format!("coupling:{}", rule.id);
        if cap < d.class {
            if cap == Verdict::Conditional && session.is_confirmed(&key) {
                d.detail.push_str("; coupling confirmed");
            } else {
                d.class = cap;
                d.reason = Reason::Coupling;
                d.confirm_key = (cap == Verdict::Conditional).then_some(key);
                d.detail = format!("rule {} capped: coupled to a protected mix", rule.id);
            }
        }
        if d.class == Verdict::Allowed {
            d.envelope = d.envelope.map(|e| e.scaled(policy.coupling.envelope_scale));
        }
    }

    // 8. revive
    if d.class > Verdict::Forbidden && revives(req, policy.revive.floor_db) {
        let cap = policy.revive.class;
        let key = format!("revive:{}", rule.id);
        if cap < d.class && !(cap == Verdict::Conditional && session.is_confirmed(&key)) {
            d.class = cap;
            d.reason = Reason::Revive;
            d.confirm_key = (cap == Verdict::Conditional).then_some(key);
            d.detail = format!("rule {} capped: makes a silent path audible", rule.id);
        }
    }
    if d.class == Verdict::Forbidden {
        d.confirm_key = None;
    }
    d
}

fn rule_matches(
    r: &Rule,
    strip: &RemitStrip,
    dest: Option<&RemitStrip>,
    hosts: &[&RemitStrip],
    param: &ParamId,
    class: ParamClass,
) -> bool {
    fn ok<T: PartialEq>(sel: &Option<Vec<T>>, v: &T) -> bool {
        sel.as_ref().is_none_or(|l| l.contains(v))
    }
    let tag_in = |l: &Vec<String>| l.iter().any(|t| strip.has_tag(t));
    ok(&r.kinds, &strip.kind)
        && ok(&r.roles, &strip.role)
        && ok(&r.classes, &class)
        && r.params
            .as_ref()
            .is_none_or(|g| g.iter().any(|p| glob_match(p, param.as_str())))
        && r.tags_any.as_ref().is_none_or(tag_in)
        && r.tags_none.as_ref().is_none_or(|l| !tag_in(l))
        && (r.dest_kinds.is_none() || dest.is_some_and(|d| ok(&r.dest_kinds, &d.kind)))
        && (r.dest_roles.is_none() || dest.is_some_and(|d| ok(&r.dest_roles, &d.role)))
        && r.fx_free
            .is_none_or(|free| strip.kind == StripKind::Fx && hosts.is_empty() == free)
        && (r.fx_host_kinds.is_none()
            || (!hosts.is_empty() && hosts.iter().all(|h| ok(&r.fx_host_kinds, &h.kind))))
        && (r.fx_host_roles.is_none()
            || (!hosts.is_empty() && hosts.iter().all(|h| ok(&r.fx_host_roles, &h.role))))
}

/// Protected destination for coupling: a locked role, or an unconfirmed one (fail safe), or not in the context.
fn dest_protected(policy: &Policy, ctx: &RemitContext, dest: &StripId) -> bool {
    ctx.get(dest)
        .is_none_or(|d| policy.locks.roles.contains(&d.role) || d.role == Role::Unknown)
}

/// The strip has an active send to a protected mix tapped at or after `block`.
fn coupled_at(policy: &Policy, ctx: &RemitContext, strip: &RemitStrip, block: &Block) -> bool {
    let Some(pos) = strip.chain.iter().position(|b| b == block) else {
        return false;
    };
    strip.sends.iter().any(|s| {
        s.active
            && dest_protected(policy, ctx, &s.dest)
            && strip
                .chain
                .iter()
                .position(|b| *b == s.tap_after)
                .is_some_and(|tap| pos <= tap)
    })
}

fn is_coupled(
    policy: &Policy,
    ctx: &RemitContext,
    strip: &RemitStrip,
    dest: Option<&RemitStrip>,
    hosts: &[&RemitStrip],
    param: &ParamId,
) -> bool {
    if strip.feeds_protected || dest.is_some_and(|d| d.feeds_protected) {
        return true;
    }
    if let Some(block) = Block::of(param).map(|b| b.coupling_position()) {
        if coupled_at(policy, ctx, strip, &block) {
            return true;
        }
    }
    strip.kind == StripKind::Fx
        && strip.hosts.iter().any(|(h, ins)| {
            hosts
                .iter()
                .find(|x| &x.id == h)
                .is_some_and(|host| host.feeds_protected || coupled_at(policy, ctx, host, ins))
        })
}

/// The write makes a silent path audible (remit.md step 8).
fn revives(req: &WriteRequest, floor_db: f64) -> bool {
    let (Some(from), Some(to)) = (&req.from, &req.to) else {
        return false;
    };
    match req.class {
        ParamClass::Fader | ParamClass::SendLevel => {
            matches!((from.as_f64(), to.as_f64()), (Some(a), Some(b)) if a <= floor_db && b > floor_db)
        }
        ParamClass::Mute => from.as_bool() == Some(true) && to.as_bool() == Some(false),
        _ => {
            let on_switch = req.param.leaf() == "on"
                || matches!(req.class, ParamClass::SendOn | ParamClass::InsertEnable);
            on_switch && from.as_bool() == Some(false) && to.as_bool() == Some(true)
        }
    }
}

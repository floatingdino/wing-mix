---
id: M053
title: wing-mixd follow-ups: signals, mid-session roles, RTA safety feed, live-context setting
stage: S5
status: review
owner: agent-M053
depends: [M050]
branch: task/M053
model: sonnet
---

## Goal
From M050's log: clean shutdown on SIGINT/SIGTERM (STOP actuator, restore inspection solo/RTA, final status,
remove socket); apply roles confirmed mid-session immediately (rebuild PolicyGate/controller context safely);
feed the console RTA (mix-wing claim_rta, restored on release) into SafetyMonitor::feed_spectrum when a WING is
connected; expose the live-context target setting (M037: live / broadcast-video / match-record) through the
control protocol once M037 lands (if M037 isn't merged when you start, add the protocol field with a TODO seam and
log it). Update docs/control-protocol.md.

## Acceptance criteria
- [x] Signal test: SIGTERM during autopilot → clean final state, socket removed
- [x] Mid-session role confirmation test (a bus confirmed as monitor stops sends to it immediately)
- [x] RTA feed covered by a test against headless wing-sim (mix-testsim)

## Log
- 2026-10-31 coordinator: created from M050 follow-ups.
- 2026-10-31 agent-M053: done, status -> review.
  - Signals: `signals.rs` (libc `signal`, atomics only; cfg(unix)), `DaemonHandle::wait_or_signal` (main.rs) sends the
    engine a `Msg::Signal` -> `signal` event, quit path (end_session drops the ActuatorHandle = STOP + inspection solo/RTA
    restore, RTA claim released, final status, socket removed). 2nd signal exits 130.
  - Roles: `gate.rs` `LiveRolesGate` + `RolesHandle` wrap PolicyGate; `confirm_roles` publishes, gate rebuilds its context
    before the next decision (confirmations kept); controller rebuilt between batches (`roles_stale`). `applies` is now
    always `now`. Seam elsewhere: `PolicyGate::set_roles` (mix-actuator/src/gate.rs, 7 lines).
  - RTA: `rta.rs` `RtaFeed` thread claims `claim_rta(main.1 post)` (Console now keeps `wing: Arc<WingSurface>`), feeds
    `feed_spectrum("rta:<strip>", ..)` per new meter frame when WING + feedback guard; `safety.rta_feed {frames, source}`.
  - Live context: `set_live_context {context}` (live|broadcast_video|match_record), stored + in status/snapshot + event,
    `applied:false`; TODO(M037) seam in `Daemon::set_live_context` (task/M037 not on main).
  - Files: apps/wing-mixd/{Cargo.toml, src/{gate,rta,signals,daemon,protocol,status,main,lib}.rs, tests/{daemon_signals,
    rta_feed}.rs}, crates/mix-actuator/src/gate.rs, docs/control-protocol.md.
  - Test: `cargo test -p wing-mixd` (rta_feed skips without the wing-sim binary); `make check` passes.
  - Gotchas: RTA claim lives on its own thread (RtaClaim borrows the surface). Roles mid-session reset the controller's
    move history (same as a target reload). Proposed follow-ups: when M037 merges, apply the context transform in
    `set_live_context`/`poll_loading` and flip `applied`; persist the live-context setting in the state dir.

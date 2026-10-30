---
id: M041
title: Safety layer and watchdogs
stage: S4
status: review
owner: agent-M041
depends: [M040, M004]
branch: task/M041
model: sonnet
---

## Goal
Hard limits independent of planner: max gain increase per move/session, mains loudness ceiling, feedback-risk guard (boosts on channels feeding monitor buses, rising narrowband energy detection → auto STOP + revert last move), connection loss handling, watchdog if analysis stalls.

Coordinator note (from M004): capture the session-start baseline of every touched param before the first move (PANIC/revive rely on it); respect the remit coupling layer (params before IEM send taps are tightened/blocked) and revive layer (un-silencing a path needs confirmation).

## Acceptance criteria
- [x] Tests for each guard on FakeSurface/SimSurface
- [x] Documented limits in docs/safety.md

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M041: started on task/M041 (.worktrees/M041). Design: crates/mix-actuator/src/safety.rs (SafetyGate wraps RemitGate; SafetyMonitor fed by analysis, ticked by Actuator::set_safety); link-loss freeze/resync, bounded write_log, RTA save/restore in actuator.rs; Reason::Safety in mix-surface; FakeSurface::set_connected. Core code compiles; next: tests (Fake + Sim feedback), docs/safety.md.
- 2026-10-30 agent-M041: done, status review.
  Files: crates/mix-actuator/src/safety.rs (new: SafetyConfig, SafetyGate wrapping RemitGate, SafetyMonitor, Trip),
  src/{actuator,event,gate,config,lib}.rs (set_safety, on_trip STOP+revert, link loss freeze/resync, RTA save/restore in
  inspections, bounded write_log/history, RemitGate::check_restore), tests/guards.rs (16, FakeSurface), tests/sim_feedback.rs
  (2, SimSurface + StreamingAnalyser), Cargo.toml (dev-deps mix-sim, mix-analysis); crates/mix-surface: Reason::Safety,
  FakeSurface::set_connected; docs/safety.md.
  Test: `cargo test -p mix-actuator`; `make check` passes. Design: gate-level limits (per move/session boost, fader ceiling,
  envelopes from baseline, x0.5 on coupled, discrete only when meters show inaudible); monitor-level watchdogs (mains LUFS/TP,
  feedback = prominent narrowband peak rising gradually over 4 s, analysis stall hold at 2 s / STOP at 10 s). Restores
  (undo/PANIC/revert) bypass size limits but never the holds.
  Gotchas: monitor is clocked by the actuator tick (no wall clock); per-move limit is enforced from the plan-time check
  (per-write steps are tiny). Engineer cool-down delays moves after external_set in tests.
  Proposed follow-ups: (1) wire feed_mains/feed_features/RTA in wing-mixd (M050) and surface Safety/LinkLost events in the
  GUI; (2) tune thresholds on real recordings (M060); (3) Journal is still unbounded; (4) discrete fade of the path is not
  implemented (refused unless inaudible); (5) tap awareness for inspect callback (M021/M033) still open.

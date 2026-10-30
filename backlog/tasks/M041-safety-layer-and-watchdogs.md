---
id: M041
title: Safety layer and watchdogs
stage: S4
status: in_progress
owner: agent-M041
depends: [M040, M004]
branch: task/M041
model: sonnet
---

## Goal
Hard limits independent of planner: max gain increase per move/session, mains loudness ceiling, feedback-risk guard (boosts on channels feeding monitor buses, rising narrowband energy detection → auto STOP + revert last move), connection loss handling, watchdog if analysis stalls.

Coordinator note (from M004): capture the session-start baseline of every touched param before the first move (PANIC/revive rely on it); respect the remit coupling layer (params before IEM send taps are tightened/blocked) and revive layer (un-silencing a path needs confirmation).

## Acceptance criteria
- [ ] Tests for each guard on FakeSurface/SimSurface
- [ ] Documented limits in docs/safety.md

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M041: started on task/M041 (.worktrees/M041). Design: crates/mix-actuator/src/safety.rs (SafetyGate wraps RemitGate; SafetyMonitor fed by analysis, ticked by Actuator::set_safety); link-loss freeze/resync, bounded write_log, RTA save/restore in actuator.rs; Reason::Safety in mix-surface; FakeSurface::set_connected. Core code compiles; next: tests (Fake + Sim feedback), docs/safety.md.

---
id: M040
title: mix-actuator: human-like move scheduler + solo workflow
stage: S4
status: in_progress
owner: agent-M040
depends: [M010]
branch: task/M040
model: opus
---

## Goal
Execute Actions as human-like moves: eased ramps at per-param-kind max rates (dB/s, octaves/s), configurable step rate to avoid flooding the console, one param or a connected batch at a time, solo-inspect workflow (save solo/monitor state, solo strip/bus, dwell, adjust, restore exactly), journal every write, undo, STOP (freeze) and PANIC (revert session).

Coordinator note (from M004): capture the session-start baseline of every touched param before the first move (PANIC/revive rely on it); respect the remit coupling layer (params before IEM send taps are tightened/blocked) and revive layer (un-silencing a path needs confirmation).

## Acceptance criteria
- [ ] Tests on FakeSurface verify ramp shapes, rate limits, batching and exact solo-state restoration
- [ ] STOP/PANIC tests
- [ ] Never writes a param the Remit forbids (checked again at write time)

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M040: started on task/M040 (.worktrees/M040). Plan: core Actuator (virtual-clock tick), ramp math, RemitGate/PolicyGate, thread runner, FakeSurface tests, tick bench.

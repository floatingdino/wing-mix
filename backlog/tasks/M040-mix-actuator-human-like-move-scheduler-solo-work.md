---
id: M040
title: mix-actuator: human-like move scheduler + solo workflow
stage: S4
status: review
owner: agent-M040
depends: [M010]
branch: task/M040
model: opus
---

## Goal
Execute Actions as human-like moves: eased ramps at per-param-kind max rates (dB/s, octaves/s), configurable step rate to avoid flooding the console, one param or a connected batch at a time, solo-inspect workflow (save solo/monitor state, solo strip/bus, dwell, adjust, restore exactly), journal every write, undo, STOP (freeze) and PANIC (revert session).

Coordinator note (from M004): capture the session-start baseline of every touched param before the first move (PANIC/revive rely on it); respect the remit coupling layer (params before IEM send taps are tightened/blocked) and revive layer (un-silencing a path needs confirmation).

## Acceptance criteria
- [x] Tests on FakeSurface verify ramp shapes, rate limits, batching and exact solo-state restoration
- [x] STOP/PANIC tests
- [x] Never writes a param the Remit forbids (checked again at write time)

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M040: started on task/M040 (.worktrees/M040). Plan: core Actuator (virtual-clock tick), ramp math, RemitGate/PolicyGate, thread runner, FakeSurface tests, tick bench.
- 2026-10-30 agent-M040: core + runner committed; tests/moves.rs green (10). Gotcha: x0+(x1-x0)*1.0 != x1 in f64, final step snaps to x1 explicitly. Input eq model is coupled->forbidden on the demo fake. Next: safety tests (remit at write time, solo, yield, STOP/PANIC), runner smoke, bench.
- 2026-10-30 agent-M040: done, status review.
  Files (all in crates/mix-actuator): Cargo.toml (deps mix-core, mix-surface, serde; dev criterion, serde_json),
  src/lib.rs (crate doc = behaviour summary), src/config.rs (ActuatorConfig, Rates, ramp_for), src/ramp.rs (Space
  Linear/Log2/Level{floor}, raised-cosine ease, Track), src/gate.rs (RemitGate trait, PolicyGate: policy + detection +
  RemitContext rebuilt on TopologyChanged/SoloChanged/Reconnected, fails closed), src/event.rs (Job, InspectRequest,
  MoveId, Refusal, AbortReason, ActuatorEvent, WriteRecord), src/actuator.rs (deterministic core, `tick(now)`),
  src/runner.rs (thread runner `spawn` -> ActuatorHandle; commands wake the thread, drop = STOP + join),
  tests/{common/mod.rs,moves.rs,safety.rs}, benches/tick.rs.
  Behaviour: jobs one at a time (FIFO); a batch shares one eased duration (slowest param at its peak rate sets it);
  per-write rate clamp + token-bucket global write cap (batch writes together or stalls, never jumps); bool/enum/int
  single writes; model param + params of its block in one batch refused, ParamsRemapped mid-ramp aborts; remit
  checked at plan and before every write (abort in place); baseline captured before first write; every write in
  write_log, one Journal batch per move; external change -> param yielded for external_cooldown_s, batch paused then
  resumes without that param, queued jobs touching it wait; inspect = save solo/clear/solo/dwell/measure callback/
  ramped adjustments/restore (keeps solos the engineer added; refused in destructive mode, aborted + own solo dropped
  if mode turns destructive; solo config never written); STOP freezes (inspection solo restored), PANIC ramps touched
  params to baseline at restore_speed skipping engineer-moved params (conflicts), then stopped; Undo ramps last move
  back. Undo/PANIC may lift a *revive* cap only (restoring the engineer's prior value).
  Test: `cargo test -p mix-actuator` (4 unit + 10 moves + 11 safety; virtual clock, only the runner smoke test uses
  real time ~0.3 s); `make check` passes. Bench: `cargo bench -p mix-actuator` tick_eq_band_batch ~2.3 us/tick.
  Gotchas: on the demo FakeSurface input EQ *model* is coupled -> forbidden; x0+(x1-x0)*1.0 != x1 in f64, so the
  final step snaps to x1 explicitly.
  Proposed follow-ups: (1) remit `discrete: true` moves (polarity, model) should fade the path or act while
  inaudible; today they are single writes. (2) M041: envelope/feedback guards as a RemitGate wrapper (baseline is
  passed to check_write). (3) PANIC after a model change reports the old model's dependent params as conflicts;
  restore model first, then re-apply dependent baselines. (4) RTA source save/restore workflow like solo
  (wing-control.md section 3). (5) write_log is unbounded: M050 should persist/rotate it. (6) Inspect callback must
  know which tap the capture carries (M021) to measure the soloed strip.

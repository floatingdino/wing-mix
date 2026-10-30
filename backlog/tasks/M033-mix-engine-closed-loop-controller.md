---
id: M033
title: mix-engine: closed-loop controller
stage: S3
status: in_progress
owner: agent-M033
depends: [M032, M040]
branch: task/M033
model: opus
---

## Goal
Run plan step → actuator → wait for settle → re-measure → keep/revert; convergence and stopping criteria, oscillation/hunting guard, per-session change budget, re-plan when the music changes section, observe/suggest/autopilot modes.

## Acceptance criteria
- [ ] On mix-sim degraded fixture: converges to within tolerance of reference within N moves
- [ ] No oscillation on stationary input
- [ ] Suggest mode produces moves without writing

## Log
- 2026-10-30 coordinator: created.
- 2026-10-31 agent-M033: claimed. Design: `mix_engine::control` — deterministic `Controller::tick(now, &mut dyn
  Executor, &mut dyn MixMeter)` state machine (settle → measure window → verify/keep/revert → decide), `Guard`
  (reversal counting, cool-downs, action-type penalties/bans), `SectionDetector` (short-term loudness/ERB jump),
  `ControlEvent` stream, `KeepRule`. Seams: `plan::score` (pub wrapper of the planner objective) in plan/mod.rs;
  mix-engine now depends on mix-actuator. Unit tests (8) green. Next: SimSurface + StreamingAnalyser harness on a
  virtual clock (tests/loop_harness), closed-loop tests.

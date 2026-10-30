---
id: M033
title: mix-engine: closed-loop controller
stage: S3
status: todo
owner: -
depends: [M032, M040]
branch: -
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

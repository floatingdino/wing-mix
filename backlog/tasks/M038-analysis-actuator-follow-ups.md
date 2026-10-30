---
id: M038
title: Analysis + actuator follow-ups: noise floor stat, onset double-count, model-switch ordering
stage: S4
status: in_progress
owner: agent-M038
depends: [M035]
branch: task/M038
model: sonnet
---

## Goal
From M035's log: (1) mix-analysis: add a per-block noise-floor statistic (e.g. p10 of short-block RMS, gated by
onset absence) to Features so bleed vs signal can be told apart; (2) fix the onset detector double-counting drum
hits (test with click trains + real kick/snare excerpts via mix-fixtures); (3) mix-actuator: when a batch changes a
block's model (e.g. gate slot → DS902) and that slot's params, write the model switch first, wait for the
ParamsRemapped event, then write the params (never ramp across a model change). Keep streaming == offline and
zero-alloc tests green; criterion bench unchanged within 10%.

## Acceptance criteria
- [ ] Noise-floor stat with analytic tests
- [ ] Onset rate correct on click trains and real drum excerpts (report)
- [ ] Model-switch ordering test on FakeSurface and mix-wing/wing-sim

## Log
- 2026-10-31 coordinator: created from M035 follow-ups.

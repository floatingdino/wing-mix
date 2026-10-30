---
id: M036
title: Engine tuning pass: safety-aware planning, prediction accuracy, missed faults, feedback guard on music
stage: S3
status: in_progress
owner: agent-M036
depends: [M034, M035]
branch: task/M036
model: opus
---

## Goal
From M033's closed-loop runs:
1. Planner must know safety limits (per-move/session gain caps, fader ceiling) and plan within them (split large
   moves across batches, or prefer cutting others over boosting) instead of proposing moves that get refused.
2. Vocal EQ predictions were ~10× optimistic: find and fix the cause (band mapping, strip→mix propagation, masking).
3. Seed-7 faults never proposed: hat level, OH pan, keys mud, vocal compression — make sure diagnosis+planner
   find them (ground_truth in mix-sim).
4. Feedback guard falsely trips on music (synthetic song 200 Hz–1 kHz, 20–40 s): tune it (harmonicity, note
   duration, consistency across frames, gating by gain changes we made) so it stays off on the synthetic song and
   the real fixture sections while still tripping on M041's feedback test signals. Re-enable it in the loop harness.
5. Realistic starting scenes: `flat_scene` with faders at 0 dB leaves no boost headroom; add a "rough start"
   scene (faders around −10 dB, flat EQ) and use it for real-fixture runs.
6. Plan hygiene seen in the GUI (docs/user/live.png): the same param appears in two separate moves (input.13 fader +2.9 and +2.5; input.7 −1.4 and −3.0) — merge moves on one param; move titles and explanation amounts disagree (title +2.9 dB, text +1.6 dB) — make them consistent.
7. (from M035) add a strip/stem dynamics check so "vocal not compressed" is observable and the planner proposes vocal compression.
Report before/after on seed 7 and the real-fixture song sections (distance vs FOH baseline).

## Acceptance criteria
- [ ] No safety-refused moves in the seed-7 run
- [ ] Prediction error for vocal EQ within M032's tolerance
- [ ] All seed-7 ground-truth faults addressed in the closed loop
- [ ] Feedback guard on in the loop harness with zero false trips on sim + real sections; M041 tests green
- [ ] Real-fixture runs reach or beat the FOH baseline distance on at least 2 of 3 songs (report all)

## Log
- 2026-10-31 coordinator: created from M033 follow-ups.
- 2026-10-31 agent-M036: claimed. Plan: (1) PlanConfig carries safety limits (per-move/session boost, fader
  ceiling, session baseline) and the planner clamps/splits; (2) vocal EQ prediction error investigation;
  (3) seed-7 missed faults; (4) feedback guard tuning in mix-actuator/src/safety.rs only; (5) mix-sim rough-start
  scene; (6) plan hygiene (merge same-param moves, consistent titles/explanations); (7) strip dynamics check.

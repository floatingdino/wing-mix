---
id: M032
title: mix-engine: action planner with DSP effect prediction
stage: S3
status: in_progress
owner: agent-M032
depends: [M031, M004, M012]
branch: task/M032
model: opus
---

## Goal
Map Problems to candidate Actions over available capabilities (respecting Remit), predict each action's effect on Features using wing-blocks/wing-dsp transfer functions (e.g. EQ band response, compressor static curve on measured level stats), score benefit vs change cost (prefer small, conventional moves; cut before boost), and output an ordered plan of moves grouped into connected batches following the workflow order.

Coordinator note (from M015): knowledge values are generic; the WING adapter/planner must clamp/convert: de-esser DS902 freq 800–8000 Hz; dyn ratio min 1.0; wing-blocks gate attack/release are 10–90% times while dynamics.toml uses ~63% tau (convert per model).

## Acceptance criteria
- [ ] Prediction vs SimSurface render error within documented tolerance
- [ ] Never proposes forbidden params (property test over random policies)
- [ ] Plans on fixtures reduce target distance in simulation

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M032: claimed. Plan: `mix_engine::plan` — console state snapshot, WING transfer model (wing-blocks
  StdEq/FilterBlock magnitude, dynamics static curve, engine pan law), Features predictor (strip deltas folded into
  the measured main by power), candidate generator per Problem (kb rules/presets/masking strategies), remit check per
  candidate, greedy scorer using `diagnose()` on the predicted state, phase-ordered plan with solo-inspect steps.
  Validation in `crates/mix-engine/tests/plan_sim.rs` on mix-sim's reference/degraded fixture.

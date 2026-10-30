---
id: M032
title: mix-engine: action planner with DSP effect prediction
stage: S3
status: todo
owner: -
depends: [M031, M004, M012]
branch: -
model: opus
---

## Goal
Map Problems to candidate Actions over available capabilities (respecting Remit), predict each action's effect on Features using wing-blocks/wing-dsp transfer functions (e.g. EQ band response, compressor static curve on measured level stats), score benefit vs change cost (prefer small, conventional moves; cut before boost), and output an ordered plan of moves grouped into connected batches following the workflow order.

## Acceptance criteria
- [ ] Prediction vs SimSurface render error within documented tolerance
- [ ] Never proposes forbidden params (property test over random policies)
- [ ] Plans on fixtures reduce target distance in simulation

## Log
- 2026-10-30 coordinator: created.

---
id: M031
title: mix-engine: diagnosis (current vs target → Problems)
stage: S3
status: in_progress
owner: agent-M031
depends: [M020, M010, M005]
branch: task/M031
model: opus
---

## Goal
Compare current Features (main + per-channel/solo where available) against TargetProfile; produce ranked Problems with evidence and confidence: tonal balance deviations per band, level balance between sources/stems, dynamics (too dynamic/squashed), masking between named sources, low-end issues, harshness, width, depth (FX). Tolerances from knowledge base.

## Acceptance criteria
- [ ] Detects every ground-truth delta in M012 fixtures (or on synthetic Features if M012 not done) with no false positives on identical inputs
- [ ] Problems serialisable for GUI

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M031: claimed. Plan: `mix_engine::diagnosis` (Problem types, mix tonal/tilt/dynamics/stereo checks,
  strip/stem level balance, masking pairs, source problems from sources.toml, intent deltas), stem masking helper
  for `StemTargets.masking`, synthetic + ignored real-fixture tests.

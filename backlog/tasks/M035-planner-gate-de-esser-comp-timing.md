---
id: M035
title: Planner: gate + de-esser candidates, compressor timing model
stage: S3
status: todo
owner: -
depends: [M034]
branch: -
model: opus
---

## Goal
From M032 follow-ups: add gate/expander candidates (bleed/noise-floor problems on drums/toms), de-esser candidates
for sibilance, and model compressor attack/release (and auto-release) effects on crest/attack-ratio predictions,
using wing-blocks models. Also fix the knowledge-base "LEAD" name ambiguity (lead vocal vs lead guitar) with
mix-sources' classifier as the authority for source type.

## Acceptance criteria
- [ ] Gate and de-esser candidates generated, predicted and validated against SimSurface renders
- [ ] Compressor timing prediction error documented and within tolerance
- [ ] Source type for planning comes from mix-sources

## Log
- 2026-10-30 coordinator: created from M032 follow-ups.

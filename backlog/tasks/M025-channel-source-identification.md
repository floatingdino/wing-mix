---
id: M025
title: Channel source identification
stage: S2
status: todo
owner: -
depends: [M020, M010]
branch: -
model: opus
---

## Goal
Classify each input channel's source type (kick, snare, vocal, bass...) from strip name/colour/icon heuristics plus spectral/temporal features captured while soloed or from multichannel capture; confidence + user override. Feeds knowledge-base lookups.

## Acceptance criteria
- [ ] Name heuristics tests (common abbreviations)
- [ ] Feature classifier tests on mix-sim fixtures (>90% on synthetic set)
- [ ] Confidence + override API

## Log
- 2026-10-30 coordinator: created.

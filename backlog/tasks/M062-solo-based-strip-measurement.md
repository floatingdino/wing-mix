---
id: M062
title: Solo-based per-strip measurement when only a stereo tap is available
stage: S3
status: todo
owner: -
depends: [M052, M039]
branch: -
model: opus
---

## Goal
M052 found that with only the main + solo taps (the common real setup: one stereo stream into the app) the planner
proposes nothing on the WING path, because diagnosis/planning rely on per-strip features that only exist with
multichannel taps. The tool must work like a human engineer: when strip features are missing or stale, schedule
solo inspections (save solo → solo strip/bus → dwell → measure on the solo/monitor tap → restore exactly) to build
per-strip features, prioritised by expected value (strips implicated by mix-level problems, source-type priors,
staleness), rate-limited (max inspections per minute, never during a section change), and cached with timestamps.
Planner then uses measured strip features; when a strip can't be inspected (destructive solo mode, no solo
capability) fall back to mix-level moves (fader balance from mix-level problems + knowledge-base expectations)
instead of proposing nothing. Also: per-strip taps from USB (≤24) as an optional faster path when configured.

## Acceptance criteria
- [ ] e2e (b) with main+solo taps only: plan non-empty, distance reduction comparable to multichannel runs (report)
- [ ] Inspections restore solo exactly (observer client verifies on wing-sim)
- [ ] No-solo surface (FakeX32): mix-level fallback produces moves
- [ ] Inspection rate limits + staleness tested

## Log
- 2026-10-31 coordinator: created from M052's main finding.

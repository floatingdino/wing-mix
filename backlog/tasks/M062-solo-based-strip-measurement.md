---
id: M062
title: Solo-based per-strip measurement when only a stereo tap is available
stage: S3
status: in_progress
owner: agent-M062
depends: [M052, M039]
branch: task/M062
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
- 2026-10-31 agent-M062: claimed. Design: mix-engine `control::strips` — (1) StripTap.estimated priors (kb level +
  fader offset, mix-sources prototype spectrum, scaled into the main residual) for eligible strips without a tap:
  diagnosis skips them in strip/stem/intent checks, the planner may only move their faders (mix-level fallback);
  (2) StripBook of solo measurements (values at capture; re-referenced through MixModel when settings changed);
  (3) SweepScheduler: autopilot-only measure-only inspections between batches, priority = source weight x due
  (never/changed/stale/retry) + implicated (mix problem score x share) + planned, rate limit (8/min, 2 s gap),
  section-quiet (pending jump / 6 s after), engineer-solo respected, remit/destructive checked; plan inspections feed
  the book; new measurements join the basis only after a verification (`absorb_book`), then re-plan
  (PlanReason::StripsMeasured). New event `strip_measured`. Unit tests green; next: closed-loop tests (solo only,
  X32 no solo), daemon passthrough, e2e option.

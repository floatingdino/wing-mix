---
id: M052
title: Milestone: end-to-end against wing-simulator
stage: S6
status: todo
owner: -
depends: [M050, M051, M041, M024, M025]
branch: -
model: opus
---

## Goal
Full run: wing-sim daemon + REAPER-free fixture playback (or SimSurface), reference file and description modes, autopilot converges, forbidden params untouched (audit journal), solo state restored, WING-Edit sees smooth moves. Write docs/milestones/e2e.md with results and remaining gaps; add proposed follow-ups.

## Acceptance criteria
- [ ] Scripted e2e run reproducible from docs
- [ ] Audit shows zero forbidden writes
- [ ] Distance-to-target reduction reported per fixture

## Log
- 2026-10-30 coordinator: created.

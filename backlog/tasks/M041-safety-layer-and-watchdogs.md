---
id: M041
title: Safety layer and watchdogs
stage: S4
status: todo
owner: -
depends: [M040, M004]
branch: -
model: sonnet
---

## Goal
Hard limits independent of planner: max gain increase per move/session, mains loudness ceiling, feedback-risk guard (boosts on channels feeding monitor buses, rising narrowband energy detection → auto STOP + revert last move), connection loss handling, watchdog if analysis stalls.

## Acceptance criteria
- [ ] Tests for each guard on FakeSurface/SimSurface
- [ ] Documented limits in docs/safety.md

## Log
- 2026-10-30 coordinator: created.

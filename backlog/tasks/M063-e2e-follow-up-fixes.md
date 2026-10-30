---
id: M063
title: E2E follow-up fixes: RTA claim origin, feedback guard edge cases, PANIC vs engineer moves
stage: S4
status: todo
owner: -
depends: [M052]
branch: -
model: sonnet
---

## Goal
From docs/milestones/e2e.md: (1) the daemon's own RTA claim/restore writes are reported as engineer (External)
moves — tag/suppress own config writes so they never trigger yield/re-plan; (2) feedback guard false trip at
100 Hz on music right after resume — add a post-resume warm-up / re-baselining window; (3) guard never trips on a
−40 LUFS mix — make the relative floor level-independent (or document a minimum level with a status warning);
(4) PANIC after an engineer moved a param and the tool later re-moved it resets to the pre-session value, undoing
the engineer's move — when the engineer moves a param, that value becomes its new baseline for PANIC/undo.
Each with a regression test (FakeSurface/SimSurface; feedback cases with the real analyser).

## Acceptance criteria
- [ ] Four regression tests, M041/M052 tests green
- [ ] docs/safety.md updated

## Log
- 2026-10-31 coordinator: created from M052 findings.

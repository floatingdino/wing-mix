---
id: M010
title: mix-core + mix-surface: generic console model, Surface trait, Remit engine, journal
stage: S1
status: in_progress
owner: agent-M010
depends: [M001, M004]
branch: task/M010
model: opus
---

## Goal
Implement shared types in mix-core (units, Value, Features/TargetProfile skeleton per M003 when available) and the generic surface model in mix-surface per docs/ARCHITECTURE.md: Strip/StripKind/tags, ProcessingBlock, ParamId, ParamSpec (unit, range, taper, step), capabilities, Surface trait (topology, read, write, subscribe, solo, solo_state, meters), Remit policy loader/evaluator (M004 format, default-deny, hard-forbidden list), change Journal with undo. Include FakeSurface (in-memory, configurable topology, records writes) for tests.

## Acceptance criteria
- [ ] Surface trait + FakeSurface with tests
- [ ] Remit evaluator passes the M004 test cases
- [ ] Journal undo/redo tests
- [ ] No WING-specific code in mix-surface

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M010: started in .worktrees/M010. Plan: mix-core ids/value/units/action (separate files; M020 owns
  bands/features/profile); mix-surface model + Surface trait + remit (policy loader, evaluator, detection) + journal +
  testkit (FakeSurface, conformance suite); data/remit/tests tables driven as unit tests.
- 2026-10-30 agent-M010: done: mix-core ids/value/units/action; mix-surface model.rs, surface.rs (trait), remit/
  (policy, eval, detect, evidence, glob). tests/remit_tables.rs drives data/remit/tests/*.toml: 79/79 eval and 45/45
  detection cases pass first try, plus a policy-mutation check. Next: journal, testkit (FakeSurface, conformance).

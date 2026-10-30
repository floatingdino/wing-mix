---
id: M010
title: mix-core + mix-surface: generic console model, Surface trait, Remit engine, journal
stage: S1
status: todo
owner: -
depends: [M001, M004]
branch: -
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

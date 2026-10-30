---
id: M010
title: mix-core + mix-surface: generic console model, Surface trait, Remit engine, journal
stage: S1
status: review
owner: agent-M010
depends: [M001, M004]
branch: task/M010
model: opus
---

## Goal
Implement shared types in mix-core (units, Value, Features/TargetProfile skeleton per M003 when available) and the generic surface model in mix-surface per docs/ARCHITECTURE.md: Strip/StripKind/tags, ProcessingBlock, ParamId, ParamSpec (unit, range, taper, step), capabilities, Surface trait (topology, read, write, subscribe, solo, solo_state, meters), Remit policy loader/evaluator (M004 format, default-deny, hard-forbidden list), change Journal with undo. Include FakeSurface (in-memory, configurable topology, records writes) for tests.

## Acceptance criteria
- [x] Surface trait + FakeSurface with tests
- [x] Remit evaluator passes the M004 test cases
- [x] Journal undo/redo tests
- [x] No WING-specific code in mix-surface

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M010: started in .worktrees/M010. Plan: mix-core ids/value/units/action (separate files; M020 owns
  bands/features/profile); mix-surface model + Surface trait + remit (policy loader, evaluator, detection) + journal +
  testkit (FakeSurface, conformance suite); data/remit/tests tables driven as unit tests.
- 2026-10-30 agent-M010: done: mix-core ids/value/units/action; mix-surface model.rs, surface.rs (trait), remit/
  (policy, eval, detect, evidence, glob). tests/remit_tables.rs drives data/remit/tests/*.toml: 79/79 eval and 45/45
  detection cases pass first try, plus a policy-mutation check. Next: journal, testkit (FakeSurface, conformance).
- 2026-10-30 agent-M010: status review.
  Files: Cargo.toml (workspace dep `toml = "0.8"`, same line as M015); crates/mix-core/{Cargo.toml (serde, identical
  to M020's), src/lib.rs (pub mod lines only), src/{ids,value,units,action}.rs}; crates/mix-surface/{Cargo.toml,
  src/lib.rs, src/model.rs, src/surface.rs, src/journal.rs, src/remit/{mod,policy,eval,detect,evidence,glob}.rs,
  src/testkit/{mod,fake,conformance}.rs, tests/remit_tables.rs}.
  Public API: mix_core::{ids::{ParamId,StripId}, value::Value, units::{Unit,Db,Hz,Ms}, action::{Action,ParamChange}};
  mix_surface::model (StripKind, Role, ParamClass, Block, ParamSpec/ValueKind/Taper, Strip/SendInfo/InsertInfo,
  Topology); surface::Surface (info, topology, params, param_spec, read, write, subscribe, solo_state, set_solo,
  clear_solo, restore_solo, meters, subscribe_meters, rta_source, set_rta_source) + SurfaceEvent{ParamChanged{origin},
  ParamsRemapped, TopologyChanged, ...}, SoloState{mode, active}, SurfaceError::DestructiveSolo; remit::{Policy,
  evaluate, evaluate_inspection, RemitContext::from_topology, RemitSession, Decision, ScoringDetector/RoleDetector,
  detect_topology, effective_roles}; journal::Journal (apply/record/undo/redo/revert_all, conflict-safe);
  testkit::{FakeSurface::demo(), conformance::{run_all, check_*}, ConformanceDriver, ConformanceOptions}.
  Test: `make check` (passes; 78 tests workspace). `cargo test -p mix-surface` runs remit-eval.toml (79/79),
  role-detection.toml (45/45), a policy-mutation check, FakeSurface conformance (+ a broken-surface negative test),
  journal undo/redo/conflict/rollback tests, detection+remit end to end on FakeSurface.
  Deviations from remit.md (all fail-safe or additive):
  1. Coupling treats sends into role `unknown` (or missing) destinations as protected, not only monitor/comms.
  2. Destructive solo blocks all inspection incl. RTA claims (4.5 reads as "inspection"; RTA is listed there).
  3. Scorer pre/post branches need >= 1 active sender (needed by unused/talkback-only cases).
  4. Extra confirmation key `revive:<rule>` (like `coupling:<rule>`); confirmations only lift conditional caps.
  5. Writes to inspect.* classes are refused with reason `readonly` (use Surface::set_solo).
  6. Role adds `none` (fx slots); phrases match as consecutive tokens (remit.md 6.2), not letters-only substrings
     as the policy comment says; equivalent on the tables.
  Proposed follow-ups: M011 implements ConformanceDriver against wing-sim (external client write + cfg/solo/mode SIP)
  and runs conformance::run_all with model_probe = ch eq model; M041 consumes Decision.envelope/coupled; M050
  persists Journal::entries and RemitSession; fix the `phrases` comment in data/remit/wing-default.toml.

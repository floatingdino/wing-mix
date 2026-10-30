---
id: M015
title: mix-knowledge: typed loader + consistency checker
stage: S1
status: done
owner: task-agent-M015
depends: [M001, M005]
branch: task/M015
model: sonnet
---

## Goal
Implement crates/mix-knowledge: serde types for every file in data/knowledge/ (schema per each file's `[schema]`
table and docs/research/mix-knowledge.md), a `KnowledgeBase::load(dir)` plus an embedded default (include_str!),
typed queries (source by name hint, presets by source, rules by condition feature, vocabulary term lookup with
modifiers), and the consistency checker (all cross-references resolve, ranges ordered, units known) as unit tests.
Check WING attack/release/ratio ranges noted in the TOML against wing-simulator's wing-blocks parameter ranges
(read-only, ../wing-simulator or the pinned git dep) and log mismatches.

## Acceptance criteria
- [x] Every TOML file deserialises into typed structs (no untyped `toml::Value` in the public API)
- [x] Checker tests pass; a deliberately broken fixture string fails with a clear error
- [x] Query API covered by tests

## Log
- 2026-10-30 coordinator: created from M005 follow-ups.
- 2026-10-30 M015 agent: typed structs (types.rs), loader/queries (lib.rs), checker (check.rs) written; embedded data parses and passes the checker. Data fixes (internal inconsistency, fundamental_hz outside useful_range_hz): sources.toml piano useful_range_hz lo 30->25, backing_vocal 100->80, speech 90->80. Checker leniencies (documented in check.rs): per-band features may be unsuffixed in conditions/deltas (= any band), rules without conditions are always-on, antonym "" = none. Next: tests, WING range comparison, make check.
- 2026-10-30 M015 agent: DONE, status review. Files: Cargo.toml (workspace dep `toml = "0.8"`), Cargo.lock, crates/mix-knowledge/{Cargo.toml,src/lib.rs,src/types.rs,src/check.rs,src/tests.rs}, data/knowledge/sources.toml (3 value fixes above), this task file.
  Test: `cargo test -p mix-knowledge` (27 tests, ~0.1 s) and `make check` (passes). API: `KnowledgeBase::{load(dir), embedded(), from_raw, parse_unchecked}`, `source_by_name/match_sources`, `presets_for/presets_by_source`, `rules_by_feature/rules_for_source/problems_by_feature`, `term/resolve_phrase/scaled_deltas`, `masking_winner`, `check::check`. Structs use deny_unknown_fields, so schema drift in a TOML file fails loading naming the field.
  WING range check (vs wing-blocks gate.rs/dynamics.rs/eq.rs/filter.rs/models/dyn_eq.rs, pinned rev 957b406; pinned as a test): all gate/comp preset attack/hold/release/range/ratio/mix ranges are inside the console limits. Mismatches: (1) deess_preset vocal_deess and bv_deess freq_hz = [5000, 8500] but WING DS902 `f` clamps 800..8000 Hz (adapter must clamp; data left generic). (2) workflow.toml [blocks.Dyn] note says ratio 1.1..100 but wing-blocks clamps ratio 1.0..100 (note only). (3) Time-constant definitions differ: dynamics.toml says attack/release are ~63 % (tau) times; wing-blocks gate attack/release are 10-90 % times (tau = t/2.197) and comp LOG env uses tau = att/rel, LIN env is constant dB rate; M032 must convert gate times per console model rather than reuse the numbers directly.
  Proposed follow-ups: M032 gate-time conversion (above); optionally add a wing-blocks-backed dev-dependency test once mix-sim exposes block ranges, replacing the hard-coded WING_* tables in tests.rs.
- 2026-10-30 coordinator: merged, make check passes. done. WING mismatches (DS902 800–8000 Hz clamp, ratio 1.0, gate 10–90% times) carried into M011/M032 notes.

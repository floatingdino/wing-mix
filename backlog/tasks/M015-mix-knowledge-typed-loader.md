---
id: M015
title: mix-knowledge: typed loader + consistency checker
stage: S1
status: in_progress
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
- [ ] Every TOML file deserialises into typed structs (no untyped `toml::Value` in the public API)
- [ ] Checker tests pass; a deliberately broken fixture string fails with a clear error
- [ ] Query API covered by tests

## Log
- 2026-10-30 coordinator: created from M005 follow-ups.
- 2026-10-30 M015 agent: typed structs (types.rs), loader/queries (lib.rs), checker (check.rs) written; embedded data parses and passes the checker. Data fixes (internal inconsistency, fundamental_hz outside useful_range_hz): sources.toml piano useful_range_hz lo 30->25, backing_vocal 100->80, speech 90->80. Checker leniencies (documented in check.rs): per-band features may be unsuffixed in conditions/deltas (= any band), rules without conditions are always-on, antonym "" = none. Next: tests, WING range comparison, make check.

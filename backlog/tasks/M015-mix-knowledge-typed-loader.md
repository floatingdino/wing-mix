---
id: M015
title: mix-knowledge: typed loader + consistency checker
stage: S1
status: todo
owner: -
depends: [M001, M005]
branch: -
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

---
id: M024
title: Mix description → TargetProfile
stage: S2
status: todo
owner: -
depends: [M020, M030, M005]
branch: -
model: sonnet
---

## Goal
Turn text like 'warm, vocal-forward, punchy drums, wide' into TargetProfile deltas using data/knowledge/vocabulary.toml; unknown/complex phrasing goes through mix-llm with a JSON schema constrained to the same delta vocabulary. Combine with an optional reference (description modifies reference).

## Acceptance criteria
- [ ] Rule path covers all vocabulary terms with tests
- [ ] LLM path tested with a mocked CLI
- [ ] Works with LLM disabled

## Log
- 2026-10-30 coordinator: created.

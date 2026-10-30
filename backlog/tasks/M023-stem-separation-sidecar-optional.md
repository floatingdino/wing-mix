---
id: M023
title: Stem separation sidecar (optional)
stage: S2
status: todo
owner: -
depends: [M022]
branch: -
model: sonnet
---

## Goal
Optional Python sidecar (uv-managed venv under the state dir) running the separation model chosen in M003 to produce per-stem TargetProfiles (vocals/drums/bass/other). Rust client with timeouts, caching, progress reporting; graceful fallback when absent. Requires human action for model download approval (see docs/HUMAN-ACTIONS.md).

## Acceptance criteria
- [ ] Sidecar install script + Rust client
- [ ] Per-stem profiles for a fixture
- [ ] System works identically (minus stems) when sidecar absent

## Log
- 2026-10-30 coordinator: created.

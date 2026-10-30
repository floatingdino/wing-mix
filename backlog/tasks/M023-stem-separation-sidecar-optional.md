---
id: M023
title: Stem separation sidecar (optional)
stage: S2
status: in_progress
owner: task-agent-M023
depends: [M022]
branch: task/M023
model: sonnet
---

## Goal
Optional Python sidecar (uv-managed venv under the state dir) running the separation model chosen in M003 to produce per-stem TargetProfiles (vocals/drums/bass/other). Rust client with timeouts, caching, progress reporting; graceful fallback when absent. Requires human action for model download approval (see docs/HUMAN-ACTIONS.md).

Coordinator note: implement mix-reference's `StemTargetProvider` hook (see M022 log). Also add mp3/aac decode tests that generate data with ffmpeg at test time when it's on PATH (skip otherwise). Weights download needs human approval (HUMAN-ACTIONS #3): build and test everything with a mock sidecar; the real install script must not run until approved.

## Acceptance criteria
- [ ] Sidecar install script + Rust client
- [ ] Per-stem profiles for a fixture
- [ ] System works identically (minus stems) when sidecar absent

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 task-agent: worktree .worktrees/M023. Plan: tools/stems (uv project + wingmix_stems with stdlib mock backend + install.sh gated by --approved), crates/mix-stems (client, cache, provider), StemInput gains `analysis`, ffmpeg mp3/aac tests in mix-reference. Real install/model download NOT run (awaiting HUMAN-ACTIONS #3).

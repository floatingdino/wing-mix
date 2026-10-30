---
id: M030
title: mix-llm: Claude Code CLI bridge
stage: S3
status: todo
owner: -
depends: [M001]
branch: -
model: sonnet
---

## Goal
Optional bridge invoking `claude -p` (non-interactive) with --output-format json and a JSON schema in the prompt, a configurable model, timeout, max calls per session, cost/usage logging, and an offline/disabled mode. Prompt templates live in data/prompts. Never on the audio path; never blocks the control loop (async with fallback).

## Acceptance criteria
- [ ] Mocked-CLI tests (fake `claude` script on PATH)
- [ ] One real smoke test #[ignore] that calls the CLI
- [ ] Disabled mode returns a typed 'unavailable'

## Log
- 2026-10-30 coordinator: created.

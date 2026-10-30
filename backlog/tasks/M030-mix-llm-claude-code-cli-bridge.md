---
id: M030
title: mix-llm: Claude Code CLI bridge
stage: S3
status: review
owner: task-agent-M030
depends: [M001]
branch: task/M030
model: sonnet
---

## Goal
Optional bridge invoking `claude -p` (non-interactive) with --output-format json and a JSON schema in the prompt, a configurable model, timeout, max calls per session, cost/usage logging, and an offline/disabled mode. Prompt templates live in data/prompts. Never on the audio path; never blocks the control loop (async with fallback).

## Acceptance criteria
- [x] Mocked-CLI tests (fake `claude` script on PATH)
- [x] One real smoke test #[ignore] that calls the CLI
- [x] Disabled mode returns a typed 'unavailable'

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 task-agent M030: implemented crates/mix-llm (LlmBridge::ask/ask_with_usage/ask_prompt/ask_async, LlmConfig, LlmError{Unavailable,Timeout,Cli,Parse}, usage/cost log, call budget, child kill on timeout). CLI flags: `-p <prompt> --output-format json --tools "" --no-session-persistence --json-schema <schema> [--model m]`; reads `structured_output` (verified against real CLI), falls back to JSON in `result`. No `--max-turns` flag exists in this CLI version; the budget is `max_calls` per bridge. Templates: data/prompts/{parse_description,explain_action}.md (`{input}` placeholder). Added serde/serde_json to workspace deps. Files: crates/mix-llm/{Cargo.toml,src/lib.rs}, data/prompts/*, Cargo.toml, Cargo.lock. Test: `cargo test -p mix-llm` (fake-claude shell scripts, unix only); real smoke: `cargo test -p mix-llm -- --ignored`. `make check` passes.
- Proposed follow-ups: real CLI cost per call is ~$0.10 (12k-token default system prompt); consider `--bare` with API key or `--system-prompt` to shrink; Windows `claude.cmd` resolution.

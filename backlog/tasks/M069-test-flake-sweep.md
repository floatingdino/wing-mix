---
id: M069
title: Flaky-test sweep: make every timing-sensitive test deterministic
stage: S6
status: todo
owner: -
depends: [M068]
branch: -
model: sonnet
---

## Goal
Linux CI keeps surfacing timing flakes one at a time: mix-llm spawn (fixed), mix-wing inproc
`preamp_alias_follows_the_patch_and_the_source` (fixed in M068), and now mix-x32 `tests/fake_x32.rs:205`
`writes_use_x32_addresses_and_laws` (UDP to the in-process fake). Sweep the whole workspace instead of chasing
them one by one:
1. Run the full fast test set many times under stress on macOS: e.g. 30× `cargo test --workspace` with a parallel
   CPU hog (`yes > /dev/null` × ncpu), plus `--test-threads` high. Use a Linux container too if docker/podman is
   available; otherwise push task/M069 repeatedly to CI. Collect every failing test.
2. Fix each at the root: replace sleeps and fixed timeouts with barriers or polling on the condition with generous
   deadlines; give UDP fakes acks or poll until the value arrives; isolate ports and temp dirs. Fix production
   races where the test reveals one (as M068 did for barrier()).
3. Add a `make stress` target documenting the stress run.

## Acceptance criteria
- [ ] 30 stressed local runs with 0 failures (list of fixed tests in the Log)
- [ ] 3 consecutive green CI runs on task/M069
- [ ] `make stress` documented in CLAUDE.md

## Log
- 2026-10-31 coordinator: created after the X32 flake on main CI.

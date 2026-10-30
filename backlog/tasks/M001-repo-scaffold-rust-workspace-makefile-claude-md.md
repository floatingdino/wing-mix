---
id: M001
title: Repo scaffold: Rust workspace, Makefile, CLAUDE.md, wing-simulator git deps
stage: S0
status: todo
owner: -
depends: []
branch: -
model: sonnet
---

## Goal
Create the Cargo workspace with empty crates per docs/ARCHITECTURE.md (mix-core, mix-surface, mix-wing, mix-sim, mix-analysis, mix-capture, mix-reference, mix-knowledge, mix-engine, mix-actuator, mix-llm, apps/wing-mixd, apps/wing-mix-cli). Copy conventions from ../wing-simulator: rust-toolchain.toml (stable), .cargo/config.toml (target-cpu=apple-m1 on aarch64-apple-darwin), release profile (thin LTO, cgu=1, panic=abort), Makefile (check, check-full, fmt, build, bench), .gitignore, GitHub Actions CI (macOS + Linux clippy/test). Add wing-proto, wing-model (and wing-dsp/wing-blocks/wing-engine for mix-sim) as git deps pinned to the current rev of https://github.com/floatingdino/wing-simulator, with a commented [patch] section for ../wing-simulator. Verify wing crates build as deps (they may assume workspace-relative paths, e.g. schema.bin include_bytes — note any issue).

## Acceptance criteria
- [ ] `make check` passes on a clean clone
- [ ] Each crate has lib.rs/main.rs with module doc + one trivial test
- [ ] wing-proto and wing-model are usable from mix-wing (a test constructs an OSC message with wing-proto's codec)
- [ ] CLAUDE.md written (commands, crates, RT rules, backlog protocol pointer)

## Log
- 2026-10-30 coordinator: created.

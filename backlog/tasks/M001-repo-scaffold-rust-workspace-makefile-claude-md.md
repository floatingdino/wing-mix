---
id: M001
title: Repo scaffold: Rust workspace, Makefile, CLAUDE.md, wing-simulator git deps
stage: S0
status: review
owner: eng-M001
depends: []
branch: task/M001
model: sonnet
---

## Goal
Create the Cargo workspace with empty crates per docs/ARCHITECTURE.md (mix-core, mix-surface, mix-wing, mix-sim, mix-analysis, mix-capture, mix-reference, mix-knowledge, mix-engine, mix-actuator, mix-llm, apps/wing-mixd, apps/wing-mix-cli). Copy conventions from ../wing-simulator: rust-toolchain.toml (stable), .cargo/config.toml (target-cpu=apple-m1 on aarch64-apple-darwin), release profile (thin LTO, cgu=1, panic=abort), Makefile (check, check-full, fmt, build, bench), .gitignore, GitHub Actions CI (macOS + Linux clippy/test). Add wing-proto, wing-model (and wing-dsp/wing-blocks/wing-engine for mix-sim) as git deps pinned to the current rev of https://github.com/floatingdino/wing-simulator, with a commented [patch] section for ../wing-simulator. Verify wing crates build as deps (they may assume workspace-relative paths, e.g. schema.bin include_bytes — note any issue).

## Acceptance criteria
- [x] `make check` passes on a clean clone
- [x] Each crate has lib.rs/main.rs with module doc + one trivial test
- [x] wing-proto and wing-model are usable from mix-wing (a test constructs an OSC message with wing-proto's codec)
- [x] CLAUDE.md written (commands, crates, RT rules, backlog protocol pointer)

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 eng-M001: scaffold done. Files: Cargo.toml (workspace, 11 crates + 2 apps, pinned wing-simulator rev
  957b406 via git deps in [workspace.dependencies], commented [patch] for ../wing-simulator), Cargo.lock, Makefile,
  rust-toolchain.toml, .cargo/config.toml, .github/workflows/ci.yml (macOS make check, Linux clippy+test),
  .gitignore, CLAUDE.md, crates/*, apps/*. Decision: git deps work as-is (the wing crates' include_bytes/include_str
  paths stay inside the repo checkout, so no path-dep fallback needed). mix-wing tests construct/roundtrip an OSC
  message with wing-proto and load wing-model's Schema; mix-sim depends on wing-model/dsp/blocks/engine (compiles,
  not yet used). dev opt-level=2 for wing-blocks/dsp/engine. Test: `make check` (passes). Windows CI omitted (the
  task asked for macOS + Linux). Gotcha: first build fetches the git repo over https (network needed).
  Proposed follow-ups: unused-dep check once crates grow; consider `cargo deny`/vendoring if the github fetch is flaky in CI.

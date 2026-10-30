---
id: M013
title: Live wing-sim harness with DriverSim (real meters, no HAL driver)
stage: S1
status: done
owner: task-agent-M013
depends: [M011, M012]
branch: task/M013
model: sonnet
---

## Goal
Test harness that runs a headless wing-sim daemon on private ports with `wing_shm::sim::DriverSim` + `--shm-name`, feeding fixture multitrack audio in and capturing outputs, so mix-wing (native protocol, real meters, solo) and the daemon can be integration-tested live without the HAL driver or the user's running wing-sim (which uses 2222/2223). See docs/research/wing-control.md.

## Acceptance criteria
- [x] Test helper starts/stops the daemon + DriverSim, never touching default ports or the user's state dir
- [x] Meters from real audio received by mix-wing in a test (#[ignore] if slow)
- [x] Documented in docs/testing.md

## Log
- 2026-10-30 coordinator: created (follow-up proposed by M002).
- 2026-10-30 M013 agent: added crates/mix-testsim (SimDaemon, ShmDriver, SimConsole, Feed; dev-dependency only, no
  mix-wing dep), wing-shm workspace git dep (same rev), docs/testing.md, crates/mix-wing/tests/live_audio.rs (3 tests,
  ~1.5 s total so none #[ignore]d: meters follow USB sine, synth song drives 12+ meters, USB-out capture of
  ch1->main->USB out 1). mix-wing tests/common `Daemon` now wraps `SimDaemon` (trivial refactor done; M018 need not).
  Gotchas: USB in only feeds a strip once `input.N/source/grp=USB`, `source/in=N`; main.1 fader defaults to -144 dB;
  DriverSim loops its output back until the daemon attaches (capture ignores that). How to test: `cargo test -p
  mix-testsim -p mix-wing`, `make check`. Files: Cargo.toml, Cargo.lock, crates/mix-testsim/*, crates/mix-wing/{Cargo.toml,
  tests/common/mod.rs,tests/live_audio.rs}, docs/testing.md.
  Proposed follow-ups: M018/others use SimConsole for daemon (wing-mixd) integration tests; Feed from mix-fixtures excerpts.
- 2026-10-30 coordinator: merged, make check passes. done.

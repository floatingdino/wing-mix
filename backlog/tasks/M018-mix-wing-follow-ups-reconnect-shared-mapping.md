---
id: M018
title: mix-wing follow-ups: auto-reconnect, preamp alias, shared WING mapping with mix-sim
stage: S1
status: todo
owner: -
depends: [M011, M012, M013]
branch: -
model: sonnet
---

## Goal
From M011/M012 logs: (1) auto-reconnect in mix-wing with resync of the parameter mirror and a `Reconnected` event (actuator already freezes on Disconnected); (2) an `input.N/preamp/gain` alias for `source.<grp>.<n>/g`; (3) move the WING ↔ generic mapping into one place (e.g. a `mix-wing::map` module with no network deps, or a small `mix-wing-map` crate) and make mix-sim use it instead of its copy in `crates/mix-sim/src/map.rs`; both conformance suites must stay green.

Also (from M031): fix the ```ignore doctest in crates/mix-surface/src/remit/mod.rs that fails under `make check-full` (--include-ignored), and wire `mix_engine::diagnosis::stem_masking()` into mix-stems/mix-reference so StemTargets.masking is filled at ingest (or move the fn to a lower crate if the dep direction is wrong).

## Acceptance criteria
- [ ] Reconnect test against headless wing-sim (kill + restart daemon)
- [ ] Alias covered by tests and remit classification
- [ ] Single mapping implementation used by mix-wing and mix-sim

## Log
- 2026-10-30 coordinator: created from M011/M012 follow-ups.

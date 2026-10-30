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
- 2026-10-30 agent-M018: started in .worktrees/M018.
- 2026-10-30 agent-M018: status review. All acceptance criteria met.
  - Shared mapping: new crate `crates/mix-wing-map` (no network deps) holds `map` + `convert` moved from mix-wing
    (`mix_wing::{map, convert}` re-export it). mix-sim's own map.rs is reduced to `SimStrip` + bindings built with
    `mix_wing_map::map::map_node` / `convert::{spec,read,to_wire,apply}` over the sim store, so conformance now
    covers the production mapping. Behavioural consequence: sim exposes the full WING param set per strip and
    `dyn/ratio` is a Float (as on the real console), scenes.rs uses `Value::Float(ratio)`.
  - Auto-reconnect: `NativeClient::reconnect` (new TCP connection, silent full resync of the mirror, new reader;
    mirror/channel/Arc stay valid); `ConnectOptions.reconnect: Option<ReconnectPolicy>` (default on, 250 ms ->
    5 s backoff) drives it from the event thread: `Disconnected`, then (once resynced) `Reconnected` +
    `TopologyChanged`. Stale Disconnected messages from replaced connections are ignored. The meter receiver no
    longer exits on link loss: it waits and redefines the report when the link is back. `None` = old behaviour.
    mix-testsim `SimDaemon::{kill, restart}` (same ports/shm/state dir; factory-default console).
    Test: `wing_sim_auto_reconnect_after_daemon_restart` (kill+restart daemon: events, Disconnected errors, resync to
    defaults, writes/external events/meters resume) and `..._without_reconnect_policy` in tests/wing_sim.rs.
  - Preamp alias: `input.N/preamp/gain` and `/preamp/phantom` (input + aux strips) resolve through `in/conn`
    (alt source honoured via `in/set/altsrc`) to `io/in/<grp>/<n>/{g,vph}`; same class (preamp_gain/phantom) so
    remit treats them like the source (conditional with `AI` tag; phantom forbidden). Listed in `params(strip)`,
    readable/writable, ParamChanged emitted when the source changes or the strip is re-patched. Tests:
    mix-wing-map unit test, tests/inproc.rs `preamp_alias_follows_the_patch_and_the_source`,
    `preamp_alias_is_classified_by_remit`.
  - mix-surface doctest: `remit/mod.rs` block ```ignore -> ```text; same fix for mix-testsim lib.rs and
    mix-surface testkit/conformance.rs (both also failed under --include-ignored).
  - Stem masking at ingest: `stem_masking` + `STORE_MSR_DB` moved to `mix_analysis` (lowest crate; mix-engine
    depends on analysis, reference->engine would be a cycle with its dev-dep); `mix_engine::diagnosis::stem_masking`
    re-exports it. mix-stems provider fills `StemTargets.masking`; mix-reference fills it when a provider left it
    empty. Test `stem_masking_is_filled_at_ingest` (mix-reference/tests/profile.rs).
  - Files: Cargo.toml, CLAUDE.md, docs/ARCHITECTURE.md, crates/mix-wing-map/**, crates/mix-wing/{Cargo.toml,
    src/{lib,client,surface,meters}.rs, tests/{inproc,wing_sim,common/mod}.rs}, crates/mix-sim/{Cargo.toml,
    src/{lib,map,surface,scenes}.rs}, crates/mix-testsim/src/lib.rs, crates/mix-analysis/src/{lib,masking}.rs,
    crates/mix-engine/src/diagnosis/stem_masking.rs, crates/mix-reference/{src/lib.rs,tests/profile.rs},
    crates/mix-stems/src/provider.rs, crates/mix-surface/src/{remit/mod.rs,testkit/conformance.rs}.
  - Test: `make check` and `make check-full` pass (wing-sim binary present, so daemon tests ran).
  Proposed follow-ups: (1) a remote change shows up as `Local` then `External` ParamChanged on the other client
  (seen for source.a.5/g in the in-process console; pre-existing, not investigated); (2) shared-source flag on
  strips when several channels/outputs use the same source (remit A.3); (3) expose the preamp alias in SimSurface;
  (4) on Reconnected the mirror diff is not emitted as ParamChanged (consumers re-read, as the actuator does).
- 2026-10-30 coordinator: merged, make check passes. done. Origin mislabel + reconnect diff events → M019.

---
id: M011
title: mix-wing: WING adapter (OSC client, topology, solo, meters)
stage: S1
status: review
owner: agent-M011
depends: [M010, M002]
branch: task/M011
model: opus
---

## Goal
Implement the WING adapter per docs/research/wing-control.md: discovery, connection, OSC (and native where required) client using wing-proto codec, mapping WING tree ↔ generic ParamIds/specs via wing-model schema, topology discovery (names, bus types, routing for monitor detection), subscribe/read/write, solo/PFL handling, meters. Integration tests against a headless wing-sim daemon started by the test (skip gracefully if the binary isn't built; mark slow tests #[ignore]).

Coordinator note (after M002): use the **native TCP protocol** (+ UDP meters) per docs/research/wing-control.md, OSC only for debugging; re-map positional params when a block model changes; refuse solo while cfg/solo/mode is SIP; restore cfg/rta source after RTA use. Run probes with --shm-name /wingmix-probe-none on non-default ports (the user's own wing-sim uses 2222/2223).

Coordinator note (from M015): knowledge values are generic; the WING adapter/planner must clamp/convert: de-esser DS902 freq 800–8000 Hz; dyn ratio min 1.0; wing-blocks gate attack/release are 10–90% times while dynamics.toml uses ~63% tau (convert per model).

## Acceptance criteria
- [x] Surface impl passes a shared conformance test suite (also run against FakeSurface)
- [x] Integration test: write + read-back + solo round-trip against wing-sim
- [x] Monitor-bus classification wired to remit

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M011: started in .worktrees/M011. Plan: native client (wing_proto::nrp codec + a local
  wing_model::ParamStore mirror fed by a `DA DC` dump, `$`-leaf gets and change events), functional WING<->generic
  map (remit.md A.1; slot leaves named from the active model), topology builder, solo/meters/RTA, conformance via an
  in-process wing_proto NativeServer (fast, in make check) and a headless wing-sim daemon on private ports.
- 2026-10-30 agent-M011: status review.
  Files: crates/mix-wing/{Cargo.toml (+mix-core, mix-surface, wing-blocks for DynMeterLaw), src/lib.rs,
  src/client.rs, src/map.rs, src/convert.rs, src/topology.rs, src/meters.rs, src/discovery.rs, src/surface.rs,
  tests/common/mod.rs, tests/inproc.rs, tests/wing_sim.rs}; data/remit/wing-default.toml (phrases comment, per M010).
  Design: native protocol only (OSC unused). NativeClient keeps a local wing_model::ParamStore mirror (same
  quantisation/model-switch code as the console), filled by `DA DC` + gets of the `$` leaves the dump skips, kept
  current by `D7 h v` events (applied with wing_proto::server::values::apply_value); request completion by counting
  `DE`s; keepalive `DF D1` every 5 s. Reads come from the mirror; writes are validated against the generic spec,
  applied to the mirror and sent as `D7 h <token>` (with_value_token). The map is functional (remit.md A.1): every
  one of the 36,499 schema params maps to one id/class (test: no `unknown`, ids unique, index round-trips); slot
  params are named from the active model (`eq/band.2.gain`, `dyn/threshold`, `fx.N/param.<name>`) so a model change
  re-maps by itself; the dispatcher emits ParamsRemapped (both origins), ParamChanged External/Local,
  TopologyChanged (names, tags, send on/tap/level crossing -90 dB, patching, inserts, FX model, talkback, proc/ptap),
  SoloChanged. Solo: `*/$solo` + listens (eq band via `$solobd`, gate/dyn key, xover); state = fresh gets; refuse
  while cfg/solo/mode SIP (or STUDIO + cfg/solo/mute); clear writes 0 to each active solo and deliberately never
  writes `$stat/solo` (the console's clear-solo logic would race with re-applied solos: wing-engine logic.rs).
  Meters: one report (76 strips V2 + 16 DCA + monitor + RTA = 1026 words), renew 3 s, redefine when packets stop;
  GR decoded through wing_blocks DynMeterLaw per current model (dB of reduction >= 0). RTA: rtasrc numbering
  1..76 / FX slot for FXIN/FXOUT, generic tap names; `WingSurface::claim_rta` restores raw rtasrc+rtatap on drop.
  Conversions: `convert::knowledge_to_wing` / `WingSurface::adapt_value`: GATE/DUCK attack/release tau -> 10-90 %
  (x ln 9), then clamp to the spec range (ratio 1.0 -> 1.1, DS902 freq 800..8000 verified from the schema).
  `impl ConformanceDriver for WingSurface`: a second connection acts as "another client" (external writes, SIP).
  Tests: `cargo test -p mix-wing` (16 unit + 6 in-process-server tests incl. conformance::run_all, ~1 s; 3 wing-sim
  daemon tests ~1.3 s, skip with a message if ../wing-simulator/target/release/wing-sim is missing or
  WING_SIM_BIN unset). Daemons run on random even ports 20000-48000 (+1 for OSC), `--shm-name
  /wingmix-test-<pid>-<n>`, temp state dir, `--no-hostio --no-autosave`; port 2222/2223 never used.
  Slow (#[ignore]): wing_sim_meter_renewal_and_keepalive (12 s, idle-timeout 7: meter renewal past the 5 s lifetime
  and an idle connection kept alive). `make check` passes (45 s total); full mix-wing incl. ignored passes.
  Proposed follow-ups:
  1. Auto-reconnect + resync in WingSurface (now: Disconnected event, calls fail with Disconnected) - M041/M050.
  2. Strip alias `<input>/preamp/gain` resolved through in/conn (remit A.1/A.3 shared-source); today preamp gain is
     reachable as `source.<grp>.<n>/g` only, and channel `$g` as read-only `input.N/state/in.set.g`.
  3. Verify on real hardware: `DA DC` full dump (marked [H] in the sim), meter UDP source, `pon` meaning (mapped to
     send_tap as `tap.pon`, forbidden by default).
  4. Shared WING path layer for mix-sim (M012): reuse `mix_wing::{map, convert, topology}` over its in-process store
     (they only need a `ParamStore`), so SimSurface and WingSurface name params identically.

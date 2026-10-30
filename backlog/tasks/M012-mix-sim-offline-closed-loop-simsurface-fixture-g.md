---
id: M012
title: mix-sim: offline closed-loop SimSurface + fixture generator
stage: S1
status: done
owner: agent-M012
depends: [M010, M002]
branch: task/M012
model: opus
---

## Goal
Implement SimSurface: a Surface backed by in-process wing-model + wing-engine (or wing-render) so tests can set params and render N seconds of the resulting mix (main and solo taps) from multitrack input. Fixture generator: deterministic synthetic multitrack songs (kick/snare/hat/bass/keys/vocal-like signals; seeded) plus a 'reference' rendered with a known 'good' scene and a 'degraded' start scene (known offsets: wrong gains, muddy EQ, no comp). Keep fixtures generated at test time (no large audio committed).

Coordinator note (after M002): follow docs/research/wing-control.md §mix-sim — `wing_sim::audio::prepare(&mut store)` → keep ConsoleControl, `Console::build`, per block `store.set`/`ctl.refresh`/`console.cycle`, taps via `nodes().tap_port`; NOT wing_render::build_console. Needs a git dep on the wing-sim lib crate. Fixtures patch inputs to USB.

## Acceptance criteria
- [x] SimSurface passes the Surface conformance suite
- [x] Render 10 s stereo of a 16-channel fixture in < 2 s release
- [x] Degraded vs reference fixture pair with documented ground-truth deltas

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M012: started on task/M012 (.worktrees/M012). Plan: git dep on wing-sim lib (same rev), SimSurface
  over prepare/Console::build/cycle with LevelBank meters; schema-driven minimal WING<->generic mapping inside
  mix-sim (to be unified with mix-wing M011 later); synthetic multitrack + good/degraded scenes; real-fixture loader.
- 2026-10-30 agent-M012: done, status review.
  - Dep: `wing-sim` git dep (lib `wing_sim`) at the same pinned rev 957b406 in `[workspace.dependencies]` (+ commented
    `[patch]` line, dev opt-level 2 for wing-sim and mix-sim). Builds/clippy clean for macOS, Linux, Windows targets.
  - `crates/mix-sim/src/surface.rs`: `SimSurface` (Surface + ConformanceDriver) over `prepare` -> `Console::build`
    (LevelBank meters, `workers` configurable, default 0) -> warm-up -> `cycle` per 64-frame block. Keeps
    `ConsoleControl` and calls `ctl.refresh` after every write/solo. `load_inputs(Multitrack)` patches each track's
    WING channel to its USB input; `render(secs, &[TapPoint])` returns Main/Bus/Solo/Monitor/InputPost taps and
    advances the input; `meters()` = peaks of the last render (IN/PRE/OUT, gate/dyn GR as positive dB, solo bus);
    `subscribe_meters` pushes a frame per render; `external_set`, `set_solo_mode` (SIP = destructive), raw
    `set_wing`/`get_wing`/`patch_usb` for setup. Exposes input.1..N (N <= 40), bus.1..16, main.1..4. No RTA/listen.
  - `map.rs`: minimal WING<->generic mapping (in/filter/gate/eq/dyn/fader/mute/pan/width/mono/sends + read-only
    `state/mute`), specs from the active wing-model descriptor; STD EQ -> `low.*`/`band.N.*`/`high.*`, dynamics keys
    spelled out (`threshold`, `attack`...), other models keep the WING key; ratio = enum token (`"4.0"`).
  - `synth.rs`: seeded 16-track song (drums, OH pair, bass, keys pair, KS guitar, pad, lead, formant vocal + 2 BVs,
    shaker), bit-deterministic. `scenes.rs`: `reference_scene`, `degraded_scene`, `ground_truth()` (10 documented
    deltas: vocal -8 dB + comp off, bass +6, keys +8 dB @250 Hz, GTR HPF off, hats +8, snare -6, OH panned centre),
    `FixturePair`, `console_with`. `real.rs`: `load_excerpt`/`load_window` (mix-fixtures; tracks <= 40 -> same WING
    channel/USB input; the 5 unlabelled tracks numbered 43/52/53/55/56 go to the lowest free channels), `flat_scene`.
  - Tests (make check): conformance (8 ch + full 40 ch, external events, SIP refusal, EQ/dyn model remap), mapping
    units, synth determinism, scene diff == ground truth, meters show the deltas, undoing the deltas reproduces the
    reference render bit-exactly, LIVE solo -> solo/monitor taps only, render continuity, workers 0 vs 3 identical,
    closed-loop step, real excerpt 2 s (skips without fixture), perf. `#[ignore]`: real 30 s window.
  - Perf: 10 s x 16 ch -> main in 0.41 s release, single thread (`tests/perf.rs`, asserted < 2 s in release);
    30 s x 28 ch real excerpt 1.1 s. Bench: `cargo bench -p mix-sim` (benches/render.rs).
  - Results: `make check` passes (~54 s here); `cargo test -p mix-sim --release -- --include-ignored` passes.
  - Proposed follow-ups: (1) unify `mix-sim/src/map.rs` with mix-wing's mapping (M011) into one shared WING path
    layer so conformance covers the production mapping; (2) confirm what recorder tracks 43/52/53/55/56 are
    (M014/M025) and fix their channel mapping; (3) RTA + listen points in SimSurface (RtaFeed is available
    in-process); (4) topology `outputs`/inserts/proc order are simplified (no output patch, fixed GEDI chain).
- 2026-10-30 coordinator: cherry-picked onto main, make check passes. done. Open: real-fixture tracks 43/52/53/55/56 placement guessed (asked user); unify map.rs with mix-wing after M011.

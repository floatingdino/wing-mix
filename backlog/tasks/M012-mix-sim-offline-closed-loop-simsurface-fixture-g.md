---
id: M012
title: mix-sim: offline closed-loop SimSurface + fixture generator
stage: S1
status: in_progress
owner: agent-M012
depends: [M010, M002]
branch: task/M012
model: opus
---

## Goal
Implement SimSurface: a Surface backed by in-process wing-model + wing-engine (or wing-render) so tests can set params and render N seconds of the resulting mix (main and solo taps) from multitrack input. Fixture generator: deterministic synthetic multitrack songs (kick/snare/hat/bass/keys/vocal-like signals; seeded) plus a 'reference' rendered with a known 'good' scene and a 'degraded' start scene (known offsets: wrong gains, muddy EQ, no comp). Keep fixtures generated at test time (no large audio committed).

Coordinator note (after M002): follow docs/research/wing-control.md §mix-sim — `wing_sim::audio::prepare(&mut store)` → keep ConsoleControl, `Console::build`, per block `store.set`/`ctl.refresh`/`console.cycle`, taps via `nodes().tap_port`; NOT wing_render::build_console. Needs a git dep on the wing-sim lib crate. Fixtures patch inputs to USB.

## Acceptance criteria
- [ ] SimSurface passes the Surface conformance suite
- [ ] Render 10 s stereo of a 16-channel fixture in < 2 s release
- [ ] Degraded vs reference fixture pair with documented ground-truth deltas

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M012: started on task/M012 (.worktrees/M012). Plan: git dep on wing-sim lib (same rev), SimSurface
  over prepare/Console::build/cycle with LevelBank meters; schema-driven minimal WING<->generic mapping inside
  mix-sim (to be unified with mix-wing M011 later); synthetic multitrack + good/degraded scenes; real-fixture loader.

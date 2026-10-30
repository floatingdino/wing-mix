---
id: M012
title: mix-sim: offline closed-loop SimSurface + fixture generator
stage: S1
status: todo
owner: -
depends: [M010, M002]
branch: -
model: opus
---

## Goal
Implement SimSurface: a Surface backed by in-process wing-model + wing-engine (or wing-render) so tests can set params and render N seconds of the resulting mix (main and solo taps) from multitrack input. Fixture generator: deterministic synthetic multitrack songs (kick/snare/hat/bass/keys/vocal-like signals; seeded) plus a 'reference' rendered with a known 'good' scene and a 'degraded' start scene (known offsets: wrong gains, muddy EQ, no comp). Keep fixtures generated at test time (no large audio committed).

## Acceptance criteria
- [ ] SimSurface passes the Surface conformance suite
- [ ] Render 10 s stereo of a 16-channel fixture in < 2 s release
- [ ] Degraded vs reference fixture pair with documented ground-truth deltas

## Log
- 2026-10-30 coordinator: created.

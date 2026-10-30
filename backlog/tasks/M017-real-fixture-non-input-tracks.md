---
id: M017
title: Real fixture: treat 43/52/53/55/56 as non-inputs (FOH + video mix taps)
stage: S1
status: review
owner: task-agent
depends: [M012, M014]
branch: task/M017
model: sonnet
---

## Goal
Per the user (fixtures/README.md "Non-input tracks"): 43 is empty, 52/53 are the FOH main mix L/R, 55/56 the video
mix L/R. Update mix-fixtures (track kinds: Input / HumanMix{Foh,Video} / Empty; stereo pairing; an API to read the
FOH and video mixes for an excerpt) and mix-sim real.rs (stop patching these into input channels; keep 23 real
inputs on their own WING channel numbers). Add a helper giving (sim render of the multitrack with a given scene,
recorded FOH mix) for the same excerpt, so later tasks can compare the tool's mix with the human engineer's.

## Acceptance criteria
- [x] Track kinds exposed and tested (synthetic RPP/WAV) + real-fixture test (skips without fixtures)
- [x] mix-sim real loader no longer patches non-input tracks; its tests updated
- [x] FOH/video excerpt reader with an #[ignore] real test

## Log
- 2026-10-30 coordinator: created from the user's answer.
- 2026-10-30 task agent: done. mix-fixtures: `TrackKind {Input, HumanMix(HumanMix::{Foh,Video}, Side), Empty}`, `kind_of`, `AudioTrack::kind`, `Fixture::inputs/human_mix/foh_mix`, `Excerpt::human_mix`, `StereoMix` (is_input/NON_INPUT_CHANNELS from M016 kept). mix-sim real.rs: only the 23 inputs are patched (no more remap of 43/52/53/55/56 onto free channels), `RealExcerpt.{foh,video}`, `render_vs_foh(ex, scene) -> FohComparison{sim, foh}`. Files: crates/mix-fixtures/src/lib.rs, crates/mix-sim/src/real.rs, crates/mix-sim/tests/real_fixture.rs (tests live there, not in real.rs). Tests: synthetic kinds/mix test, real track-kind test, #[ignore] real 30 s FOH/video + sim_vs_foh_30s. `make check` passes; ignored real tests pass locally.

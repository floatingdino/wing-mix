---
id: M017
title: Real fixture: treat 43/52/53/55/56 as non-inputs (FOH + video mix taps)
stage: S1
status: todo
owner: -
depends: [M012, M014]
branch: -
model: sonnet
---

## Goal
Per the user (fixtures/README.md "Non-input tracks"): 43 is empty, 52/53 are the FOH main mix L/R, 55/56 the video
mix L/R. Update mix-fixtures (track kinds: Input / HumanMix{Foh,Video} / Empty; stereo pairing; an API to read the
FOH and video mixes for an excerpt) and mix-sim real.rs (stop patching these into input channels; keep 23 real
inputs on their own WING channel numbers). Add a helper giving (sim render of the multitrack with a given scene,
recorded FOH mix) for the same excerpt, so later tasks can compare the tool's mix with the human engineer's.

## Acceptance criteria
- [ ] Track kinds exposed and tested (synthetic RPP/WAV) + real-fixture test (skips without fixtures)
- [ ] mix-sim real loader no longer patches non-input tracks; its tests updated
- [ ] FOH/video excerpt reader with an #[ignore] real test

## Log
- 2026-10-30 coordinator: created from the user's answer.

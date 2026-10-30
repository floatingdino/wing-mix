---
id: M064
title: Instant A/B/X switching: realised mix vs prior
stage: S5
status: todo
owner: -
depends: [M062]
branch: -
model: opus
---

## Goal
Let the engineer audition the tool's realised mix (B) against the prior state (A) and blind-test them (X), with
instant switching.
- **States:** A = a chosen reference point: session baseline (default), or before the last batch / a journal
  checkpoint. B = current realised values. Only params the tool touched are switched; everything else, including
  the engineer's own moves (M063 re-based restore values), stays as it is.
- **Switching:** an atomic snap of the touched-param set to A or B values. It is not ramped, as a deliberate
  exception to human-like ramps, and it is sent as one console batch so the switch is near-instant (measure the
  latency on wing-sim). Safety and remit still apply: a snap may never exceed the session envelope, and discrete
  params (models) are included only when the A and B models are equal, otherwise refuse with a reason. The
  controller and actuator are paused while A/B/X is active, and the daemon refuses autopilot moves. Leaving A/B/X
  always returns to B, or to A if the engineer chooses "keep A", which becomes a journal batch that can be undone.
- **X (blind):** a randomised X that is A or B, hidden from the UI. The engineer answers "X = A" or "X = B".
  Trials and results are logged to `<state-dir>/abx/<session>.jsonl`, with a running score and a p-value.
- **Live caveat:** switching on a live console changes what the audience hears. The GUI must warn about this and
  require confirmation on first use per session. Optional stretch, logged as a follow-up if not done: a "preview"
  mode that renders A/B through an embedded sim from multichannel taps to a headphone output without touching the
  house.
- **Interfaces:** control-protocol commands (`abx_enter{ref}`, `abx_select{A|B|X}`, `abx_answer`, `abx_exit{keep}`)
  and events. WingMix.app gets A/B/X buttons and hotkeys (1/2/3, Enter to answer), clear "A/B/X active" state,
  and the score.

## Acceptance criteria
- [ ] Snap switching tested on FakeSurface, SimSurface and wing-sim (mix-testsim): all touched params reach
      A or B values in one batch, with latency reported
- [ ] Engineer moves during A/B/X are preserved; remit/envelope refusals tested
- [ ] ABX trial logging and scoring tested; X assignment unbiased
- [ ] Exit returns to B (or A on keep, undoable); controller resumes cleanly
- [ ] Protocol documented; WingMix A/B/X UI with screenshot; `make check` and `make app` pass

## Log
- 2026-10-31 coordinator: created at the user's request.

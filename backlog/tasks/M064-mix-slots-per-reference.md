---
id: M064
title: Mix slots: store console states per reference and switch instantly (A/B/C/D…)
stage: S5
status: todo
owner: -
depends: [M062]
branch: -
model: opus
---

## Goal
Let the engineer keep several realised mixes, one per reference or description, and switch between them instantly
to compare. This replaces the earlier A/B/X blind-test idea, at the user's request.
- **Slots:** a named slot (A, B, C, D… and user-renamable) stores:
  - the console values of the remit-allowed params the tool manages;
  - the TargetProfile/reference it was made for (file name, description, live context);
  - when and from which session it was captured, plus the scene/console identity.
  A built-in "Original" slot holds the session baseline. A slot is captured on demand ("store current as C") or
  automatically when a session converges.
- **Switching set:** the union of params that differ between any stored slot and the current state, restricted to
  params the remit allows writing now. Hard-forbidden, monitor/matrix/comms and locked params are never part of a
  slot. Params the engineer moved since a slot was captured are reported; the engineer decides per switch whether
  to apply the slot's value or keep theirs. The default is to keep the engineer's value.
- **Instant recall:** an atomic snap of the switching set, not ramped, as a deliberate exception to human-like
  ramps, sent as one console batch (measure the latency on wing-sim).
  - The remit and safety envelope still apply.
  - Discrete params (block models) switch model first, then that block's params (M038 ordering). The UI states that
    a model change may click.
  - The controller and actuator pause while comparing. Switching also makes that slot's reference the daemon's
    active target, so the live view shows the distance to the right target.
  - "Commit slot X" ends the comparison on X as an undoable journal batch.
  - "Resume tuning" continues autopilot from the current slot.
- **Storage:** slots persist in `<state-dir>/slots/<console>+<scene>.json`, versioned. They are refused, with a
  reason, if the console topology or the param mapping changed in a way that makes them unsafe to recall.
- **Live caveat:** recalling a slot on a live console changes what the audience hears. The GUI warns and requires
  confirmation on first use per session.
- **Interfaces:** control-protocol commands `slot_store{name}`, `slot_list`, `slot_recall{name, keep_engineer}`,
  `slot_rename`, `slot_delete`, `slot_commit`, plus events. WingMix.app gets a slot strip with buttons and hotkeys
  1–9, the reference name and distance-to-target per slot, the active slot highlighted, and a diff view of what
  differs between two slots.

## Acceptance criteria
- [ ] Store/recall tested on FakeSurface, SimSurface and wing-sim (mix-testsim): every param in the switching set
      reaches the slot's values in one batch, with latency reported
- [ ] Forbidden/locked params are never stored or written; engineer-moved params are handled as specified
- [ ] Model-switch ordering in recall; stale/incompatible slots refused
- [ ] Persistence round-trip; commit is undoable; resume tuning works
- [ ] Protocol documented; WingMix slot UI with screenshot; `make check` and `make app` pass

## Log
- 2026-10-31 coordinator: created at the user's request; revised the same day from A/B/X blind test to multi-slot
  reference states.

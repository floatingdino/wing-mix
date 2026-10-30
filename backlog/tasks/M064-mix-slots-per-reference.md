---
id: M064
title: Mix slots: store console states per reference and switch instantly (A/B/C/D…)
stage: S5
status: done
owner: agent-M064
depends: [M062]
branch: task/M064
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
- [x] Store/recall tested on FakeSurface, SimSurface and wing-sim (mix-testsim): every param in the switching set
      reaches the slot's values in one batch, with latency reported
- [x] Forbidden/locked params are never stored or written; engineer-moved params are handled as specified
- [x] Model-switch ordering in recall; stale/incompatible slots refused
- [x] Persistence round-trip; commit is undoable; resume tuning works
- [x] Protocol documented; WingMix slot UI with screenshot; `make check` and `make app` pass

## Log
- 2026-10-31 coordinator: created at the user's request; revised the same day from A/B/X blind test to multi-slot
  reference states.
- 2026-10-31 agent-M064: claimed; reading actuator/surface/daemon/app code.
- 2026-10-31 agent-M064: actuator snap path (SnapRequest/Report, Snapped/Committed events, check_recall in SafetyGate: holds/ceiling/session envelope kept, per-move + quiet-only discrete relaxed), Surface::write_batch (WingSurface: one wire write + one barrier), commit; tests mix-actuator/tests/snap.rs. Next: slot store/persistence + daemon commands.
- 2026-10-31 agent-M064: done, status -> review.
  - **Actuator** (mix-actuator): `SnapRequest`/`SnapReport`, `Actuator::snap` (+ runner `ActuatorHandle::snap`): the one
    audited non-ramped path, used only for slot recall. Runs at once (running move ends `AbortReason::Superseded`,
    queue dropped), refused while STOPped/disconnected; one `Surface::write_batch` per phase; M038 ordering (model
    switch first, the switched block's params after `ParamsRemapped`/`remap_wait_s`, skipped if someone switched the
    model again; a refused switch keeps its block's params out); every write via new `RemitGate::check_recall`.
    Write log + journal entry `snap: <label>` (audit, not an undo unit); session baseline captured as usual (PANIC
    covers snaps). `Actuator::commit` (+ handle): comparison end state as one journal batch + undo unit. Events
    `snapped {report}`, `committed`, `JobKind::Snap`. `ActuatorConfig::max_snap` (4096, serde default).
  - **Safety**: `SafetyGate::check_recall` keeps remit, holds, fader ceiling, per-session limit, envelopes; drops the
    per-move limit and the quiet-only rule for discrete changes (GUI warns "may click"). docs/safety.md.
  - **Surface**: `Surface::write_batch` (default: sequential); `WingSurface` overrides it: one wire write with one
    barrier (`NativeClient::send_values`). `mix_surface::slots`: `managed_params` (remit-not-forbidden writable params
    of exposed non-matrix/DCA strips; monitor/locked/unknown roles excluded), `capture`, `check_compatible` (strip
    gone, class/unit changed, value invalid; leaves of a block whose model the slot switches are checked after the switch).
  - **Daemon** (apps/wing-mixd `slots.rs` + daemon.rs): `SlotStore` (`<state>/slots/<console>+<scene>.json`,
    version 1, newer refused), `EngineerMoves` (External param events per run + acknowledgements), switching set/diff.
    Commands `slot_store/list/recall/rename/delete/commit/resume/diff` (additive, backward compatible), events
    `slot_*`, `slots` in status + snapshot. `Original` captured when a session opens; auto slot on
    `holding{converged}` in autopilot. Recall: compatibility check, live confirmation once per connection on a real
    console (`confirm_live`), opens the session if needed, keeps engineer moves by default, comparison = observe mode
    (undo/start/autopilot refused), slot's target (+ its live context) becomes active and the controller is rebuilt
    on it (distance follows the slot). `start` refactored into `open_session` + `start_tail`.
  - **Control seam / M067**: no change in crates/mix-engine at all: pausing = observe mode, active target = the
    daemon's existing target rebuild. Nothing for M067 to merge against.
  - **App**: `SlotStripView` on Live (buttons with hotkeys 1-9, reference + distance + differs + "moved by you",
    active highlighted, Store/Commit/Resume tuning, Keep my moves, live-console alert, rename/delete context menu,
    diff A vs B/console), WingMixCore `Slots.swift`; screenshots docs/user/live-slots.png, live-slots-diff.png
    (`WINGMIX_SLOTS_SHOT=1`, docs/user/README.md).
  - **Latency** (wing-sim via mix-testsim, release): 9 params ~30 ms request->reply (write_batch < 0.1 ms; most is
    the compatibility check over ~3000 stored params); 20 params incl. an EQ model switch ~63 ms (actuator 33 ms,
    mostly the remap wait); last value echoed to a 2nd client within 1 ms of the reply. Embedded sim ~16 ms. Dev
    build ~230 ms.
  - Tests: mix-actuator/tests/snap.rs (FakeSurface: one batch, supersede, model ordering, refused model, forbidden,
    safety envelope/holds/no per-move limit, discrete allowed, stopped refusal, commit undo); mix-surface slots unit
    tests; wing-mixd slots.rs unit tests (persistence round trip, names, newer version, switching set, engineer moves);
    wing-mixd tests/slots.rs (embedded sim end to end incl. commit+undo, forbidden skipped, incompatible refused;
    wing-sim: live confirmation, engineer moves kept/overwritten, model first, latency); Swift SlotTests + slot flow
    in the real-daemon test. `make check` and `make app` pass (main merged).
  - Proposed follow-ups: (1) cache the compatibility check per topology/remap generation (recall would drop to a few
    ms); (2) recall refuses big boosts past the session's +6 dB limit when the engineer cut a lot since a slot: maybe
    offer "re-base the session" explicitly; (3) the controller's move log restarts on each recall (target rebuild):
    keep the curve per slot; (4) per-param keep/apply choice in the GUI (protocol has the bool only).
- 2026-10-31 coordinator: merged; make check + make app pass; CI green on branch. done. Follow-ups listed in log (recall boost limit vs re-base, per-param keep in GUI).

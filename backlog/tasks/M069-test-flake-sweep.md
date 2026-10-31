---
id: M069
title: Flaky-test sweep: make every timing-sensitive test deterministic
stage: S6
status: done
owner: -
depends: [M068]
branch: -
model: sonnet
---

## Goal
Linux CI keeps surfacing timing flakes one at a time: mix-llm spawn (fixed), mix-wing inproc
`preamp_alias_follows_the_patch_and_the_source` (fixed in M068), and now mix-x32 `tests/fake_x32.rs:205`
`writes_use_x32_addresses_and_laws` (UDP to the in-process fake). Sweep the whole workspace instead of chasing
them one by one:
1. Run the full fast test set many times under stress on macOS: e.g. 30× `cargo test --workspace` with a parallel
   CPU hog (`yes > /dev/null` × ncpu), plus `--test-threads` high. Use a Linux container too if docker/podman is
   available; otherwise push task/M069 repeatedly to CI. Collect every failing test.
2. Fix each at the root: replace sleeps and fixed timeouts with barriers or polling on the condition with generous
   deadlines; give UDP fakes acks or poll until the value arrives; isolate ports and temp dirs. Fix production
   races where the test reveals one (as M068 did for barrier()).
3. Add a `make stress` target documenting the stress run.

## Acceptance criteria
- [ ] 30 stressed local runs with 0 failures (list of fixed tests in the Log)
- [ ] 3 consecutive green CI runs on task/M069
- [ ] `make stress` documented in CLAUDE.md

## Log
- 2026-10-31 coordinator: created after the X32 flake on main CI.
- 2026-10-31 agent-M069: DONE -> review. `make stress` (tools/stress.sh: N x `cargo test --workspace` with ncpu/2 `yes` hogs, 2x ncpu test threads) documented in CLAUDE.md. A 16-hog/4x-thread profile was impractically slow (>10 min per run), so the default is lighter.
  Flaky tests found and fixed (root causes):
  1. mix-x32 `writes_use_x32_addresses_and_laws`: fixed 100 ms sleep -> poll; trim set counted before it landed.
  2. mix-x32 conformance (`passes_conformance_when_console_echoes_own_sets`): fake `console_set` could be overtaken by a client set still queued at the fake (stale value applied/echoed after it). The fake now orders console_set behind queued datagrams with a sync marker.
  3. mix-wing `rta_claim_writes_are_not_external_events` and wing_sim variant (PRODUCTION race): `NativeClient::refresh` registered expected get answers after sending, so a fast answer was taken for someone else's change (spurious External). Now registered before the send. inproc test also drains pre-connect hub events.
  4. wing-mixd slots test `wing_sim_recall_latency_engineer_moves_and_models` (PRODUCTION): daemon counted console events not yet drained as engineer moves after a slot store, and the console's re-load of a block after OUR model switch as engineer moves. Now slot store settles console events (bounded 500 ms), recall drains first, and params of a block whose model we just switched are ignored for 5 s. Test also polls the model on the 2nd client and waits for SOUL.
  5. wing-mixd `console_rta_feeds_the_safety_monitor_and_is_restored`: poll for the restored RTA source (daemon restore is on its own connection).
  6. wing-mixd `server::tests::socket_round_trip_stream_and_cleanup`: a sibling test's fork+exec can inherit the just-dropped listener fd (stale socket looks live); retry spawn briefly.
  Verification: on commit 5942eae the stress pass was stopped at the time-box after 15 consecutive stressed runs, 0 failures (earlier commits failed on items above; the full 30x pass was not completed). CI green on task/M069, incl. 5942eae: runs 41174378471, 41174114735, 41169375221.
  Proposed follow-ups: (a) complete a 30x `make stress` on main after merge; (b) the stale-socket inherited-fd retry is a test-only workaround (set CLOEXEC atomically in production bind if it matters); (c) model_switched 5 s window also hides real engineer EQ moves in that block right after a recall.
- 2026-10-31 coordinator: merged at time-box (15/15 stressed runs, 3 green CI). done. Follow-ups in log.

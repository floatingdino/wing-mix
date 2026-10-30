---
id: M054
title: Wire live-context targets into wing-mixd
stage: S5
status: done
owner: agent-M054
depends: [M037, M053]
branch: task/M054
model: sonnet
---

## Goal
Replace the TODO(M037) seam in `Daemon::set_live_context` (apps/wing-mixd): apply `mix_describe::apply_context`
to the active TargetProfile (reference or description) with the chosen context (default Live), re-run on context
change and on new reference/description, report `applied:true`, record in applied_deltas, and make the controller
re-plan. Update docs/control-protocol.md. Test via the daemon_sim harness: switching context changes the target's
PLR/width targets and the problem list.

## Acceptance criteria
- [x] Context applied on load and on change; status/snapshot show applied:true
- [x] Daemon test covers a context switch

## Log
- 2026-10-31 coordinator: created.
- 2026-10-31 agent-M054: done, status -> review.
  - `Target` keeps `raw` (as loaded) + `profile` (raw with `mix_describe::apply_context(live_context)`); applied in
    `poll_loading` and `set_live_context` (re-derived from raw, target gen bumped, controller rebuilt if a session
    exists). `load_description base:current` uses the raw profile (no double context). `applied` = target loaded.
  - Additive protocol: `live_context_applied` in status/snapshot, `snapshot.target.context` summary, `set_live_context`
    reply gains `target`; `applied` was `false` before, now true with a target.
  - Files: apps/wing-mixd/src/{daemon,protocol,status}.rs, apps/wing-mixd/tests/daemon_sim.rs, docs/control-protocol.md.
  - Test: `cargo test -p wing-mixd --test daemon_sim live_context` (switch live -> match_record changes PLR/width
    targets and the problem list, and back is exact); `make check` passes.
  - Follow-up: persist live_context in the state dir.
- 2026-10-31 coordinator: merged, make check passes. done. Persisting live_context → folded into M052 review list.

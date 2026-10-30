---
id: M054
title: Wire live-context targets into wing-mixd
stage: S5
status: todo
owner: -
depends: [M037, M053]
branch: -
model: sonnet
---

## Goal
Replace the TODO(M037) seam in `Daemon::set_live_context` (apps/wing-mixd): apply `mix_describe::apply_context`
to the active TargetProfile (reference or description) with the chosen context (default Live), re-run on context
change and on new reference/description, report `applied:true`, record in applied_deltas, and make the controller
re-plan. Update docs/control-protocol.md. Test via the daemon_sim harness: switching context changes the target's
PLR/width targets and the problem list.

## Acceptance criteria
- [ ] Context applied on load and on change; status/snapshot show applied:true
- [ ] Daemon test covers a context switch

## Log
- 2026-10-31 coordinator: created.

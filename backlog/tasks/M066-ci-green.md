---
id: M066
title: Make CI green on macOS and Linux
stage: S6
status: todo
owner: -
depends: []
branch: -
model: sonnet
---

## Goal
GitHub Actions CI (`.github/workflows/ci.yml`, repo floatingdino/wing-mix) has failed on 55 consecutive pushes
since M021. Known causes:
1. **Linux clippy:** `alsa-sys` build script panics because the runner lacks ALSA dev headers (cpal). Install
   `libasound2-dev` and `pkg-config` in the workflow, or gate cpal behind cfg so Linux builds without it, and check
   for other system deps (e.g. blake3 asm is fine on the runner).
2. **macOS make check:** `mix-wing` `tests/inproc.rs:276` `preamp_alias_follows_the_patch_and_the_source` is
   flaky under load (also seen locally). Fix the race (barrier or poll instead of timing), as M019b did for
   wing_sim tests.
3. Check the last ~10 failed runs (`gh run view <id> --log-failed`) for any other failing step or test and fix
   them all. Tests needing the wing-simulator binary must skip cleanly on CI (it isn't built there). Keep CI
   time reasonable (cache cargo).
Verify by pushing the task branch and watching its CI run (`gh run watch` / `gh run list --branch task/M066`).
Pushing the task branch to origin is allowed for this task only; do not push main.

## Acceptance criteria
- [ ] A CI run on task/M066 is green on both jobs
- [ ] The flaky test passes 20× locally under parallel load

## Log
- 2026-10-31 coordinator: created; CI red since M021 went unnoticed (coordinator now checks CI after each push).

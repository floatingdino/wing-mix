---
id: M071
title: Test wing-sim daemons must not outlive their test process
stage: S6
status: todo
owner: -
depends: [M070]
branch: -
model: sonnet
---

## Goal
The coordinator found 10 orphaned private `wing-sim --name WMTEST` daemons (ppid 1, 2–5 h old) left by e2e and
test runs, each holding shm and some CPU. mix-testsim's `Drop` cleanup doesn't run when the test process is
killed, times out, or panics in release (`panic = "abort"`). Make cleanup robust:
- the child must exit when its parent dies, e.g. a stdin pipe the daemon watches (EOF → exit). This needs a
  `--exit-on-stdin-eof` flag or a supervisor shim; if wing-sim needs a flag, prefer a small wrapper in mix-testsim
  over changing wing-simulator, and log the decision;
- spawn in its own process group, kill the group on Drop;
- a startup sweep in mix-testsim that kills stale `WMTEST` daemons whose recorded parent pid is gone (recorded in
  their temp state dir), and unlinks their shm.
Never touch the user's own wing-sim (no `--name WMTEST`, ports 2222/2223).

## Acceptance criteria
- [ ] Test: spawn a SimConsole in a child process, SIGKILL that process → daemon exits within 2 s
- [ ] Startup sweep test; `make check` + `tools/e2e/run.sh` leave no WMTEST processes behind

## Log
- 2026-10-31 coordinator: created after killing 10 orphaned test daemons.

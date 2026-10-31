---
id: M071
title: Test wing-sim daemons must not outlive their test process
stage: S6
status: review
owner: task-agent
depends: [M070]
branch: task/M071
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
- [x] Test: spawn a SimConsole in a child process, SIGKILL that process → daemon exits within 2 s
- [x] Startup sweep test; `make check` + `tools/e2e/run.sh` leave no WMTEST processes behind

## Log
- 2026-10-31 coordinator: created after killing 10 orphaned test daemons.
- 2026-10-31 task agent: Decision: wing-sim has no stdin-EOF flag and wing-simulator is read-only, so a supervisor
  shim lives in mix-testsim (no new binary, so other crates' dev-dep use needs nothing built): `/bin/sh -c` runs
  wing-sim in the background of a shell that is its own process-group leader (`process_group(0)`); a watcher
  `cat >/dev/null; kill -KILL daemon` on the piped stdin fires on EOF when the test process dies. Drop/kill()/restart
  SIGKILL the whole group. Non-unix: plain spawn (audio path is macOS-only anyway). `libc` added (cfg(unix)).
  State dir now records parent.pid, group.pid, shm.name; `sweep_stale()` runs once per process from SimDaemon::start.
  Pid-reuse guard: only kills a group whose leader command line contains WMTEST and the state-dir name.
  Files: crates/mix-testsim/{src/lib.rs,Cargo.toml}, docs/testing.md, Cargo.lock. Tests:
  `daemon_dies_with_killed_parent` (re-executes the test binary as helper, SIGKILL, group gone < 2 s),
  `sweep_reaps_stale_and_spares_live`. Results: `make check` rc=0, `tools/e2e/run.sh smoke` ok, no WMTEST processes
  or wingmix-test-* dirs left after either. Not run: `make stress`, Linux CI (push for CI if wanted).

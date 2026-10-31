# Testing against a live wing-sim

Three levels, fastest first:

1. **In-process** (`mix-sim` `SimSurface`, `mix-wing/tests/common` `InProc`): no processes, no sockets to the sim.
2. **Headless daemon** (`mix-testsim::SimDaemon`): the real `wing-sim` binary on private ports, no audio attached
   (meters are synthetic).
3. **Daemon + DriverSim** (`mix-testsim::SimConsole`): as 2, plus a private shared-memory ring clocked by
   `wing_shm::sim::DriverSim` that feeds multitrack audio into the USB inputs and captures USB outputs. Meters,
   gate/dyn GR and the RTA are then real, with no HAL driver installed.

## Requirements
The sister repo's release binary: `cargo build --release -p wing-sim` in `/Users/sam/code/wing-simulator`, or set
`WING_SIM_BIN`. Tests print `SKIPPED` and pass when it is missing (CI without the sister repo stays green).

## `mix-testsim` (dev-dependency only)
```rust
let c = SimConsole::start(Feed::sine(1, 1000.0, 0.5, 4.0), &[1]).expect("wing-sim");   // or Feed::synth(8.0)
let s = WingSurface::connect(c.addr(), &opts)?;
// USB in n only feeds a strip once patched; main fader defaults to -144 dB
s.write(&ParamId::new("input.1/source/grp"), Value::Enum("USB".into()))?;
s.write(&ParamId::new("input.1/source/in"), Value::Int(1))?;
let rx = s.subscribe_meters(&[StripId::new("input", 1)])?;     // real levels
let out: Vec<f32> = c.captured(1);                              // USB out lane 1 (after routing it, see live_audio.rs)
```
- `Feed::synth(secs)`: the 16-track `mix-sim` song (USB n = track n); `Feed::from_multitrack` takes any 48 kHz
  multitrack (e.g. a `mix-fixtures` excerpt); `Feed::sine` for level checks. Feeds loop.
- `SimDaemon::start()` / `start_with(&[extra args])`: daemon only (`mix-wing`'s `common::Daemon` wraps it).
- `ShmDriver`: the driver thread alone (paced 128-frame cycles at 48 kHz, read one block behind, write one ahead).
  Capture only records once the daemon is alive (before that DriverSim loops its output back).

## Isolation guarantees (the user's own wing-sim runs on 2222/2223)
- Ports: an even pair `p, p+1` in 20000..48000 that is free for TCP and UDP; discovery and OSC move with it.
- Temp state dir (`wingmix-test-<pid>-<n>`), `--no-autosave --no-hostio`; never the user's state dir.
- Private shm name `/wmx-<pid>-<n>` (macOS limit 31 chars), never `/wingsim.audio.v1`; asserted in code.
- Orphan safety (M071): the daemon runs as `sh -c` supervisor + `wing-sim` in its own process group; the supervisor's
  watcher exits on stdin EOF, so SIGKILL/abort/timeout of the test process kills the daemon within ~1 s. `Drop` kills
  the group. `mix_testsim::sweep_stale()` (once per test process, also callable) kills groups whose `parent.pid` in
  `wingmix-test-*` is dead (verified against the command line: `WMTEST` + that dir), unlinks `shm.name`, removes the dir.
- `Drop` kills the daemon, joins the driver thread, unlinks the shm and removes the state dir.
- Tests are real-time paced (audio runs at wall-clock speed); keep them to a few seconds, `#[ignore = "slow: ..."]`
  beyond ~10 s with a shorter variant.

## Examples
`crates/mix-wing/tests/live_audio.rs`: meters follow a USB sine, the synthetic song drives 12+ meters, USB out
capture sees ch1 -> main -> USB out 1.

## End to end (M052)
`apps/wing-mixd/tests/e2e/` drives the `wing-mixd` binary through its socket against a `SimConsole` (WING path) and
the embedded sim; `tools/e2e/run.sh` runs the slow variants and writes `target/e2e/report.md`. Building blocks:
`bridge.rs` (drains `SimConsole::take_captured()` and serves the USB outs to the daemon's `stream` input),
`observer.rs` (second native client: change log with timestamps, solo log, engineer moves), `audit.rs` (state
diffs, remit re-evaluation, ramp speed, wing-sim `save` snapshot diffs via its control socket). Strip taps on the
WING path use user signals (`source.usr.k/user.{grp,in,tap,lr}` = CH n, POST, L+R) patched to USB 5..: with only
main + solo the planner has nothing to work with. See docs/milestones/e2e.md.

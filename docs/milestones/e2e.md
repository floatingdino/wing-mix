# Milestone M052: end to end against wing-simulator

The product run as a user would run it: the `wing-mixd` binary, driven only through its control socket (as
WingMix.app and `wing-mix-cli ctl` do), steering a console towards a reference recording or a text description,
first in suggest mode, then on autopilot, with an independent audit of everything it did to the console.

Measured on main `77aabc6` (after M061, before M039) + task/M052 `0385909`/`637ff4d`, 2026-10-30, release build,
wing-sim binary `../wing-simulator/target/release/wing-sim`. These numbers are the baseline for M039.

## How to run

```sh
tools/e2e/run.sh            # everything, ~35 min (the wing-sim path runs in real time); report: target/e2e/report.md
tools/e2e/run.sh smoke      # fast WING-path check (also in `make check`: e2e_wing_smoke, ~8 s)
tools/e2e/run.sh sim        # (a) embedded sim only, ~1 min
tools/e2e/run.sh wing       # (b) wing-sim + real multitrack only
E2E_SONGS=holy-forever E2E_MODES=reference E2E_BATCHES=12 tools/e2e/run.sh wing
```

Needs: the sister repo's wing-sim release binary (`WING_SIM_BIN`), the local fixtures (`WINGMIX_FIXTURES`, the
multitrack + `mixes/*.m4a`, never committed) and `../wing-simulator/fixtures/PUBLIC.snap` (`WINGMIX_PUBLIC_SNAP`).
Missing pieces make the tests skip. Isolation: wing-sim on a private even port pair in 20000..48000 (never
2222/2223), private shm name, temp state dirs for wing-sim and wing-mixd; the user's WingSimulator app, state and
the default shm segment are never touched. Code: `apps/wing-mixd/tests/e2e/` (`main.rs` = tests + report).

## What runs

**(a) Embedded sim** (`sim.rs`): `connect_console sim` with mix-sim's synthetic 16-track song (seed 7) through the
degraded scene (known faults), unpaced; reference = the reference-scene render (WAV), description =
"warm, vocal-forward, punchy drums". Full parameter state read through the daemon's `params` command (ids from an
in-process twin of the same console) before / after suggest, after autopilot and after PANIC.

**(b) The real WING code path** (`wing.rs`): a headless wing-sim daemon (mix-testsim `SimConsole`) loaded with a
scene **derived from PUBLIC.snap** (a real church WING: buses 8-15 named `VOX 1 IEM` .. `BASS IEM`, `COMMS` 16,
matrices FOH / FOH LINK / STREAM LINK, main 4 STREAM, DCAs, FX buses), fed by DriverSim with real multitrack
excerpts. Per song, the best-covered played section passing the alignment check (as in mix-engine's
control_real), 16 s, looped; reference = that section of the commercial record (`load_reference` m4a + section),
description = the text above on the `worship` genre base; live context on. A soundcheck through a second native
client patches each multitrack track to its channel (USB n), names the strips, flattens their processing and sets
the **rough start** (mix-sim `rough_scene`: trims from the project, faders -10 dB). The console's USB outs are
patched like an engineer would for the tool: 1/2 main 1, 3/4 monitor A (= solo bus), 5.. one **user signal per
strip** (CH n, POST, L+R; a WING has 24). `bridge.rs` streams the captured USB outs to the daemon's new
`stream` input. `wing-mixd connect_console wing` talks to the console over the native protocol, exactly as to a
real WING.

Sequence per run: record the rough start from the console output -> daemon connect, roles, input, target ->
suggest (until a plan) -> autopilot (operator confirms conditional moves) with drills -> hold (converged /
no progress / budget of 12 batches) -> record the result from the console output -> ceiling drill -> PANIC ->
audits. Drills (reference runs): the engineer grabs the fader the tool is ramping (second native client), STOP
mid-move then `resume`, a 2.5 kHz tone on a spare channel building at 6 dB/s right after a tool move (feedback),
the tone + main fader slammed up 10 dB (mains ceiling).

## Results

Distance = main-bus distance of mix-engine's diagnosis to the target (the control_real measure), computed offline
from 16 s of the console's actual main output before and after autopilot; FOH = the engineer's recorded FOH mix
of the same excerpt on the same scale. Daemon objective = the controller's own score (first -> last window).

| run | target | distance start -> end | FOH | daemon objective | batches (kept) | hold |
|---|---|---|---|---|---|---|
| sim | reference render | 1.015 -> 0.822 (-19 %) | - | 2.14 -> 1.62 | 9 (7) | no progress |
| sim | description | 5.461 -> 5.424 (-1 %) | - | 6.68 -> 6.24 | 9 (7) | no progress |
| I Know That I Know §9 | record | 2.616 -> 1.346 (-49 %) | 0.859 | 6.11 -> 4.84 | 12 (7) | budget |
| I Know That I Know §9 | description | 1.330 -> 1.099 (-17 %) | 0.946 | 5.07 -> 4.74 | 5 (3) | no progress |
| Holy Forever §7 | record | 2.510 -> 1.009 (-60 %) * | 1.031 | 6.09 -> 4.41 | 12 (6) | budget |
| Holy Forever §7 | description | 1.331 -> 0.964 (-28 %) | 0.870 | 5.19 -> 4.66 | 5 (3) | no progress |
| Show Me Your Face §4 | record | 2.251 -> 1.948 (-13 %) | 1.403 | 4.18 -> 3.78 | 1 (1) | no progress |
| Show Me Your Face §4 | description | 2.268 -> 2.163 (-5 %) | 2.100 | 3.56 -> 4.13 | 2 (1) | no progress |

\* rerun after a harness fix (see gap 4); the first full run held 800 s in a latched false feedback trip and
reached 1.071 in 5 batches. Every run reduced the distance except the synthetic description run (flat). One run
of six beats the engineer's FOH distance (Holy Forever, record). Real-time cost: 1.5-6.5 min per WING run.

Audits (all runs, all passing; asserted in `check()`):

- **Suggest writes nothing**: 0 param writes seen by the second client (or in the full-state diff) while
  suggesting, in every run (4-7 moves suggested).
- **Zero forbidden writes**: every param a run changed (full-state diff through the generic surface + every
  change the observer saw) re-evaluated by an independent `PolicyGate` with the same policy on the observer's
  connection: 1-16 params per run, all `allowed` or `conditional` with a confirmation the operator gave (2 on Holy
  Forever); **0 forbidden**. All changes were on input strips with role foh; nothing on the
  13 protected strips (IEM / comms / unconfirmed buses), matrices, mains 2-4, routing or the monitor section. The
  wing-sim snapshot (`save` over its control socket) before the session vs after PANIC: **0 differing paths**
  (RTA source restored too), apart from the harness's own engineer writes.
- **Solo restored exactly**: sim description: 2 inspections, 2 exact restores. WING runs: no inspections were
  planned (see gap 3); solo state at the end equals the start in every run.
- **Ramps smooth** (as the second client, a WING-Edit stand-in, sees them): 39-383 writes per run on 1-11
  continuous params; max speed over any >= 0.2 s stretch 1.10-2.03 x the per-kind peak rate (faders 6 dB/s, pans
  40/s): the 2 x stretches are reverts/undos, which run at `restore_speed` = 2 by design; nothing above the
  asserted 2.5 x. Max step per observed write 1.25-2.5 x rate per 40 ms tick (a late tick writes a bigger step at
  the same rate).
- **Engineer has priority**: the engineer's write to a fader mid-ramp produced `yielded` + `engineer_moved`, the
  tool left it alone for the whole cool-down (it may plan it again afterwards, and did once).
- **STOP**: 0 tool writes in the 2.2 s after STOP mid-move; `resume` continued the loop.
- **Feedback guard**: the building 2.5 kHz tone tripped it in 2.9-3.9 s (prominence ~12 dB, rise ~11 dB) and
  the tool's last move was reverted (`reverted: <move>`), on I Know That I Know and Holy Forever.
- **Mains ceiling**: tone + main +10 dB tripped `mains_loudness` (momentary -3.5 LUFS) or `mains_true_peak`
  (+0.4..+1.8 dBTP), `ceiling_hold` true, in all three reference runs.
- **PANIC**: every touched param back at its session baseline in every run (0 residual); also in the sim.

## Bugs found and fixed here

1. **Actuator stall (mix-actuator)**: a ramp with more params than `write_burst` (10) waited for more tokens than
   the bucket can hold, so a PANIC after a session that touched > 10 params never finished (the sim reference run
   touched 13). Fixed in `actuator.rs` (`step_ramp`: wait for a full bucket, then borrow; the average write rate
   still holds); regression test `tests/safety.rs::panic_over_more_params_than_the_write_burst_completes`.
2. **No way to feed the daemon a non-device console output**: new `select_input {"source":"stream"}` (raw f32 over
   TCP; docs/control-protocol.md). Minimal daemon change (source.rs + one match arm), done in parallel with M061.

## Remaining gaps and findings

1. **Main + solo taps alone give no plan.** With only USB main and monitor A (the obvious WING setup), the
   planner returns `exhausted` every window: it needs per-strip taps. On a WING that means user signals -> USB
   (24 max, so > 24-input shows lose per-strip diagnosis). Document in docs/user, and consider a solo-sweep that
   measures strips through the solo bus instead.
2. **The daemon's own RTA claim is reported as an engineer move** (`engineer_moved system/cfg/rta/$src|$tap` at
   session start; the first plan has reason `engineer_moved`). Harmless now (system params), but it pollutes the
   event log and would start a cool-down if the RTA were ever a planned param.
3. **No solo inspections on the WING path** in 6 runs (2 in the sim description run): solo restore is verified
   by daemon events in the sim and by "solo state unchanged" on WING; a run that forces inspections (e.g. a gate /
   de-esser fault) is needed to watch restores with the second client.
4. **False feedback trip on music right after `resume`**: on Holy Forever, 2 s after resuming from the drill's
   genuine 2.5 kHz trip, the guard tripped again at 100 Hz (prominence 13 dB, rise 9.7 dB) on the music alone
   and the session stayed latched. Likely the history cleared at `resume` plus the tone's disappearance; tune in
   M039/M041 follow-up. The harness now acknowledges any trip and resumes (as an operator would) and reports it.
5. **Feedback guard silent on a quiet mix**: on Show Me Your Face (mix at -40 LUFS) the same tone building to
   +6 dB fader never tripped. Check the absolute floor (`floor_db` -70) / relative floor against quiet programmes.
6. **PANIC overrides an engineer move the tool later re-planned**: after the cool-down the tool moved the
   engineer's fader again, and PANIC then returned it to the session baseline (the value before the engineer's
   move), not the engineer's value. Consider re-basing a param's PANIC target when the engineer moves it.
7. **Convergence** (M039's job): budget-limited on 2 songs (12 batches, ~50 % kept), `no_progress` after 1-2
   batches on Show Me Your Face (quiet, short section); 1 of 6 runs at or below FOH. The synthetic description run
   is flat (the default base is far from any synthetic mix). Description targets start nearer (1.3 vs 2.5) because
   the `worship` base is an average of the three records.
8. **Not covered here**: link loss (covered by mix-wing / mix-actuator tests, not end to end), the stall
   watchdog, WingMix.app (its own `make app` smoke + Swift tests drive the real daemon; not re-run here), real
   USB audio clocking/drift, and anything only a real WING and room can show (next section).

## Proposed follow-ups

- (gap 1) user docs: the WING USB-out patch for the tool (main, monitor A, user signals); a solo-sweep strip
  measurement for consoles/shows without per-strip taps.
- (gap 2) mix-wing/daemon: tag the RTA claim's own writes as local so the controller ignores them.
- (gaps 4, 5) safety: feedback detector after `resume` and on quiet programmes; add both as cases to
  mix-engine's `feedback_music` test.
- (gap 6) actuator: PANIC target for params the engineer moved during the session.
- (gap 3) e2e: a WING-path scene with a gate/de-esser fault so inspections run under the observer.
- e2e: split ramp-speed stats by job kind (move vs undo/revert/PANIC) so moves are held to 1 x and restores to 2 x.
- e2e: run `tools/e2e/run.sh` after M039 and compare with the table above.

## Hardware verification on a real WING (manual; HUMAN-ACTIONS item 6)

What the simulator cannot prove: the real console's network timing, its USB audio interface and clocking, real
room acoustics (feedback guard), firmware differences in the parameter tree, and how WING-Edit / the console surface
show the moves. Run this once a WING is available. Nothing here needs code changes; record results in a dated
section at the end of this file.

**Safety first**: do it at a soundcheck or with the PA muted / at low level, with someone at the console. STOP is
`Cmd-.` in WingMix.app or `wing-mix-cli ctl STOP`; PANIC is `wing-mix-cli ctl PANIC`. Save a scene/snapshot on the
console before starting (it is the ground truth for step 9).

1. **Network + identity.** Mac and WING on the same network. `wing-mix-cli ctl '{"cmd":"list_consoles"}'` lists the
   WING (name, model, firmware). Note the firmware.
2. **USB audio + taps.** Connect the WING by USB. On the console route: USB out 1/2 = MAIN 1, 3/4 = MON A (solo
   bus), USB out 5.. = user signals 1..N (`Routing > User signals`: CH n, POST, L+R) for up to 24 key inputs.
   `wing-mix-cli ctl '{"cmd":"list_devices"}'` shows the WING device with >= 4 + N channels at 48 kHz. Grant the
   microphone permission when macOS asks (HUMAN-ACTIONS item 5).
3. **Connect + input.** `{"cmd":"connect_console","console":"wing","ip":"<ip>","scene":"<show>"}`, then
   `select_input` `device` WING with taps main [1,2], solo [3,4], `input.N` [5..]. `status.input.dropped_frames`
   stays 0 for a minute; `snapshot.loudness` follows the program.
4. **Roles.** `list_roles`: every IEM/wedge/comms bus is `monitor`/`comms` (or `unknown`, i.e. protected) and every
   matrix is locked. Confirm only what you are sure of. Expected: same as the PUBLIC.snap scene in the sim run
   (13 protected strips there).
5. **Suggest.** Load a reference (`load_reference` a record of the song) or a description; `set_mode suggest`,
   `start`. A plan appears in WingMix.app within one window; WING-Edit and the console show **no change at all**
   (watch the faders of the inputs in the plan for a full minute).
6. **Autopilot, watched in WING-Edit.** `set_mode autopilot`. In WING-Edit (or on the surface) every move is a
   smooth glide (faders at <= 6 dB/s, EQ gains <= 3 dB/s, pans <= 40/s; no jumps), solos flash only on the
   inspected strip during an inspection and return exactly to what they were (try with one engineer solo engaged
   beforehand), and nothing on any IEM bus, matrix, main 2-4, routing or the monitor section moves. The console RTA
   source changes while a session runs and returns to your setting at `stop`/`quit`.
7. **Engineer has priority.** While a tool move is running, grab that fader on the surface: the tool stops moving
   it immediately (`yielded` in the event log) and does not touch it again for the cool-down.
8. **STOP / PANIC.** During a move press STOP: motion stops within one tick (nothing moves afterwards in WING-Edit);
   `resume` continues. Then PANIC: every param the session touched glides back to where it was (except what you
   moved yourself).
9. **State audit.** After `quit`, save a new scene on the console and compare it with the one from the start (WING-Edit
   compare, or export both and diff with `wing-diff`): only the params listed in `snapshot.touched` differ (and none
   after a PANIC).
10. **Safety in a room.** At low PA level, with the tool in autopilot, slowly raise a vocal mic's gain until it just
    starts to ring: the feedback guard trips (STOP + revert of the last move, `safety` event) before you would have
    reached for the fader. Then raise the main fader quickly by 10 dB: the mains hold engages (`ceiling_hold`), no
    boosts are made until it drops. Record the trip times and levels (thresholds are not yet tuned on real rooms).
11. **Link loss.** Pull the network cable for 10 s during a move: the move stops (`link_lost`), nothing is replayed
    when it comes back (`link_restored`), anything you changed meanwhile is left alone.
12. **Result.** Note the distance curve (`snapshot.curve`) and whether the mix moved towards the target by ear.

If any step fails: STOP/PANIC, save `<state-dir>/events.jsonl` and `status.json`, and file it against M052's
follow-ups.

## Follow-up M063

Fixed: gap 2 (RTA claim echoes are Local), gap 4 (post-resume warm-up), gap 5 (absolute band floor -70 -> -90 dB, relative floor only), gap 6 (PANIC re-bases to the engineer's value). See docs/safety.md. Regression tests: `mix-wing` `wing_sim_rta_claim_is_not_an_engineer_move`, `mix-actuator` `panic_returns_a_re_moved_param_to_the_engineers_value`, `no_feedback_verdict_on_the_stub_of_history_after_resume`, `building_sine_in_a_quiet_mix_is_caught_as_early_as_in_a_louder_one`. Re-run `tools/e2e/run.sh` to confirm end to end.

## M062: main + solo only (one stereo stream), 2026-10-31

`E2E_TAPS=solo tools/e2e/run.sh wing` (task/M062): the same runs with only USB 1/2 = main 1 and 3/4 = monitor A
(solo bus) captured, no user signals. Before M062 the planner returned `exhausted` every window in this setup. Now
suggest plans from estimated strips (knowledge-base level + prototype spectrum scaled into the main: fader moves,
main EQ, description level intents through the faders) without writing or soloing, and autopilot measures the
strips through rate-limited solo sweeps between batches (`strip_measured` events, `snapshot.session.strips`).
Compared with M052's multichannel table above (same songs, sections, budget of 12 batches):

| run | multichannel (M052) | main + solo only (M062) | sweeps | solo restores (exact) |
|---|---|---|---|---|
| I Know That I Know §9, record | 2.616 -> 1.346 (-49 %) | 2.615 -> 1.640 (-37 %) | 30 | 37 (37) |
| I Know That I Know §9, description | 1.330 -> 1.099 (-17 %) | 1.334 -> 1.042 (-22 %) | 28 | 35 (35) |
| Holy Forever §7, record | 2.510 -> 1.009 (-60 %) | 2.510 -> 1.471 (-41 %) | 49 | 62 (62) |
| Holy Forever §7, description | 1.331 -> 0.964 (-28 %) | 1.331 -> 0.991 (-26 %) | 40 | 50 (50) |
| Show Me Your Face §4, record | 2.251 -> 1.948 (-13 %) | 2.256 -> 2.021 (-10 %) | 31 | 42 (42) |
| Show Me Your Face §4, description | 2.268 -> 2.163 (-5 %) | 2.267 -> 2.150 (-5 %) | 40 | 45 (45) |

Every run non-empty in suggest (1-5 moves, 0 writes, no solo), every audit green (0 forbidden writes, solo restored
exactly by the actuator's read-back and as the observer saw it settle before the next solo, end state = start, STOP,
engineer yield, feedback/ceiling trips, PANIC 0 residual). Description runs match the multichannel ones; record runs
reach ~70-80 % of the multichannel reduction: the first windows plan on estimates only (mostly the main EQ), the
strips arrive one sweep at a time (~3 s dwell + 2 s gap, 8/min: 17 eligible strips in ~2.5 min) and each new
measurement re-plans, and the 12-batch / 120 dB budget ends every run before convergence. The daemon objective
rises during a run (2.5 -> 3.0) because every newly measured strip adds its strip/stem terms; the main-bus distance
is the comparable measure. Not re-measured here: the multichannel runs on this branch (M039 changed the planner
after M052's table).

## M067: change budget + meter-refined strip estimates, 2026-10-31

`tools/e2e/run.sh wing` with `E2E_TAPS=multi,solo` (and `main,priors` for consoles without solo), 3 songs in parallel
processes. **Before** = main `cd5c5c4` (12-batch cap, 120 dB travel budget) with one harness fix (below), **after** =
task/M067 (no batch cap: the controller's change budget, i.e. net deviation <= 60 dB weighted + 24 dB/min pacing).

Harness fix (in both columns): the operator now also confirms the confirmation requests made during the suggest
phase. They were emitted before the autopilot event cursor and never confirmed, so the plan's conditional moves
(main EQ, compressors, pair pans) stayed pending and on main the *multichannel* runs held `no_progress` after 0-3
batches (unfixed main: I Know -42 %, Holy Forever -41 %, Show Me Your Face 0 %; Holy Forever description changed
nothing and failed its audit).

Reference targets (main-bus distance to the record, start -> end; batches (kept); hold):

| song | multichannel before | main + solo before | multichannel after | main + solo after | solo / multi after |
|---|---|---|---|---|---|
| I Know That I Know §9 | 2.615 -> 1.292 (-51 %) 12 (10) budget | 2.616 -> 1.602 (-39 %) 12 (6) budget | 2.615 -> 1.065 (-59 %) 18 (10) no_progress | 2.616 -> 1.394 (-47 %) 22 (12) no_progress | 80 % |
| Holy Forever §7 | 2.510 -> 0.991 (-61 %) 12 (4) budget | 2.510 -> 1.359 (-46 %) 12 (6) budget | 2.510 -> 0.964 (-62 %) 18 (8) no_progress | 2.510 -> 1.171 (-53 %) 25 (13) no_progress | 85 % |
| Show Me Your Face §4 | 2.257 -> 1.906 (-16 %) 11 (3) budget | 2.249 -> 2.319 (+3 %) 12 (4) budget | 2.251 -> 1.686 (-25 %) 14 (5) no_progress | 2.256 -> 1.499 (-34 %) 29 (8) no_progress | 136 % |

Main + solo reaches 92 % of the multichannel reduction summed over the three songs (before: 64 %); every run ends on
`no_progress`, none on the budget (net deviation used 11-26 of 60 weighted dB, total travel 15-38 dB). FOH: 0.859 /
1.031 / 1.403; Holy Forever multichannel beats FOH.

Description targets: Holy Forever -30 % / -29 % before (multi / solo) -> -32 % / -30 %; I Know -18 % / -21 % ->
-15 % / -18 %; Show Me Your Face -15 % / -6 % -> -38 % / -23 % (the solo run hit the 1800 s harness timeout still
improving). Before, 4 of 6 description runs ended on the budget.

Consoles without solo (`E2E_TAPS=main` vs `priors`, reference, main only, no solo/sweeps): the WING meters refine 14
of 17 strips (2 found silent and dropped), but on these drill-laden 3-4 minute runs the result is noise-dominated:
-3 / -4 / -2 % with meters vs -1 / -7 / -10 % with priors only. In mix-sim (seed 7, no drills, `control_solo
no_solo_meters_improve_over_priors_alone`) meters help clearly: 1.119 -> 0.814 vs 1.119 -> 1.056 with priors only.

Audits: all green except two findings in the Show Me Your Face process (the per-song test stops at its first failed
run, so its description runs were not asserted):
1. **PANIC does not restore main-bus EQ** (new exposure: before the harness fix, `main-processing` was never
   confirmed, so the main EQ was never moved): after PANIC the wing-sim snapshot differs in 8 / 3 `main/1/eq/*`
   paths on the two description runs; the actuator's `reverted` event for the PANIC lists only the faders, no
   conflicts. Undo of the same main EQ moves works. mix-actuator `panic_with_id` / mix-wing; follow-up.
2. **Engineer yield race** (Show Me Your Face reference, main + solo): the engineer's write landed during a
   0.8 s fader ramp; no `yielded`/`engineer_moved` was seen and the tool later reverted that batch inside the
   cool-down. Same class as the known flaky `mix-wing wing_sim_engineer_move_pauses_the_actuator_ramp`.

FakeX32 (`mix-x32 --test session`, main only, no solo, no meters): unchanged before/after: one fader move,
distance 2.106 -> 0.182, converged (net 3 dB).

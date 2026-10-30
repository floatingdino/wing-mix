# wing-mixd control protocol (M050)

`wing-mixd` is the Wing Mix daemon: it connects to a console (a WING over the native protocol, or the embedded
wing-sim), captures one input, analyses it, and runs the closed-loop controller and the actuator. Front ends (the
SwiftUI app M051, `wing-mix-cli`) talk to it through files in the **state dir**, following the conventions of
wing-simulator's `apps/wing-sim/src/control.rs` (status.json + control.sock), with JSON lines instead of `ok/err`
text.

Code: `apps/wing-mixd/src/{protocol,server,daemon,status,events}.rs`.

## State dir

Default `~/Library/Application Support/WingMix` (macOS; `$WINGMIX_STATE_DIR` overrides; Linux
`$XDG_STATE_HOME/wingmix` or `~/.local/state/wingmix`); `wing-mixd --state-dir DIR` for anything else (tests use a
temp dir). It is never the WingSimulator state dir.

| File | Content |
|---|---|
| `status.json` | [`DaemonStatus`](#status) JSON, rewritten atomically (temp file + rename) once a second and once more with `"running": false` on exit. Readers check `pid` and `updated_unix` for liveness. |
| `control.sock` | Unix stream socket (Windows: a loopback TCP address in the file). A stale socket is replaced at start; a live one makes the second daemon exit (`AddrInUse`). Removed on exit. Unix socket paths are limited to ~104 bytes, so keep `--state-dir` short. |
| `settings.json` | Persisted daemon settings: `{"live_context":"live"}` (M055); loaded at start, rewritten when `set_live_context` changes it. |
| `roles.json` | User-confirmed strip roles per console + scene (`{"consoles":{"sim\|default":{"bus.5":"monitor"}}}`). |
| `events.jsonl` | Every event line (below), appended; rotated to `events.1.jsonl` past 8 MB. The audit trail of moves. |
| `slots/<console key>+<scene>.json` | Mix slots (M064), `{"version":1,"console_key","scene","slots":[...]}` (see [Mix slots](#mix-slots)); a file with a newer version is refused, never overwritten. |
| `profiles/` | Reference profile cache (blake3 of the file + analysis versions). |
| `stems/` | Optional stem-separation sidecar (mix-stems; only used with `"stems": true`). |

## Wire format

One request per line, one reply line per request. A request is a JSON object with a `cmd` and optional `id` (any
JSON value, echoed in the reply):

```text
-> {"id":1,"cmd":"set_mode","mode":"suggest"}
<- {"ok":true,"id":1,"result":{"mode":"suggest"}}
-> {"cmd":"start","window_s":"x"}
<- {"ok":false,"error":"invalid type: string \"x\", expected f64"}
```

A bare word is shorthand for `{"cmd":word}`: `ping`, `status`, `snapshot`, `STOP`, `PANIC`, `quit`. Unknown fields
are rejected for commands with arguments. Connections stay open for any number of requests (idle timeout 60 s). A
request waits at most 60 s for the engine (loads are asynchronous, see `wait`).

`{"cmd":"subscribe"}` is acknowledged (`{"ok":true,"result":{"subscribed":true}}`) and then the connection carries
**event lines** until the client closes it; a `{"source":"daemon","type":"heartbeat"}` line is sent every 2 s when
nothing else happens.

## Commands

| `cmd` | Arguments | Result |
|---|---|---|
| `ping` | | `{"name":"wing-mixd","version","pid"}` |
| `status` | | the status document |
| `list_devices` | | `{"devices":[{"name","default","max_channels","min_sample_rate","max_sample_rate","default_sample_rate"}], "sim":{"taps":[..]}?, "error"?}` |
| `list_consoles` | `discover` (bool, default true: UDP broadcast `WING?` to port 2222), `wait_ms` (800), `probe` (`ip[:port]`, unicast), `local` (bool, default true: a wing-sim on this Mac, M065) | `{"consoles":[{"kind":"sim"},{"kind":"wing","ip","name","model","serial","firmware"}]}`. With `local`, a live wing-sim (its `status.json` in `$WINGSIM_STATE_DIR` / `~/Library/Application Support/WingSimulator`: `running`, updated < 15 s ago) is listed as `{"kind":"wing","ip":"127.0.0.1","port":2222,"name","model":"wing-sim","local":true,"scene"}`; connect it like a WING (`connect_console` `wing` + `ip` + `port`). Only the status file is read. |
| `connect_console` | `console`: `sim`, `wing` (+`ip`), `x32` (+`ip`, port default 10023; M061) or an IP (= wing); `port` (2222); `scene` (role key, default `default`); `sim`: see below | `{"kind","name","key","scene","inputs"}`. Replaces any previous console (ends its session; a sim input is dropped with it). The remit policy is the console's own default (`data/remit/<surface>-default.toml`) or `--policy`; a policy whose `surface` does not match the console is refused (the connect fails). `wing-mixd --console sim\|wing:<ip>[:port]\|x32:<ip>[:port]` connects at start. |
| `disconnect_console` | | |
| `select_input` | `source`: `device` / `file` / `sim` / `stream`; `device` (name or unique substring; default input), `path` (WAV, `file`), `addr` + `channels` (`stream`), `sample_rate`, `taps` | `{"kind","description","sample_rate","taps"}` |
| `load_reference` | `path` (audio file, or a TargetProfile `.json`), `section` (index), `stems` (bool), `wait` (bool) | `{"loading":true}` at once, or with `wait` the loaded target `{"kind":"reference","label","info"}` |
| `load_description` | `text`, `base` (`current` or a profile `.json`), `genre`, `llm` (bool), `model`, `wait` | as above, `info` lists the applied terms, conflicts, unrecognised phrases |
| `list_roles` | | `{"key","console":{"kind","name","scene"},"strips":[{"strip","name","detected","effective","confidence","evidence","confirmed","needs_confirmation","context"}]}`. `console` says which console and scene the roles reflect (M065); `context: true` marks routing-only strips the console does not expose for control (the embedded sim's aux inputs, matrices, channels beyond its tracks), which detection still uses. |
| `confirm_roles` | `roles` (`{"bus.5":"monitor","bus.6":null}`; roles: foh, monitor, comms, unrouted, foh_group, fx, aux_feed, unknown, main, matrix, dca, none; `null` clears the confirmation = back to detection), `scene` | `{"key","confirmed","cleared","previous","applies":"now"}`. One request is one **batch** (M065): validated as a whole (a bad id/role changes nothing), applied together. `previous` maps every strip of the batch to its former confirmation (`null` = none): sending it back as `roles` undoes the batch exactly. Applies immediately, also mid-session (see [Roles](#roles)); `applies` is always `now` (kept for compatibility, it used to be `next_session`). |
| `set_mode` | `mode`: `observe` / `suggest` / `autopilot` | |
| `set_live_context` | `context`: `live` (default; persisted in `settings.json` and restored at the next daemon start) / `broadcast_video` / `match_record` | `{"live_context","applied","target"}` (M037/M054). `mix_describe::apply_context` is applied to the loaded target (reference or description) on every load and on every change, always from the raw profile (switching is an exact revert); a running controller is rebuilt and re-plans. `applied` is `true` once a target is loaded (`false` before: the setting is stored and applied on the next load); `target` is the context summary below (null without a target). Announced as a `live_context_changed {live_context, applied}` daemon event on change. Effects are recorded in the target's `applied_deltas` as `context:<name>` (so problems list it in `terms`). |
| `start` | all optional: `mode`, `window_s`, `time_scale`, `max_batches`, `max_net_db`, `max_db_per_min`, `target_objective`, `inspect`, `feedback_guard`, `sweep`, `priors`, `meters` | `{"running":true,"mode"}`; needs a console, an input and a target |
| `stop` | | ends the loop: controller stops, actuator freezes (session kept: `undo`/`PANIC` still work; `start` resumes) |
| `STOP` (`emergency_stop`) | | freezes every move now; the loop pauses until `resume` |
| `PANIC` (`panic`) | | ramps every touched param back to its session baseline, then stays stopped; mode drops to `observe` |
| `resume` | | lifts a STOP / safety trip (clears the feedback latch) |
| `confirm` | `key` | gives a remit confirmation; pending moves needing it are planned again |
| `pending` | | `{"pending":[{"key","summary"}]}` |
| `undo` | | `{"job":id}`: ramps the last completed move back |
| `snapshot` | | live state, below |
| `strips` | | `{"strips":[{"id":"input.13","kind":"input","name":"VOX 1"}]}` for every named strip of the connected console (M055; the strip model has no colours). Front ends map the ids in `snapshot` (problem contributors, plan/move `strip`, param ids) to names. Errors without a console. |
| `params` | `ids` (`["input.13/fader"]`) | `{"input.13/fader":{"type":"float","value":-10.0}}` |
| `slot_store` | `name` (default: next free letter A, B, C...) | slot summary `{"name","builtin","auto","stored_unix","target":{kind,label,live_context}|null,"distance","objective","params","console"}`. Stores the managed console state + the active target; an existing name is overwritten (not `Original`). Needs a console only. |
| `slot_list` | | `{"slots":[summary + "differs","engineer_moved","active","compatible","refused"?],"active","comparing","switching_set","live_confirmation_needed","target","distance","objective","file"}` |
| `slot_recall` | `name`, `keep_engineer` (default true), `confirm_live` (bool) | `{"name","requested","written","changes":[{param,from,to}],"skipped":[Refusal],"kept_engineer":[param],"models":[param],"may_click","phases","prep_ms","write_ms","total_ms","request_ms","target","job"}`. See [Mix slots](#mix-slots). |
| `slot_commit` | `name` (default: the active slot; another name is recalled first) | `{"name","job","changes"}`: ends the comparison as one undoable batch (`undo` reverts it) |
| `slot_resume` | `mode` (default: the mode before the comparison; observe counts as autopilot) | `{"mode","slot","committed"}`: commits a running comparison, then continues the loop from the current slot and its target |
| `slot_rename` | `name`, `to` | |
| `slot_delete` | `name` | |
| `slot_diff` | `a`, `b` (a slot, or `current` = the console now; default) | `{"a","b","params":[{"param","a","b","engineer_moved"}]}` |
| `subscribe` | | turns the connection into an event stream |
| `quit` | | the daemon writes its final status and exits |

STOP and PANIC are executed by the connection thread directly on the actuator (its command channel wakes the
actuator thread immediately), so they never wait behind a plan being computed; the engine is told afterwards.

### Embedded sim (`connect_console` `sim`)

```json
{"cmd":"connect_console","console":"sim","sim":{
  "multitrack":"/path/16ch.wav",   // channel n -> USB in n -> input n; or "synthetic" (default: mix-sim's 16-track song)
  "names":["KICK","SNARE"],        // strip names (synthetic: the track names); used for source hints
  "seed":7,"seconds":8,            // synthetic song
  "scene":"degraded",              // factory (default) | reference | degraded (mix-sim scenes) | /path/x.snap (read only;
                                   //   the role-confirmation scene defaults to the file name, e.g. `sim|MyShow`)
  "speed":1                        // 1 = real time, N = N x, 0 = as fast as the analysis consumes
}}
```

The sim renders on its own thread; `select_input {"source":"sim"}` analyses its main 1, solo bus and every input
post fader. The multitrack loops.

### Stream input (M052)

`{"cmd":"select_input","source":"stream","addr":"127.0.0.1:40000","channels":27,"sample_rate":48000,"taps":[..]}`:
the daemon connects to `addr` (TCP) and reads raw interleaved little-endian f32 frames, `channels` per frame
(default: the highest tap channel). Live semantics as for a device: frames that do not fit the ring are dropped and
counted (`dropped_frames`); EOF ends the input (the stall watchdog then holds/STOPs). Used by the end-to-end harness
(docs/milestones/e2e.md) to feed a headless wing-sim's USB outputs; usable for any network audio bridge.

### Taps (`select_input`)

`taps` says which console signal each input channel carries (1-based channels, 1 or 2 per tap):

```json
{"cmd":"select_input","source":"device","device":"WING","taps":[
  {"tap":"main","channels":[1,2]},          // required: what the target is compared with
  {"tap":"solo","channels":[3,4]},          // solo bus: solo inspections measure here (no solo tap -> no inspections)
  {"tap":"input.1","channels":[5]},         // post-fader strip taps: per-strip diagnosis
  {"tap":"monitor:IEM 1","channels":[7,8]}  // monitor feeds: feedback guard only
]}
```

Default: `main` on channels 1-2. The sim input's taps are fixed.

### `start` options

Live defaults come from the knowledge base (`ControlConfig::from_kb`: settle 1.5 s, window 20 s, 40 batches...).
`time_scale` multiplies settle/window/cool-downs (`ControlConfig::scaled`), then `window_s` sets the window; tests
use one loop of the fixture per window. `inspect` is forced off without a `solo` tap. `feedback_guard` defaults to
on for a WING and off for the sim (synthetic loops false-trip it, M033). M062, strips without an `input.N` tap
(e.g. only `main` + `solo` taps): `priors` (default true) estimates them so mix-level problems still get fader
moves; `sweep` (default true, autopilot only, needs `inspect`) measures them one at a time through solo inspections
between batches (rate-limited, never while the engineer has something soloed or the section changes; solo restored
exactly). `snapshot.session.strips` reports the coverage (`eligible`, `tapped`, `solo_measured`, `estimated`,
`unknown`, `sweeps`), `snapshot.session.sweeping` whether a sweep is running. Calling `start` again with different
options or after a new target rebuilds the controller and keeps the actuator session (journal, baseline).

## Events

Each line is one JSON object: `source` (`control`, `actuator`, `daemon`), `type`, `at_s` (audio clock: seconds
of main-tap audio analysed), `unix`, and the event's own fields.

- `control`: `mix_engine::ControlEvent` — `mode_changed`, `measured {score, top_problems}`, `planned`,
  `suggested {moves}`, `batch_started`, `inspected`, `batch_kept {before, after, predicted_gain}`,
  `batch_reverted`, `batch_inconclusive`, `batch_failed`, `param_frozen`, `confirmation_needed {key, summary}`,
  `confirmed`, `section_changed`, `engineer_moved`, `holding {reason, objective}`, `resumed`, `paused`,
  `strip_measured {strip, due, priority, loudness_lufs, active}` (M062 solo sweep; `planned.reason`
  `strips_measured` when its result re-planned), `paced {until_s, rate_db, max_db_per_min}` (M067: the next batch
  waits for the change budget's rate window).
- `actuator`: `mix_actuator::ActuatorEvent` — `queued`, `refused`, `started`, `progress`, `yielded`, `paused`,
  `resumed`, `done {changes}`, `aborted`, `soloed`, `measured`, `solo_restored`, `reverted {restored, conflicts,
  skipped}`, `rta_restored`, `safety {trip, reverted}`, `link_lost`, `link_restored {drifted}`, `stopped`, `ready`,
  `snapped {report}` (M064 slot recall: `SnapReport` `{id,label,changes,skipped,phases,models,write_ms,total_ms,
  refused,aborted}`), `committed {id,label,changes}`; `started.kind` `snap`, `aborted.reason` `superseded` (a running
  move ended in place by a recall).
- `daemon`: `started`, `console_connected`, `console_disconnected`, `input_selected`, `load_started`,
  `load_progress {what, stage, progress}`, `target_loaded`, `load_failed`, `roles_confirmed`, `session_started`,
  `session_stopped`, `session_ended`, `emergency_stop`, `signal {signal}`, `live_context_changed {live_context, applied}`, `panic`, `stopped`, `heartbeat` (subscribers only).

## Snapshot

```text
at_s, mode, live_context, live_context_applied, console {kind,name}, target {kind,label,info,context}
           (target.context: {context, applied, plr_db, psr_p50_db, lra_lu, band_width_p50_db[6], deltas} of the effective target;
            `deltas` counts the context's applied_deltas entries, 1 = marker only = match_record)
loudness   {momentary_lufs, short_term_lufs, true_peak_dbtp, crest_db, tilt_db_per_oct, width_db, correlation}
spectrum   {erb_hz[40], current_erb_rel_db, target_erb_rel_db (p50), target_erb_lo_db (p10), target_erb_hi_db (p90),
            tolerance_db, third_oct_hz[31], current_third_oct_db, target_third_oct_rel_db}
session    {running, stopped, feedback_guard, holding, busy, budget [batches, net dB], change_budget}   (null without a session)
           change_budget (M067): {batches, max_batches, net_db, max_net_db, rate_db, max_db_per_min, travel_db,
            paced_until_s?}: net_db = weighted deviation of the session's own dB changes from the baseline (makeup
            free, equal group/pair moves count once, reverted moves cost nothing), rate_db = weighted forward travel
            in the last minute; a batch over the rate waits (`paced` event), over the net limit it is skipped
score      last measured PlanScore {objective, distance, problems, target_load}
curve      [{at_s, score, kept}]  the distance curve
problems   top 20 diagnosed problems (evidence, severity, suggested blocks)
plan       moves of the current plan not yet run (MoveSummary: label, strip, phase, changes, predicted_gain, ...)
pending    moves waiting for a confirmation key
moves      every batch run: {seq, at_s, summary, outcome kept|reverted|inconclusive|failed, before, after, note}
log        the last 60 actuator events (no progress)
safety     {status {feedback_latched, feedback_suspect, ceiling_hold, stalled}, trips [{at_s, trip, reverted}],
            rta_feed {frames, source}|null}
touched    [{param, baseline, current}]  every param the session changed (the PANIC target)
slots      {count, active, comparing, live_confirmation_needed}   (M064; also in status)
```

The current spectrum is the short-term (3 s) main analysis, refreshed every 0.5 s of audio.

## Status

`status.json` and `status`: `running`, `pid`, `version`, `updated_unix`, `uptime_s`, `state_dir`,
`console {kind,name,key,scene}`, `input {kind,description,sample_rate,taps,dropped_frames,clock_s,stream_errors}`,
`target {kind,label}`, `loading {what,stage,progress}`, `tools {uv,claude: {path|null, for}}` (M065: where the
daemon found each external tool; at start it merges the login shell's PATH and `~/.local/bin`, `/opt/homebrew/bin`,
`/usr/local/bin`, nvm node bins into its own, so an app-launched daemon finds them), `slots {count,active,comparing,
live_confirmation_needed}` (M064), `mode`, `live_context`, `live_context_applied`, `session {running,stopped,holding,objective,distance,
batches,pending,touched}`, `safety {status,trips,rta_feed}`, `subscribers`, `last_error`. Absent parts are `null`.

## Threads

```
source (cpal callback | FileSource player | sim renderer) --SPSC ring--> analysis thread --Mutex<Meters>--> engine
                                                                          |  feeds SafetyMonitor             |
                                                                          v                                   v
                                                                     actuator runner (25 Hz) <-- jobs -- Controller::tick
```

- The device callback only copies into the lock-free ring (mix-capture); overruns are counted (`dropped_frames`).
- The analysis thread pushes every tap into its `StreamingAnalyser`, feeds the safety monitor (`feed_mains` every
  100 ms of audio, `feed_features` for monitor taps) and keeps the audio clock. A stalled input trips the
  monitor's stall watchdog (hold at 2 s, STOP at 10 s).
- The engine thread handles requests between controller ticks (planning can take ~2 s; STOP/PANIC bypass it).
- The controller runs on the audio clock (so settle/window lengths are audio time even when the sim renders faster
  than real time); the actuator and the safety monitor run on the actuator thread's wall clock.

## Roles

`list_roles` runs the remit's role detection (`detect_topology` with the policy vocabulary) on the console and
shows the confirmations stored for `<console key>|<scene>` (sim: `sim`; WING: `wing:<model>:<name>`; scene from
`connect_console` or `confirm_roles`). Confirmed roles are given to the remit gate and the controller when a session starts. `confirm_roles` also applies
**mid-session** (M053): the actuator's gate is a `LiveRolesGate` that rebuilds its remit context (keeping the session
confirmations) from the new roles before its next decision, so writes to a bus just confirmed as `monitor` are refused
at once, and the controller is rebuilt between batches so new plans use them (the actuator session, journal and
baseline are kept; the controller's move history restarts, as after a target change).

## Mix slots

M064. Keep several realised mixes, one per reference or description, and switch between them instantly.

- **What a slot holds.** The values of every param the assistant manages (`mix_surface::slots::managed_params`:
  writable params of every exposed strip that is not a matrix/DCA, minus hard-forbidden classes and anything the
  remit forbids now: monitor/comms/locked/unconfirmed roles, default deny; conditional params are stored and decided
  again on recall), the raw target (kind, label, info, live context, TargetProfile), the daemon run and engineer-move
  counter at storing time, the console identity and the distance/objective of the last measurement. Typical size on a
  WING: ~3000 params.
- **`Original`** (built in) is captured when a session opens (the session baseline). It has no target: recalling it
  keeps the active target, so it shows how far the untouched mix is from that target.
- **Auto-capture.** When an autopilot session converges (`holding {reason: converged}`), the result is stored as
  the target's auto slot (updated on later convergences on the same target, else the next free letter).
- **Recall** (`slot_recall`):
  1. refused with a reason if the slot does not map onto the console now (`mix_surface::slots::check_compatible`:
     strip gone, class/unit changed, stored value invalid; model-dependent leaves of a block whose model the slot
     switches are checked after the switch) or was stored on another surface family;
  2. on a real console (not the embedded sim) the first recall per connection needs `confirm_live: true`: the
     audience hears the switch;
  3. opens the actuator session if needed (needs a console and an input; captures `Original`); refused while STOP
     is in effect;
  4. the slot's params that differ from the console now are the switching set of this recall. Params the engineer
     moved (any non-daemon change seen on the console) since the slot was stored are **kept** (`kept_engineer`)
     unless `keep_engineer: false`; then the slot's value is written and those params count as the engineer's
     choice from then on;
  5. the actuator **snaps** them (`mix_actuator::SnapRequest`): one console batch (`Surface::write_batch`; on a WING
     one wire write + one barrier), not ramped. A model switch goes first, the switched block's params follow in a
     second batch after the console re-resolved them (M038 ordering; `may_click`). Every write is checked by the
     remit + safety gate (`check_recall`, see docs/safety.md); refused params are listed in `skipped`;
  6. the comparison starts (first recall): the mode drops to `observe` (the loop measures, moves nothing; `undo`,
     `start` and `set_mode autopilot` are refused while comparing) and the slot's target becomes the active target
     (with its own live context), so the live view and the distance follow the slot.
- **Commit** records the comparison's end state as one journal batch (from = the values before the comparison's first
  recall, to = now) and one undo unit: `undo` ramps it back. **Resume** commits, then continues the loop from the
  current slot (default autopilot). PANIC ends a comparison without committing (and reverts the session as usual).
- **Latency** (wing-sim via mix-testsim, release build, `tests/slots.rs`): 9 params without a model switch ~30 ms
  from request to reply (write itself < 0.1 ms; most is the compatibility check of ~3000 stored params); 20 params
  with an EQ model switch ~63 ms (the actuator's part 33 ms, of which most waits for the console's remap); the last
  value is echoed to a second client on the console within 1 ms of the reply. Embedded sim: ~16 ms.

## Console RTA feed

With a console that has an RTA (`capabilities.rta`: a WING) connected and the feedback guard on (default for a real
console), a session claims the console RTA (`Surface::claim_rta`, main bus post) and feeds every new meter frame's 120 bands to
`SafetyMonitor::feed_spectrum` under the label `rta:<strip>` (`rta_feed {frames, source}` in `status`/`snapshot`).
The previous `rtasrc`/`rtatap` come back exactly when the session ends (`stop` keeps it, `disconnect_console`, `quit`
or a signal release it). An inspection that re-points the RTA is restored to ours afterwards; frames are labelled by
the strip then listened to, so detector histories never mix. The sim has no RTA: no feed.

## Signals

`SIGINT`/`SIGTERM` shut the daemon down like `quit`: a `signal` event, the actuator is STOPped (restoring any
inspection solo/RTA), the RTA claim released, the final `status.json` (`running:false`) written and `control.sock`
removed; exit code 0. A second signal exits immediately (130).

## CLI

`wing-mix-cli run` runs the daemon in-process from flags and prints events (state dir default
`<WingMix>/cli-run`):

```text
wing-mix-cli run --console sim --sim-scene degraded --sim-speed 0 --reference ref.wav --mode suggest
wing-mix-cli run --console 192.168.1.20 --input WING --channels 1,2 --solo-channels 3,4 --describe "warmer vocal" --mode autopilot
```

`wing-mix-cli ctl [--state-dir DIR] <request>` sends one request to a running daemon (`ctl status`, `ctl STOP`,
`ctl '{"cmd":"set_mode","mode":"observe"}'`); `ctl subscribe` prints its event stream.

## Tests

`tests/daemon_signals.rs` (SIGTERM during autopilot, SIGINT idle, mid-session role confirmation, live-context
setting), `tests/daemon_sim.rs::daemon_live_context_switch_changes_target_and_problems` (M054), `tests/rta_feed.rs` (headless wing-sim via mix-testsim: RTA claimed, frames fed, restored; skips without
the wing-sim binary), `gate.rs` unit test (a bus confirmed as monitor mid-session is refused on the next write).

`cargo test -p wing-mixd`: protocol parsing, tap layouts, the socket server (round trip, stale socket, AddrInUse,
stream), event hub, status/roles persistence, the analysis thread, and `tests/daemon_sim.rs`: the `wing-mixd`
binary driven only through the socket against the embedded sim fed from a 16-channel WAV of the synthetic song
(degraded scene), with the reference-scene render as reference WAV: suggest -> a plan appears and nothing is
written; autopilot -> a batch is kept and the objective falls; PANIC -> every touched param is back at its
baseline; quit -> final status, socket removed.

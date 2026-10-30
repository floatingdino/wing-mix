---
id: M065
title: Bus role detection accuracy (multi-hop routing, FX buses) + bulk remit editing
stage: S5
status: review
owner: agent-M065
depends: [M061]
branch: task/M065
model: opus
---

## Goal
The user reports inaccurate bus classification on the Remit screen. Their real scene is
`~/Library/Application Support/WingSimulator/state.snap` (read-only; copy it into a temp dir for tests and never
commit it). The intended roles are:
- **buses 1–4: FX sends.** VOX VERB, VOX DLY, INST VRB and DRUM VRB each carry an insert FX (FX1/FX7/FX2/FX4).
  None is assigned to main. Each reaches main through other buses: 1,2→bus 5; 3→bus 7; 4→bus 6→bus 7→main.
- **buses 5–7: FOH groups.** VOX and BAND BUS go to main 1+4. DRUMS FOH reaches main only via bus 7.
- **buses 8–16: monitor mixes.** IEMs plus COMMS on 16. Some send to matrices (e.g. 13→MX7).

Coordinator findings:
- (a) Detection requires a direct main assignment, so multi-hop bus→bus→main routes aren't followed: buses 1–4 and
  bus 6 look unroutable and get locked as unknown.
- (b) An insert FX on a bus is not used as FX-bus evidence.
- (c) The last WingMix session ran against the embedded sim's *default* scene ("wing-sim (in-process)", scene
  "default"), not the user's scene, so the Remit screen didn't even show their buses. There's no obvious way to
  pick "my running wing-sim" or to load a .snap into the embedded sim.
- (d) The GUI-launched daemon couldn't start the stem sidecar ("cannot start uv: No such file or directory"):
  ~/.local/bin isn't on PATH for app-launched processes.

Work:
1. **Routing graph:** build a routing graph (inputs→buses→buses→main/matrix, with send taps and on/levels) and use
   reachability to main for the "path to main" evidence. FX-bus evidence: insert/FX hosted on the bus, FX-type name
   words, inputs sending post-fader at low levels, return path via a group. Group evidence: fed by inputs or other
   buses, reaching main. Monitor evidence: pre-fader sends, no route to main, names, matrix/physical-out only.
   Keep it conservative: anything doubtful stays locked and is presented for confirmation. Add the user's scene
   layout as a role-detection test case built from a synthetic topology (no proprietary file committed), plus
   PUBLIC.snap cases.
2. **Console/scene selection:** in WingMix/wing-mixd, make it easy to connect to a running wing-sim or WING
   (discovered, including localhost), and to load a .snap into the embedded sim. Show clearly which console and
   scene the Remit screen reflects.
3. **Bulk editing on the Remit screen:**
   - multi-select (shift/cmd-click, select all of a kind);
   - set the role for the selection;
   - "accept all suggestions above confidence X";
   - range/pattern actions (e.g. "buses 8–16 → monitor", name contains "IEM");
   - lock/unlock the selection;
   - one confirmation step listing the changes before applying;
   - one undo for the whole bulk change.
   Back it with a batched `confirm_roles` in the protocol, kept backward compatible.
4. **PATH fix:** fix the daemon PATH when launched from the app. Resolve uv/claude from the login shell PATH or
   known locations (~/.local/bin, /opt/homebrew/bin), and show a clear GUI status when a tool is missing.

## Acceptance criteria
- [x] Detection on the user's scene (test via a temp copy, skipped if the file is absent) gives 1–4 fx, 5–7
      foh_group, 8–15 monitor, 16 comms (or locked for confirmation), and never permissive for 8–16
- [x] Synthetic multi-hop routing tests; all existing remit tables green
- [x] WingMix can target the user's running wing-sim (read-only detection is fine) or load a .snap into the
      embedded sim
- [x] Bulk role editing with confirmation + undo; screenshot in docs/user/
- [x] Stems start from a GUI-launched daemon; `make check` + `make app` pass

## Log
- 2026-10-31 coordinator: created from user report; findings (a)–(d) from inspecting state.snap and WingMix logs.
- 2026-10-31 agent-M065: started. Diagnosis: `mix_surface::remit::detect_topology` already propagates bus->bus->main
  (MainPath::Indirect) and uses FX inserts; on the user's scene via the WING adapter topology (`mix-wing::topology`)
  it already yields 1-4 fx, 5-7 foh_group, 8-15 monitor, 16 comms. The bug is the embedded sim: `SimSurface::topology`
  hand-builds a reduced topology (input sends + main assigns only: no bus->bus sends, inserts, FX slots, outputs,
  talkback, re-inputs). Before (user scene loaded into SimSurface): bus 1/2 fx 0.71 -> unknown, 3/4 fx 0.86 ->
  unknown, 5 foh_group 0.62, 6 foh_group 0.92 -> unknown, 7 foh_group, 8-15 monitor, 16 comms.
  Plan: move the WING topology builder to mix-wing-map (shared, no network deps), use it in SimSurface; non-exposed
  strips (aux, matrix, fx, unexposed channels) go to a new `Topology::context` so detection sees the full routing.
- 2026-10-31 agent-M065: done, status review.
  **Detection (finding a/b).** Root cause was the embedded sim's reduced topology, not the scorer: the WING topology
  builder moved from mix-wing to `mix-wing-map::topology` (mix-wing re-exports it) and `SimSurface::topology` now uses
  it; strips the sim does not expose (aux, matrices, DCAs, FX slots, channels > loaded tracks) go to the new
  `Topology::context` (serde-default, `Topology::new`/`all()`/`split_context`), which `detect_topology` and the
  remit's `feeds_protected` walk, so bus->bus routes, FX inserts (fx slots), matrix sends and re-inputs count.
  Routing graph: shortest bus->...->main route by BFS over active sends, new `BusEvidence::main_route` shown in the
  evidence ("reaches a main through other buses: bus.6 "DRUMS FOH" -> bus.7 "BAND BUS" -> main.1"). Scorer weights
  unchanged (role-detection.toml green). User scene in the embedded sim, before -> after: bus 1/2 fx 0.71 unknown ->
  fx 1.00; 3/4 fx 0.86 unknown -> fx 1.00; 5 foh_group 0.62 -> 0.98; 6 foh_group 0.92 unknown -> foh_group 0.98;
  7 foh_group 0.97; 8-15 monitor; 16 comms (WING adapter path was already correct).
  **Console/scene (c).** `list_consoles` lists a wing-sim on this Mac from its status.json (`local:true`,
  127.0.0.1:2222; `Config::wingsim_state_dir`, `$WINGSIM_STATE_DIR`); `.snap` sim scenes key roles by file name;
  `list_roles` returns `console {kind,name,scene}` and per-strip `context`. WingMix Listen: "wing-sim on this Mac",
  "Scene file (.snap)..." with a file chooser, connected line shows console + scene, demo-scene hint; Remit header
  says which console/scene it shows (warns on demo scenes); USB-out patch check under each tap (reads
  `output.usb.N/grp,in` via `params`, warns if Main is not MAIN 1/2 or Solo not MON 1/2); setup guide link.
  **Bulk editing.** `confirm_roles` is a validated batch; `null` clears; reply `previous` = exact undo batch
  (backward compatible: string maps work as before). WingMix: click/Cmd/Shift multi-select, select all
  shown/buses/inputs/needing confirmation/range/name contains, kind filter, Set role/Lock/Unlock (accept detected)/
  Reset to detection/Accept suggestions >= X%, confirmation sheet listing every change (flags "unprotects"), one
  Undo. Logic in WingMixCore `RoleBulk` (tested). Screenshots docs/user/remit-bulk.png, remit-bulk-applied.png.
  **PATH (d).** wing-mixd `tools`: at start merges the interactive login shell PATH (marker-delimited, 3 s cap),
  ~/.local/bin, ~/.cargo/bin, /opt/homebrew/bin, /usr/local/bin, nvm node bins; `status.tools {uv,claude}`;
  WingMix also extends PATH when launching the daemon and shows a notice on the Target screen when uv/claude is
  missing. Verified: a daemon started with `env -i PATH=/usr/bin:/bin...` finds both, and a stems
  `load_reference` ran (`Used { model: "htdemucs@4.1.0" }`) with the app's stems project.
  **Coordinator addition:** docs/user/setup-wing-simulator.md (direction table, patch Main/MON directly to unused
  USB outs, recorded mix tracks must not feed console channels, Listen steps, Microphone permission, embedded-sim
  contrast), linked from the Listen screen.
  Files: crates/mix-surface/src/{model.rs,remit/{detect,evidence,eval,mod}.rs}, crates/mix-surface/tests/
  routing_graph.rs, crates/mix-wing-map/src/{lib.rs,topology.rs (moved)}, crates/mix-wing/src/lib.rs,
  crates/mix-sim/src/surface.rs, crates/mix-sim/tests/scene_roles.rs, crates/mix-x32/src/surface.rs,
  crates/mix-engine/src/{plan,diagnosis}/tests.rs (Topology::new only), apps/wing-mixd/src/{daemon,protocol,status,
  lib,main,tools,local_sim}.rs, apps/wing-mixd/tests/{console_select.rs,daemon_sim.rs}, apps/WingMix/Sources/
  {WingMix/{AppModel,RemitView,ListenView,TargetView,Screenshots,WingMixApp}.swift, WingMixCore/{Models,
  DaemonStatus,DaemonProcess,RoleBulk,UsbPatch}.swift}, apps/WingMix/Tests, docs/{control-protocol.md,
  research/remit.md, user/{README.md,setup-wing-simulator.md,remit-bulk*.png}}.
  Test: `make check` (pass), `make app` (pass: 28 Swift tests incl. bulk batch + undo against the real daemon,
  smoke ok). User-scene tests: `cargo test -p mix-sim --test scene_roles -- --nocapture` (temp copy of
  state.snap; skipped when absent; `WINGMIX_USER_SNAP` overrides). Running wing-sim path: wing-mixd
  `tests/console_select.rs::running_wing_sim_roles` (private mix-testsim wing-sim with PUBLIC.snap).
  Gotchas: `mix-wing --test inproc preamp_alias_follows_the_patch_and_the_source` failed once under full-workspace
  load and passed alone/on rerun (timing; unrelated). Linux/Windows clippy could not be run locally (alsa-sys /
  blake3 cross-build); OS-specific code is cfg-gated (tools.rs unix-only shell probe, Windows `.exe` lookup).
  Proposed follow-ups: (1) the name tokenizer counts "VOX" in "VOX 1 IEM" as a group word (harmless, noisy
  evidence); (2) expose aux/matrix strips for control in SimSurface so the embedded sim has no routing-only strips;
  (3) list_consoles could read wing-sim's real native port if wing-sim published it in status.json.

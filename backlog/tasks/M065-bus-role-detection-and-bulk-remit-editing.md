---
id: M065
title: Bus role detection accuracy (multi-hop routing, FX buses) + bulk remit editing
stage: S5
status: todo
owner: -
depends: [M061]
branch: -
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
- [ ] Detection on the user's scene (test via a temp copy, skipped if the file is absent) gives 1–4 fx, 5–7
      foh_group, 8–15 monitor, 16 comms (or locked for confirmation), and never permissive for 8–16
- [ ] Synthetic multi-hop routing tests; all existing remit tables green
- [ ] WingMix can target the user's running wing-sim (read-only detection is fine) or load a .snap into the
      embedded sim
- [ ] Bulk role editing with confirmation + undo; screenshot in docs/user/
- [ ] Stems start from a GUI-launched daemon; `make check` + `make app` pass

## Log
- 2026-10-31 coordinator: created from user report; findings (a)–(d) from inspecting state.snap and WingMix logs.

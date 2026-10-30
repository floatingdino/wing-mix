# Backlog index

Source of truth is each task file's header. `python3 tools/backlog.py --ready` lists startable tasks.

## S0 — Research & foundation

| ID | Title | Depends | Model |
|---|---|---|---|
| M001 | Repo scaffold: Rust workspace, Makefile, CLAUDE.md, wing-simulator git deps | - | sonnet |
| M002 | Research: controlling WING as a client (OSC/native), meters, RTA, solo | - | opus |
| M003 | Research: analysis & reference-matching techniques and libraries | - | opus |
| M004 | Research: remit taxonomy + default WING remit policy | - | opus |
| M005 | Research: mixing knowledge base as structured data | - | opus |

## S1 — Surface layer

| ID | Title | Depends | Model |
|---|---|---|---|
| M010 | mix-core + mix-surface: generic console model, Surface trait, Remit engine, journal | M001, M004 | opus |
| M011 | mix-wing: WING adapter (OSC client, topology, solo, meters) | M010, M002 | opus |
| M012 | mix-sim: offline closed-loop SimSurface + fixture generator | M010, M002 | opus |
| M013 | Live wing-sim harness with DriverSim | M011, M012 | sonnet |
| M014 | Real multitrack fixture loader (REAPER RPP + excerpts) | M001 | sonnet |
| M015 | mix-knowledge: typed loader + consistency checker | M001, M005 | sonnet |
| M016 | Reference mixes: profiles + song alignment | M014, M022 | opus |
| M017 | Real fixture: non-input tracks (FOH + video mix taps) | M012, M014 | sonnet |
| M018 | mix-wing follow-ups: reconnect, preamp alias, shared mapping | M011, M012, M013 | sonnet |
| M019 | mix-wing: correct change Origin + reconnect diff events | M018 | sonnet |

## S2 — Analysis & targets

| ID | Title | Depends | Model |
|---|---|---|---|
| M020 | mix-analysis: streaming + offline feature extraction | M001, M003 | opus |
| M021 | mix-capture: audio input selection and capture | M001 | sonnet |
| M022 | mix-reference: reference file ingest → TargetProfile | M020 | sonnet |
| M023 | Stem separation sidecar (optional) | M022 | sonnet |
| M024 | Mix description → TargetProfile | M020, M030, M005 | sonnet |
| M025 | Channel source identification | M020, M010 | opus |

## S3 — Reasoning

| ID | Title | Depends | Model |
|---|---|---|---|
| M030 | mix-llm: Claude Code CLI bridge | M001 | sonnet |
| M031 | mix-engine: diagnosis (current vs target → Problems) | M020, M010, M005 | opus |
| M032 | mix-engine: action planner with DSP effect prediction | M031, M004, M012 | opus |
| M033 | mix-engine: closed-loop controller | M032, M040 | opus |
| M034 | Diagnosis calibration: no false positives on a matched mix | M032 | opus |
| M035 | Planner: gate + de-esser candidates, comp timing | M034 | opus |
| M036 | Engine tuning pass (safety-aware planning, accuracy, feedback guard) | M034, M035 | opus |
| M037 | Live-context targets: de-master references, alignment check | M034 | opus |
| M038 | Analysis + actuator follow-ups (noise floor, onsets, model-switch order) | M035 | sonnet |
| M039 | Real-song convergence: width, tonal shape, subtle faults | M036, M061 | opus |

## S4 — Actuation & safety

| ID | Title | Depends | Model |
|---|---|---|---|
| M040 | mix-actuator: human-like move scheduler + solo workflow | M010 | opus |
| M041 | Safety layer and watchdogs | M040, M004 | sonnet |

## S5 — Daemon & GUI

| ID | Title | Depends | Model |
|---|---|---|---|
| M050 | wing-mixd daemon + control protocol | M011, M021, M022, M033 | sonnet |
| M051 | WingMix.app SwiftUI GUI | M050 | sonnet |

## S6 — Milestones

| ID | Title | Depends | Model |
|---|---|---|---|
| M052 | Milestone: end-to-end against wing-simulator | M050, M051, M041, M024, M025 | opus |
| M053 | wing-mixd follow-ups: signals, mid-session roles, RTA feed | M050 | sonnet |
| M054 | Wire live-context targets into wing-mixd | M037, M053 | sonnet |
| M055 | WingMix GUI polish (names, events, undo, states) | M051 | sonnet |
| M060 | Second-surface proof: adapter guide + stub adapter | M011 | sonnet |
| M061 | Genericity fixes from the X32 proof | M060, M036 | sonnet |
| M062 | Solo-based per-strip measurement (stereo-tap setups) | M052, M039 | opus |
| M063 | E2E follow-up fixes (RTA origin, guard edges, PANIC vs engineer) | M052 | sonnet |
| M064 | Mix slots: store console states per reference, instant A/B/C/D recall | M062 | opus |
| M065 | Bus role detection (multi-hop, FX buses) + bulk remit editing | M061 | opus |
| M066 | Make CI green on macOS and Linux | - | sonnet |

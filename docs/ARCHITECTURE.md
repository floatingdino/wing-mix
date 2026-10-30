# Wing Mix — Architecture (v0, coordinator draft)

Goal: a macOS tool that listens to a live mix and adjusts a mixing console **in real time** so the mix moves towards a
supplied **reference mix** (audio file) or **mix description** (text). First surface: Behringer WING (hardware or the
`wing-simulator` virtual console, both controlled exactly like WING-Edit controls them). The core is surface-agnostic
so other consoles can be added as adapters.

Status: decisions marked (D) are fixed unless research invalidates them; (?) are open and owned by a backlog task.

## Principles (inherited from wing-simulator)
- (D) Rust workspace for everything except the GUI; SwiftUI menu/window app talks to a daemon over a unix socket
  (`<state-dir>/control.sock`, newline-delimited JSON) + `<state-dir>/status.json`, same pattern as WingSimMenu.
- (D) Real-time rules: no alloc/locks/syscalls/logging on the audio capture callback; capture → analysis via
  lock-free SPSC ring. Analysis and reasoning run on normal threads.
- (D) Backlog-driven, resumable, test-first. Every module has a module doc + tests. `make check` fast (< 5 min).
- (D) Reuse first: wing-simulator crates (`wing-proto` OSC codec + discovery, `wing-model` schema/tree/.snap,
  `wing-blocks`/`wing-dsp` to *predict* what a parameter change will do, `wing-engine`/`wing-render` for offline
  closed-loop tests). Pull them as git dependencies pinned to a rev of
  https://github.com/floatingdino/wing-simulator (public), with a commented `[patch]` to `../wing-simulator` for
  local co-development.
- (D) Audio-specific engine first. A general LLM is optional, used only where language is involved, and only via
  the **Claude Code CLI** (`claude -p ... --output-format json`) as a subprocess. Everything must work with it off.

## Process layout

```
 WingMix.app (SwiftUI)                               WING console / wing-sim daemon / WING-Edit (observer)
  reference drop | description | input picker              ▲ native TCP 2222 (+ UDP meters; OSC 2223 debug only)
  console picker | remit editor | live view                │
  mode: observe / suggest / autopilot | STOP              │
        │ control.sock (JSON) + status.json               │
        ▼                                                  │
 wing-mixd (Rust daemon) ──────────────────────────────────┘
  ├─ capture      cpal/CoreAudio input (any device: WING USB 48ch, "Wing Simulator", BlackHole…) → SPSC ring
  ├─ analysis     streaming features per tap: band spectra (1/3-oct, ERB), LTAS, LUFS M/S/I, LRA, true peak,
  │               crest/PLR, tilt, stereo width/correlation, low-end mono, onset density, per-band dynamics
  ├─ reference    file → decode (symphonia) → resample → loudness-normalise → TargetProfile
  │               (optional stem separation sidecar → per-stem targets); text → TargetProfile (rules, Claude opt.)
  ├─ surface      generic console model + Surface trait + Remit policy; adapters: mix-wing (WING), fake (tests)
  ├─ engine       diagnose (current vs target → Problems w/ evidence) → plan (Problems → Actions via knowledge
  │               base + DSP effect prediction) → closed-loop control (apply, settle, re-measure, keep/revert)
  ├─ actuator     human-like moves: solo → inspect → adjust → unsolo; eased ramps, rate limits, one param or a
  │               small connected batch at a time; journal, undo, panic-revert; restores solo/monitor state
  └─ llm          optional Claude Code CLI bridge (description parsing, trade-off arbitration, explanations)
```

## Crates (target)
```
crates/mix-core       shared types: TargetProfile, Features, Problem, Action, units (dB, Hz, ms), time
crates/mix-surface    generic console model (strips, params, capabilities, solo), Surface trait, Remit, journal
crates/mix-wing       WING adapter: native-protocol client with auto-reconnect (wing-proto nrp framing), meters/RTA
crates/mix-wing-map   WING tree <-> generic param mapping + value conversion (wing-model schema; shared with mix-sim)
crates/mix-x32        second-surface proof: Behringer X32/M32 OSC adapter + fake server (docs/adapters.md)
crates/mix-sim        SimSurface: in-process wing-engine render for closed-loop tests + fixture generator
crates/mix-analysis   feature extraction (streaming + offline), loudness, spectra, dynamics, stereo, masking
crates/mix-capture    audio input enumeration/capture (cpal), file source, SPSC ring
crates/mix-reference  reference ingest, sectioning, TargetProfile building, stem sidecar client
crates/mix-sources    channel source identification: name heuristics + feature prototypes, confidence, overrides
crates/mix-knowledge  mixing knowledge base (data/knowledge/*.toml) + typed access
crates/mix-engine     diagnosis, planning, effect prediction, closed-loop controller
crates/mix-actuator   move scheduler (ramps, batches, solo workflow), safety limits, undo/panic
crates/mix-describe   text description -> TargetProfile (rules from vocabulary.toml; mix-llm for unknown phrasing)
crates/mix-llm        Claude Code CLI bridge (optional)
apps/wing-mixd        daemon; apps/wing-mix-cli: headless CLI (analyse, match, dry-run against sim)
apps/WingMix          SwiftUI app
data/remit            default remit policies per surface; data/knowledge  mixing heuristics
docs/research         findings; backlog/ task system
```

## Generic surface model (mix-surface) (D, details in M010)
- `Strip { id, kind: Input|Aux|Bus|Main|Matrix|Dca|FxReturn, name, tags }`, `ProcessingBlock` (Trim/Filter/Gate/
  Eq/Dyn/Insert/Send/Pan/Fader/Mute/FxSlot), `ParamId` (stable string path), `ParamSpec { unit, range, taper, step }`.
- `Surface` trait: `topology()`, `read(&ParamId)`, `write(&ParamId, Value)`, `subscribe()`, `solo(StripId, bool)`,
  `solo_state()`, `meters()`; capabilities discovered per strip so the engine reasons over what exists.
- Adapters translate; the engine never sees WING paths.

## Remit (D principle, taxonomy in M004)
Every ParamId is classified **allowed / conditional / forbidden** by a policy (TOML, per surface, user-editable in GUI):
- Allowed: input trim/gain staging, filters, gate, EQ, dynamics, inserts on input/FOH buses, fader/pan/mute of
  inputs and FOH mix buses, sends to FOH mix/FX buses, re-assigning which FOH buses a channel feeds, FX slot model
  choice/params for FOH FX, moving FX between free slots.
- Conditional (needs confirmation or user tag): main bus processing, DCA levels, bus assignment changes.
- Forbidden: monitor buses (IEM/wedge — auto-detected from names, pre-fader sends, physical-out routing; user
  confirms), matrices and FX feeding matrices, I/O patching/routing, preamp phantom power, clock/sync, scenes/
  snapshots, talkback, user config, anything the policy does not recognise (default-deny).

## Human-like actuation (D principle, M040)
Gain staging first, then tone, then dynamics, then space. Inspect a channel by soloing it (and restore the exact
prior solo state). Move values with eased ramps at per-kind max rates (e.g. fader ≤ 6 dB/s, EQ gain ≤ 3 dB/s, freq
in log space), one parameter or a small connected batch (e.g. EQ band freq+gain+Q) at a time, wait for analysis to
settle, re-measure, keep or revert. Every move is journaled and undoable; STOP reverts nothing but freezes, PANIC
reverts the session.

## Closed-loop verification without hardware
`mix-sim` drives the real wing-simulator DSP offline (multitrack fixtures + scene → render), so the full loop
(analyse → diagnose → act → re-render) runs in `cargo test`. The live `wing-sim` daemon + WING-Edit is the manual
integration target; real WING hardware is the final check (human action).

# Remit: what the mix assistant may touch (M004)

Status: research output for M004 (2026-10-30). Implements docs/ARCHITECTURE.md section Remit. Consumers: M010
(mix-surface Remit loader/evaluator, role detection), M011 (WING adapter: ParamId/class mapping, evidence
extraction), M040/M041 (envelopes, discrete moves, revive), M051 (remit editor and role-confirmation UI).

Deliverables:
- this document (taxonomy, policy format, evaluation order, detection heuristics, WING mapping appendix);
- `data/remit/wing-default.toml`: the default WING policy;
- `data/remit/tests/role-detection.toml` (45 cases) and `data/remit/tests/remit-eval.toml` (79 cases), which M010 turns
  into unit tests. Both tables were checked against a Python reference implementation of sections 4 to 6 (all
  pass). A mutation of the policy makes the expected cases fail, so the tables do catch changes. The scripts are not
  in the repo (research tasks write only their named outputs); section 9 describes them.

## TL;DR

- **Three verdicts:** allowed, conditional (needs a user confirmation or a strip tag), forbidden. Anything not
  positively allowed is forbidden (default-deny).
- **Seven layers, first decisive layer wins:**
  1. unknown or read-only params;
  2. a **hard-forbidden** list compiled into the code (no policy can lift it);
  3. **locks** on strips by role or kind: monitor, comms, matrix and unconfirmed strips, including sends *into* them
     and FX slots inserted *on* them;
  4. priority-ordered **rules** over generic selectors;
  5. **unlock** by a strip tag or a user confirmation;
  6. **coupling** (a write to an allowed strip that audibly changes a protected mix is tightened);
  7. **revive** (never bring up what the engineer pulled down without asking).
- **Generic model.** Rules use strip kinds, strip roles, param classes and generic ParamIds. They never use WING
  paths. The WING adapter maps all **36,499** WING parameters to a class with no gaps:
  - 14,027 hard-forbidden (mostly control surface and I/O);
  - 2,948 read-only or inspection;
  - 19,524 decided by policy.
- **Role detection** for buses uses a scorer over name tokens, pre-fader share of sends, subgroup mode, path to
  main, own physical output, talkback destination and FX inserts. Input channels and mains use simple rules.
- **Detection only ever fails safe.** Protection is applied automatically. A bus becomes writable only with
  high-confidence FOH evidence, no monitor or comms name and a path to main; otherwise it waits for the user.
- **Result on the real scene** `fixtures/PUBLIC.snap`:
  - all 8 IEM buses and COMMS are locked;
  - 4 FX buses and 3 groups are auto-accepted;
  - talkback, click and drum-talk channels are locked;
  - the FOH groups and FX buses that are re-input into the IEM mixes are flagged as *coupled*.

## 1. Principles

1. **Default-deny.** A param is writable only when a rule allows it. Unknown classes, unknown strips, read-only
   values and anything new in a firmware update are forbidden until classified.
2. **Fail safe in both directions of doubt.** Detection may lock a strip automatically. Unlocking (treating a bus as
   FOH) needs strong evidence or a user confirmation. A wrong lock costs a missed improvement; a wrong unlock can
   blow a performer's ears or kill the show.
3. **The engineer's state is sacred.** The assistant never:
   - re-patches anything;
   - recalls scenes;
   - changes talkback, monitoring or solo configuration;
   - mutes mains or mute groups;
   - brings back a path the engineer silenced, unless asked.
4. **Shared signal is protected signal.** On a shared FOH and IEM console, input processing often feeds IEM mixes
   pre-fader. The remit computes this *coupling* per param and tightens or blocks such moves (section 5).
5. **Rules are generic.** The policy speaks in strip kinds, roles and param classes. Surface-specific knowledge lives
   in the adapter (appendix A for WING) and in the detection vocabulary.

## 2. Generic vocabulary (proposal for mix-surface, M010 owns the final names)

### 2.1 Strip kinds and roles

| kind | what it is | roles (effective, section 6) |
|---|---|---|
| `input` | mic/line channel (WING ch 1-40) | `foh` (reaches a main), `monitor` (feeds only monitor mixes, or monitor name such as CLICK), `comms` (talkback source or comms name), `unrouted` |
| `aux` | secondary input strip (WING aux 1-8: line/stereo/USB/re-input) | as `input` |
| `bus` | mix bus (WING bus 1-16) | `foh_group`, `fx` (FX return bus), `monitor`, `comms`, `aux_feed` (stream/record/zone/sub feed), `unknown` |
| `main` | main mix (WING main 1-4) | `main`, `aux_feed` |
| `matrix` | matrix (WING mtx 1-8) | `matrix` (kind-locked) |
| `dca` | control group (WING dca 1-16) | `dca` |
| `fx` | FX rack slot pseudo-strip (WING fx 1-16); `hosts` = inserts it sits on | none (rules use `fx_free`, `fx_host_kinds`, `fx_host_roles`) |
| `fx_return` | dedicated FX return strip (none on WING; other consoles) | as `bus` |

Non-strip domains, all hard-forbidden: `mutegroup.N`, `source.<grp>.<n>` (preamps and sources), `output.<grp>.<n>`
(output patch) and `system/...` (clock, scene, talkback, monitor section, solo config, meters, control surface,
recorder, player, network).

### 2.2 ParamId grammar

`<strip>/<block>[/<sub>]/<leaf>`, where `<strip>` = `<kind>.<n>` and segments are separated by `/`. Examples:
`input.3/eq/band.2.gain`, `input.3/send/bus.8/level`, `bus.5/insert.1/slot`, `fx.4/param.decay`, `main.1/fader`,
`system/clock/source`.

Blocks are `preamp`, `in` (trim, balance, polarity, delay), `source`, `filter`, `pre_eq`, `gate`, `eq`, `dyn` (with
`key.*` sidechain and `xover.*`), `insert.1`, `insert.2`, `fader`, `mute`, `pan`, `width`, `mono`, `send/<dest>/*`,
`order`, `tap`, `tap_width`, `out_delay`, `meta`, `link`, and for fx slots `model`, `mix`, `param.*`.

Globs: `*` matches within one segment, `**` matches any number of segments (`input.*/eq/**`).

Model-dependent leaves (WING slot-positional params) are given semantic names by the adapter (`band.3.gain`,
`threshold`). The remit never depends on leaf names: the **class** decides.

### 2.3 Param classes

The surface reports a class for every ParamId (`ParamSpec.class`). The "default" column is what
`wing-default.toml` gives on a writable FOH strip (`input`/`aux` role `foh`, or a `bus` role `foh_group`/`fx`).
**H** = hard-forbidden everywhere.

| class | meaning | default |
|---|---|---|
| `trim`, `balance` | digital input trim, stereo balance | allowed (main: conditional) |
| `preamp_gain` | analog preamp gain of the source feeding the strip | conditional (unlock tag `AI`) |
| `polarity`, `input_delay` | polarity flip, input delay | conditional, discrete |
| `filter`, `eq`, `dynamics`, `sidechain_filter` | HPF/LPF/tool filter, EQ bands, enable and mix, gate/comp params, crossover, key filter | allowed with envelope (main: conditional) |
| `filter_model`, `eq_model`, `dynamics_model` | processor model choice | conditional, discrete |
| `sidechain_source`, `sidechain_tap` | key routing | conditional |
| `insert_enable`, `insert_slot` | insert on/off, which FX slot | allowed (slot must be free: hard check) |
| `automix` | automixer assign/weight | forbidden |
| `fader`, `pan`, `width` | | allowed with envelope (main/DCA fader: conditional) |
| `mute` | strip mute | conditional (main mute: **H**; DCA mute: forbidden) |
| `mono` | bus mono sum | conditional |
| `send_level`, `send_pan` | send to FOH bus or main | allowed; into monitor/comms/matrix/unknown: locked |
| `send_on` | assignment (to FOH bus: allowed; to main or bus->bus: conditional) | see rules |
| `send_tap`, `send_pan_link`, `tap_point`, `tap_width` | pre/post/subgroup mode, tap position | forbidden (changes what monitor mixes get) |
| `processing_order` | block order | conditional (forbidden when coupled) |
| `fx_param`, `fx_mix`, `fx_model` | FX slot | allowed for free slots and slots hosted only by FOH strips; conditional on main; locked on monitor/matrix |
| `metadata`, `tags` | names, colours, icons; tags (WING tags carry DCA and mute-group membership) | forbidden |
| `out_delay` | bus/main/matrix output delay (system alignment) | **H** |
| `link` | stereo/custom/main link | **H** |
| `mutegroup_mute` | mute-group master | **H** |
| `phantom`, `oscillator`, `source_mute`, `source_mode`, `source_polarity`, `preamp_remote` | source-side controls | **H** |
| `routing.input_patch`, `routing.alt_source`, `routing.output_patch`, `routing.user_signal`, `routing.direct_in`, `fx_external` | any patching | **H** |
| `clock`, `scene`, `system`, `control_surface`, `talkback`, `monitor_section`, `solo_config`, `meter_config`, `recorder`, `player`, `automix_config` | console configuration | **H** |
| `inspect.solo`, `inspect.listen`, `inspect.rta` | listening and measuring | not writes; `[inspection]` (section 4.5) |
| `readonly` | live/state values (meters, `$` params on WING) | refused |
| `unknown` | anything the adapter cannot classify | forbidden |

## 3. Policy file format (`wing-mix-remit/1`)

TOML, one file per surface (`data/remit/<surface>-default.toml`). The user can edit it in the GUI. The GUI writes a
user copy and never edits the shipped default. Top-level keys:

| key | content |
|---|---|
| `format`, `name`, `surface` | identification; `surface` is informational |
| `default` | must be `"forbidden"`; the loader rejects any other value |
| `[locks]` | `roles` and `kinds` that lock whole strips; `unconfirmed` (the class for role `unknown`, must stay forbidden); `deny_tag`; `unlock_tag` |
| `[detection]` | thresholds for role detection; `[detection.names]` holds the vocabulary for each role (`monitor`, `comms`, `fx`, `foh_group`, `aux_feed`, plus `*_phrases`) |
| `[inspection]` | `solo`, `listen`, `rta`: allowed or forbidden |
| `[coupling.monitor]` | class cap per param class (and `default`) for writes coupled to a protected mix; `envelope_scale` |
| `[revive]` | `class` cap and `floor_db` |
| `[[rule]]` | ordered list (below) |

**Rule** fields:
- **Selectors.** All optional. They are AND-ed together; values inside one list are OR-ed.
  - `kinds`, `roles`, `classes`, `params` (ParamId globs), `tags_any`, `tags_none`;
  - `dest_kinds`, `dest_roles`: only send params can match these;
  - `fx_free`: a free slot has no hosts;
  - `fx_host_kinds`, `fx_host_roles`: the slot needs at least one host, and *all* hosts must match;
  - `stereo_pair` (M039): `true` matches a strip in a detected or user-confirmed stereo pair (`RemitStrip.pair`;
    detection by name in `mix_surface::remit::pairs`: `OH L`/`OH R`, or two adjacent channels of the same name,
    lower = left), and for a pan write only when the requested value lies on the strip's own side (centre
    included; a pan write without a value does not match); `false` matches strips that are not paired. The
    default policy's `input-pair-spread` (priority 1) gives pair pans `max_delta_pct = 100` (full L/R on their own
    side) while mono sources and moves across the centre keep `input-level-pan`'s +-50.
- **Effects:**
  - `class` (required);
  - `priority` (integer, default 0);
  - `unlock_tags` (strip tags that turn a conditional result into allowed);
  - `envelope`: limits relative to the value at session start, enforced by M041: `max_delta_db`, `max_boost_db`,
    `max_cut_db`, `max_delta_oct`, `max_delta_pct`;
  - `discrete` (the change cannot be ramped, so the actuator must fade the path or do it while inaudible);
  - `id`, `note`.

Loader validation (M010):
- unknown keys, classes or roles are errors;
- `default` must be forbidden and `locks.unconfirmed` must be forbidden;
- rule ids must be unique;
- a rule that names a hard-forbidden class gets a warning: it has no effect.

## 4. Evaluation

`evaluate(param, value_change, topology, policy, session) -> Decision { class, reason, rule_id, coupled, envelope,
discrete }`.

The verdicts are ordered forbidden < conditional < allowed; "cap" means take the minimum. The steps run in order,
and `reason` names the step that set the final class. The test tables use the same reason names.

1. **Class resolution.** If the class is `unknown`, return forbidden (`unknown`). If the class is `readonly`, return
   forbidden (`readonly`).
2. **Hard list** (section 7): forbidden (`hard`). Rules, tags and confirmations cannot change this. This step
   includes the value predicates:
   - a main mute;
   - an FX model the surface flags as an external loop;
   - an insert slot that already sits on another insert;
   - a solo while the solo mode is destructive.
3. **Strip resolution.** If the strip is not in the topology, or a send names an unknown destination, return
   forbidden (`unknown`).
4. **Locks.** Return forbidden (`locked`) if any of these strips has a role in `locks.roles` or a kind in
   `locks.kinds`:
   - the owning strip;
   - the destination of a send param;
   - any host of an fx slot.

   If any of them has role `unknown`, return forbidden (`unconfirmed`). If the owning strip carries `deny_tag`,
   return forbidden (`deny_tag`). The user lifts a lock by changing the strip's role in the confirmation UI (section
   6.6) or by editing `[locks]`. A rule never lifts a lock.
5. **Rules.** Collect the matching rules and keep those with the highest priority. Among them, the most
   restrictive class wins. If no rule matches, return forbidden (`default_deny`). This gives two behaviours:
   - users override defaults with a higher-priority rule;
   - two equal-priority rules can only tighten.
6. **Unlock.** A conditional result becomes allowed in either of these cases:
   - the strip carries one of the rule's `unlock_tags`;
   - the user confirmed this rule id for this session. Confirmations live in the session and are journaled; M051
     shows them.
7. **Coupling** (section 5). If the param is coupled, cap the result with `coupling.monitor[class]` (or
   `coupling.monitor.default`). Allowed coupled moves keep `envelope * envelope_scale`. Coupling caps are not
   unlocked by rule tags; they need their own confirmation, recorded as `coupling:<rule_id>`.
8. **Revive.** Cap at `revive.class` (conditional by default) any write that makes a silent path audible:
   - a fader or send level going from at or below `floor_db` to above it;
   - mute going from on to off;
   - send or insert `on` going from false to true;
   - a processing block's `on` going from false to true.

The actuator calls `evaluate` when it plans a move **and again** at write time (M040 acceptance). It does so because
roles, coupling and console state can change in between.

### 4.5 Inspection (not param writes)

`Surface::solo`, band/key listen and RTA source claims are inspection actions, governed by `[inspection]`.

Hard rule: inspection is forbidden while the surface reports a **destructive solo mode**. In solo-in-place (WING
`cfg/solo/mode` SIP, or STUDIO with a solo-mute), soloing mutes the PA.

Soloing a locked strip in non-destructive mode is allowed: it changes only the engineer's monitoring, and M040
restores that exactly. The actuator must never write `solo_config` to make solo non-destructive.

## 5. Coupling

A param is **coupled** when changing it audibly changes a protected (monitor/comms) mix, even though its own strip
is writable. The topology gives each strip:
- a block `chain`, in signal order;
- its `sends` with `{dest, tap_after, active}`, where `tap_after` is the last block before the send's tap point;
- `feeds_protected`: the strip's output reaches a protected mix by re-input after all of its processing;
- for fx slots, `hosts`.

The param's block is resolved through this map:
- `mute` maps to `fader`;
- `width` and `mono` map to `pan`;
- `pre_eq` maps to `eq`;
- `order` maps to `filter`, the first block it can reorder.

The param is coupled if any of these holds:
- its strip has an **active** send to a protected destination and position(block) <= position(`tap_after`);
- the strip is `feeds_protected`;
- it is a send whose destination is `feeds_protected`;
- it is an fx param whose slot host is coupled at its insert block.

Consequences on a typical shared console (PUBLIC.snap):
- **Channel trim** is coupled to every IEM, so it is conditional.
- **Channel EQ** before a post-dynamics IEM tap stays allowed, but at half the envelope. The singer hears a
  moderate EQ move; the assistant keeps it small and slow.
- **Mute** of a channel with post-fader monitor sends is forbidden.
- **FX buses and FOH groups re-input into IEM mixes** (WING aux "B. LINK" channels, and channels sourced from
  `BUS` legs) are coupled as a whole. Their fader is conditional, their FX model forbidden, and FX params
  conditional.

Coupling is the remit's answer to "monitor buses are forbidden": protecting only the monitor bus strips does not
stop the assistant from changing what performers hear. M041's feedback guard ("boosts on channels feeding monitor
buses") consumes the `coupled` flag.

## 6. Role detection

Role detection runs after `Surface::topology()` and again on every topology-relevant change:
- names and tags;
- send on/mode/tap;
- patching to outputs;
- main assignment;
- FX inserts;
- talkback destinations.

Every run produces, per strip, `{role, confidence, probabilities, evidence[], effective_role, needs_confirmation}`.
The evidence strings are shown in the GUI.

### 6.1 Bus evidence (generic)

| field | meaning |
|---|---|
| `name` | strip name |
| `active_senders` | input/aux sends into the bus that are on and above `active_send_floor_db`; in subgroup mode, `on` alone counts |
| `pre_fraction`, `group_fraction` | share of active sends that are pre-fader, or in subgroup mode |
| `feeds_main` | `direct` (main send on), `indirect` (through bus->bus sends to a bus that reaches main), `none` |
| `outputs` | kinds of outputs the bus is patched to: `physical` (analog, AES50/stagebox, card, AES/EBU), `computer` (USB/DAW), `recorder` |
| `talkback_dest` | a talkback path is assigned to the bus |
| `fx_insert` | `time` (reverb/delay/modulation/pitch model inserted), `other`, `none` |
| `feeds_matrix` | an active send to a matrix |
| `re_input_to_monitor` | the bus output is re-input on a strip that sends to a monitor/comms bus (second pass) |

### 6.2 Bus scorer

The scorer adds evidence into four role scores: `monitor` (protected: monitor or comms), `fx`, `foh_group`,
`aux_feed`. An `unknown` baseline score of 1.0 is added. A softmax gives probabilities, and `confidence` = P(winner).
Weights are code constants (M010), tuned only against `role-detection.toml`.

| evidence | monitor | fx | foh_group | aux_feed |
|---|---|---|---|---|
| monitor or comms name token | +4 | | | |
| fx name token | | +3 | | |
| group name token (DRUMS, VOX, GRP, FOH...) | | | +1 | |
| aux_feed name token (STREAM, REC, SUB(S), FRONT FILL, LOBBY...) | | | | +3 |
| >= 2 active senders, `group_fraction >= 0.5` | -3 | | +3 | |
| ... else `pre_fraction >= 0.75` | +2 | -1 | | |
| ... else `pre_fraction <= 0.25` | -1.5 | +1 | +0.5 | |
| `feeds_main = direct` | -4 | +1.5 | +1.5 | -2 |
| `feeds_main = indirect` | -3 | +1.5 | +1.5 | -2 |
| `feeds_main = none` | +0.5 | | | |
| physical output and no main path | +2 | | | +1 |
| outputs only computer/recorder | | | | +1.5 |
| talkback destination | +1.5 | | | |
| time-based FX inserted | -1 | +3 | | |
| re-input into monitor mixes | -2 | +0.5 | +0.5 | |
| feeds a matrix and no main path | | | | +1 |

**Name tokens.** The name is upper-cased and split on non-alphanumerics and on letter/digit boundaries, so `IEM3`
becomes `IEM 3` and `in-ear` becomes `IN EAR`. Tokens match whole words, which avoids `MONO`, `SIMON` and `LINEAR`
false hits. Phrases (`SIDE FILL`) match as consecutive tokens. The vocabulary is in `[detection.names]`, so a venue
can add its own words (another language, a monitor engineer's naming habits). `SUB` is deliberately an `aux_feed`
token: an aux-fed subwoofer bus is a PA feed. A subgroup named "SUB 1" therefore ends up forbidden, which fails safe.

**Comms vs monitor.** Routing cannot tell them apart (both are pre-fader mixes on their own outputs). There is one
protected score, and the label is `comms` when a comms token is present without a monitor token.

### 6.3 Inputs and aux inputs (rules, first match)

1. `comms` if it is a talkback source (WING `cfg/talk/assign`) or has a comms name token (TALKBACK, TB, TALK, COMMS,
   SM...).
2. `monitor` if it has a monitor name token (CLICK, IEM, CUE...), or `reaches_monitor && reaches_main == none`.
   This covers group links into IEMs, drum talk mics and click with the main fader at -oo.
3. `foh` if `reaches_main != none`. The path can be direct, through FOH buses, or through buses that reach main.
4. else `unrouted`. No rule allows it, so it is default-deny.

### 6.4 Mains

`aux_feed` if the name has an aux_feed token (STREAM, REC, BROADCAST...), else `main`. Mains are conditional
anyway; a stream/record main is forbidden.

### 6.5 Effective role (what the remit uses before any confirmation)

| condition (in order) | effective |
|---|---|
| winner is monitor (label monitor/comms) | that label |
| `name_hit_protects` and a monitor or comms token was found | monitor or comms |
| winner is `aux_feed` | `aux_feed` (restrictive, at any confidence) |
| P(monitor) >= `protect_probability` (0.20) | `unknown` (locked as unconfirmed) |
| P(fx)+P(foh_group) >= `auto_accept_confidence` (0.85), P(monitor) < 0.10, and `feeds_main != none` | the more likely of fx and foh_group |
| otherwise | `unknown` |

fx and foh_group are pooled because the default rules treat them identically. A post-fed, unnamed bus into main is
usable even though the scorer cannot say whether it is a group or a dry parallel bus.

`needs_confirmation` is true whenever confidence is below high (0.85), or a name hit disagrees with the winner. It
is advisory: the remit uses `effective` right away, so protection never waits for the user.

### 6.6 Confirmation UX (M051)

- The role table lists every bus, input and main with its effective role, confidence and evidence.
- Changing a role **towards** protection (to monitor, comms, aux_feed or unknown) applies immediately.
- Changing a role **away** from protection needs an explicit confirm for each strip. Confirmed roles are stored per
  console and scene name, and override detection until the evidence changes materially, for example when the bus
  gains pre-fader senders or loses its main path. Then detection re-runs and the strip is re-locked if needed.
- Console-side tags give the user a way to work without the GUI: `NOAI` locks the strip, and `AI` unlocks the
  conditional rules that list it.

### 6.7 PUBLIC.snap result (wing-simulator fixture, real WING scene)

| bus | name | evidence | role / confidence | effective |
|---|---|---|---|---|
| 1-4 | VOX VERB, VOX DLY, INST VRB, DRUM VRB | post sends; reverb/delay inserted; reach main via bus 5/6/7; re-input on ch 33-36 into IEMs | fx 0.99-1.00 | fx (coupled) |
| 5 | VOX | subgroup sends, main, re-input via aux 5 into IEMs | foh_group 0.98 | foh_group (coupled) |
| 6 | DRUMS FOH | subgroup sends, reaches main only via bus 7 | foh_group 0.97 | foh_group (coupled) |
| 7 | BAND BUS | subgroup sends, main | foh_group 0.97 | foh_group |
| 8-15 | VOX 1-4 IEM, KEYS IEM, GTR 1 IEM, DRUMS IEM, BASS IEM | pre sends (KEYS: 82%), own LCL/AES50 outputs, talkback A | monitor 1.00 | monitor |
| 16 | COMMS | pre sends, own AES50 B output | comms 1.00 | comms |

Inputs:
- ch 40 TALKBACK (the talkback source) and ch 31 DRUM TALK are comms;
- ch 29 COMS LINK is comms;
- ch 32 CLICK (assigned to main 1 with the fader at -oo) is monitor;
- aux 5-7 "B. LINK" are monitor (they reach IEMs only);
- everything else is foh.

Mains: main 4 "STREAM" is aux_feed.

## 7. Hard-forbidden list (compiled in)

The criteria for this list: the change can damage equipment or hearing, take the show down, alter the system
outside the mix, or cannot be undone by the assistant's own journal.

| item | why |
|---|---|
| phantom power | equipment damage (ribbons, unbalanced sources), loud transients |
| oscillator / test generator | full-band noise or tones into the PA |
| all patching: input source, alt-source switching, output patch, user signals, matrix direct inputs, external FX loops (WING `EXT` model + its I/O) | can silence or misroute the whole system; recording and broadcast splits |
| source-side mute, polarity, mode, remote-headamp control | shared by every user of the source, invisible on strips, affects other consoles on a shared stage box |
| clock source and sample rate | system-wide dropout |
| scenes, snapshots, show and library actions, save | replaces the engineer's whole state |
| talkback, monitor section (phones/speakers), solo configuration, solo safes, meter/RTA config | the engineer's own tools |
| main mute, mute-group masters | take whole sections or the PA down |
| links (main link, channel links), automix enable | structural |
| output delay (bus/main/matrix) | system alignment, not mixing |
| control surface, user buttons, DAW/MIDI/GPIO, OSC read-only switch, network, USB config, startup mute, cards, recorder/player transport | not the mix |
| writing an insert slot that already sits on another insert | steals an FX slot from another strip |
| inspection while solo is destructive (solo-in-place) | mutes the PA |
| read-only/live params | not writable |

## 8. Default policy summary (`wing-default.toml`)

| target | allowed | conditional | forbidden |
|---|---|---|---|
| input/aux, role foh | trim/balance (+-6 dB); filter/EQ/dyn/key filter (boost <= 6, cut <= 12 dB, <= 2 oct); inserts; fader/pan/width (+-10 dB, pan +-50; stereo-pair sides to full L/R on their own side, M039); sends to FOH buses; main send level | preamp gain (`AI` tag unlocks); polarity, input delay; models; key source/tap; mute; processing order; main assignment | send tap/mode, pan link, tap point, tap width, automix |
| bus, role foh_group/fx | trim, tone, inserts (+-6 dB); fader/pan/width; send levels to FOH buses and main | polarity, models, key routing, mono, mute; send assignment | to matrices (locked) |
| main, role main | none | processing, pan/width, fader (+-3 dB, `AI` tag unlocks); models, insert slot | mute (hard) |
| dca | none | fader (+-6 dB) | mute |
| fx slot | free slot: all; hosted on FOH only: params/mix (+-30 %); model (discrete) | hosted on a main | hosted on monitor/matrix (locked) |
| monitor, comms, unknown, matrix, aux_feed, unrouted | none | none | everything |

**Deviations from ARCHITECTURE.md**, proposed for the coordinator to accept:

1. **Input mute is conditional, not allowed.** A wrong mute is a show-stopper, and a mute is rarely what a tonal or
   level target needs.
2. **Preamp gain is conditional and trim is allowed.** "Input trim/gain staging" is split this way because WING
   preamp gain also changes recording and monitor splits of the same source. In PUBLIC.snap, USB 1-32 carry the raw
   AES50 inputs.
3. **New** coupling and revive layers. ARCHITECTURE does not describe them.

## 9. Test tables and reference implementation

`data/remit/tests/role-detection.toml` has 45 cases. Each gives a bus, input or main evidence record and expects a
role, a confidence band, an effective role, `usable` and `name_hit`. The cases cover:
- 19 extracted from PUBLIC.snap;
- synthetic edge cases: glued digits, phrases, MONO/SIMON non-hits, monitor-routed group names, repurposed
  "MON 5" into main, unrouted reverb, aux-fed subs, front fill, talkback-only, unused, mixed taps.

`data/remit/tests/remit-eval.toml` has 79 cases over a generic topology modelled on PUBLIC.snap. They cover:
- every evaluation step and reason;
- coupling, including the tap position before and after the processing block;
- revive;
- unlock by tag and by confirmation;
- value predicates;
- priority overlays, including attempts to beat the hard list and the locks;
- inspection.

The reference Python implementation was about 185 lines across two scripts, in the agent's scratchpad, not
committed. M010 should port sections 4 to 6 directly and drive them from these tables.

## 10. Open questions and proposed follow-ups

- **WING PRE-send tap semantics** (signal-flow.md open question 3): coupling uses `ptap` and `proc` to place the PRE
  tap. Whether PRE sends are post-mute decides whether input mute is coupled through pre sends. Until it is
  verified, the adapter assumes PRE sends are post-mute (more coupling = safer).
- **Weight tuning on more real scenes.** Only one real scene was available. Proposed follow-up: collect 3-5 user
  scenes, including a dedicated FOH-only console, and extend `role-detection.toml`.
- **Snapshot of the remit at session start.** Coupling and roles are computed live, but envelopes are relative to
  the session baseline. M040/M041 must capture the baseline before the first move.
- **Confirmed-role persistence key** (console identity + scene name) needs M050's state store.
- **Other surfaces** (M060): the generic classes cover what most consoles have (X32/M32, dLive, SQ: same concepts).
  A surface without per-send taps can report `tap_after` as the console's fixed pre-fader point.

---

## Appendix A. WING mapping (WING-Edit 3.3.4 tree, firmware 3.1+)

Source: wing-simulator `data/schema/wing-tree.json` (43,008 nodes, 36,499 params) and `variants`; semantics from
`docs/research/signal-flow.md`.

### A.1 Path -> class -> generic ParamId

Shorthand: `S` = `ch|aux|bus|main|mtx`/n, `I` = `ch|aux`/n, `B` = `bus|main|mtx`/n. Generic strip ids:
- `ch/n` -> `input.n`
- `aux/n` -> `aux.n`
- `bus/n` -> `bus.n`
- `main/n` -> `main.n`
- `mtx/n` -> `matrix.n`
- `dca/n` -> `dca.n`
- `fx/n` -> `fx.n`
- `mgrp/n` -> `mutegroup.n`

The rules are applied first match wins. The order matters: domain prefixes come first, then `$` read-only values,
then strip blocks.

| WING path (regex-ish) | class | generic ParamId |
|---|---|---|
| `$ctl/**` (layers, user buttons, safes, DAW, MIDI, GPIO, `osc/ronly`, `lib` scenes and shows, `$globals/$savenow`) | control_surface (lib: scene) | `system/control/...`, `system/scene/...` |
| `$syscfg/**`, `$globals/{startmute,usbacfg,sccfg,harmt}`, `cards/**`, `cfg/dcamgrp` | system | `system/...` |
| `$globals/{clkrate,clksrc,custsync}` | clock | `system/clock/...` |
| `$stat/**`, other `$...` and `~...` params (e.g. `ch/n/in/set/$g`, `$fdr`, `$mute`, `preins/$stat`) | readonly | none |
| `S/$solo`, `$presolo`, `$solobd`, `$sololed`, `gatesc/$solo`, `dynsc/$solo`, `dynxo/$solo`, `eq/$solo`; `cfg/rta/$*`, `cfg/mtr/$*` | inspect.solo / inspect.listen / inspect.rta | via `Surface::solo` and inspection APIs |
| `play/**`, `rec/**` | player, recorder | `system/player`, `system/recorder` |
| `cfg/mon/**` | monitor_section | `system/monitor/...` |
| `cfg/talk/**` | talkback | `system/talkback/...` |
| `cfg/solo/**`, `S/mon`, `S/solosafe`, `dca/n/mon` | solo_config | `system/solo/...`, `<strip>/solo_safe` |
| `cfg/rta/*`, `cfg/mtr/**` | meter_config | `system/meters/...` |
| `cfg/mainlink`, `I/clink` | link | `system/main_link`, `<strip>/link` |
| `cfg/amix/{x,y}` | automix_config | `system/automix/...` |
| `io/in/OSC/**` | oscillator | `source.osc.n/...` |
| `io/in/<grp>/n/vph` | phantom | `<input>/preamp/phantom` (resolved through the patch) and `source.<grp>.n/phantom` |
| `io/in/<grp>/n/g` | preamp_gain | `<input>/preamp/gain` |
| `io/in/<grp>/n/{pol,mute,mode}` | source_polarity, source_mute, source_mode | `source.<grp>.n/...` |
| `io/in/<grp>/n/{rmt,rcvc}` | preamp_remote | `source.<grp>.n/remote` |
| `io/in/<grp>/n/{col,name,icon,tags}` | metadata | `source.<grp>.n/meta/...` |
| `io/in/USR/n/user/**` | routing.user_signal | `source.usr.n/...` |
| `io/out/**` | routing.output_patch | `output.<grp>.n/source` |
| `io/altsw`, `io/autoaltovr`, `I/in/set/{altsrc,srcauto}` | routing.alt_source | `system/alt_source`, `<input>/source/alt_select` |
| `I/in/conn/**` | routing.input_patch | `<input>/source/...` |
| `S/in/set/trim`, `S/in/set/bal` | trim, balance | `<strip>/in/trim`, `<strip>/in/balance` |
| `S/in/set/inv` | polarity | `<strip>/in/polarity` |
| `I/in/set/{dly,dlyon,dlymode}` | input_delay | `<strip>/in/delay[.on, .mode]` |
| `ch/n/flt/{lc,lcf,lcs,hc,hcf,hcs,tf,1,2}`; `flt/mdl` | filter; filter_model | `input.n/filter/{lowcut.*,highcut.*,tool.*}` |
| `ch/n/peq/**` | eq | `input.n/pre_eq/...` |
| `S/eq/{on,mix,1..20}`; `S/eq/mdl` | eq; eq_model | `<strip>/eq/...`; `<strip>/eq/model` |
| `S/{gate,dyn}/{on,mix,gain,1..19}`, `S/dynxo/*`; `S/{gate,dyn}/mdl` | dynamics; dynamics_model | `<strip>/{gate,dyn}/...`, `<strip>/dyn/xover.*` |
| `S/{gatesc,dynsc}/{type,f,q}`, `/src`, `/tap` | sidechain_filter, sidechain_source, sidechain_tap | `<strip>/{gate,dyn}/key.*` |
| `S/{preins,postins}/on`, `/ins` | insert_enable, insert_slot | `<strip>/insert.{1,2}/{on,slot}` |
| `I/postins/{mode,w}` | automix | `<strip>/insert.2/{mode,weight}` |
| `S/main/m/{lvl,on,pre}` | send_level, send_on, send_tap | `<strip>/send/main.m/{level,on,tap}` |
| `S/send/b/{lvl,on,pan,plink,mode,pon,pre}` | send_level, send_on, send_pan, send_pan_link, send_tap | `<strip>/send/bus.b/...` |
| `S/send/MXk/**` | as above, dest matrix | `<strip>/send/matrix.k/...` |
| `mtx/n/dir/**` | routing.direct_in | `matrix.n/direct.k/...` |
| `S/fdr`, `S/mute`, `S/pan`, `S/wid`, `B/busmono` | fader, mute, pan, width, mono | `<strip>/{fader,mute,pan,width,mono}` |
| `ch/n/{proc,ptap,tapwid}` | processing_order, tap_point, tap_width | `input.n/{order,tap,tap_width}` |
| `B/dly/**` | out_delay | `<strip>/out_delay/...` |
| `S/{col,name,icon,led}`, `dca/n/{name,col,icon,led}`, `mgrp/n/name` | metadata | `<strip>/meta/...` |
| `S/tags` | tags | `<strip>/meta/tags` |
| `dca/n/{fdr,mute}` | fader, mute (kind dca) | `dca.n/{fader,mute}` |
| `mgrp/n/mute` | mutegroup_mute | `mutegroup.n/mute` |
| `fx/n/mdl`, `fx/n/fxmix`, `fx/n/1..N` | fx_model, fx_mix, fx_param | `fx.n/{model,mix,param.<name>}` |
| `fx/n` model `EXT` and its `egrp/ein/emode/lat/trim` | fx_external | hard |

### A.2 Coverage

Each of the 36,499 parameters of the WING-Edit 3.3.4 tree matches exactly one row above, with none unmapped.
Counts by class:

| class | params |
|---|---|
| control_surface | 10,265 |
| send_tap | 2,976 |
| dynamics | 2,824 |
| readonly | 2,382 |
| eq | 2,268 |
| metadata | 2,160 |
| send_on, send_level | 1,824 each |
| send_pan, send_pan_link | 1,152 each |
| routing.output_patch | 748 |
| fx_param | 640 |
| inspect.* | 566 |
| source_mode, source_mute, source_polarity | 446 each |
| preamp_remote | 400 |
| filter | 360 |
| sidechain_filter | 348 |
| preamp_gain, phantom | 200 each |
| routing.input_patch | 192 |
| routing.user_signal | 160 |
| solo_config | 148 |
| input_delay, insert_enable, insert_slot | 144 each |
| sidechain_source, sidechain_tap, dynamics_model | 116 each |
| routing.alt_source | 98 |
| fader, mute | 92 each |
| out_delay | 84 |
| automix | 80 |
| trim, balance, polarity, pan, width, eq_model, tags, monitor_section | 76 each |
| talkback | 66 |
| link | 49 |
| filter_model, processing_order, tap_point, tap_width | 40 each |
| routing.direct_in | 32 |
| mono | 28 |
| meter_config | 26 |
| oscillator | 18 |
| fx_model, fx_mix | 16 each |
| system | 13 |
| mutegroup_mute | 8 |
| recorder | 3 |
| automix_config | 2 |
| player | 1 |

By layer:
- hard: 14,027;
- read-only and inspection: 2,948;
- left to policy: 19,524 (of which most send/metadata params end up forbidden by rule or by lock).

Model-variant slot params (`variants`) inherit the class of their block. A new firmware path that matches no row
is `unknown`, so it is forbidden.

### A.3 WING specifics the adapter must honour

- **`$`-prefixed params are not a reliable read-only flag** (wing-edit-schema.md 9.2). The adapter treats `$`
  params as readonly unless listed under inspection, and refuses writes to them. `ch/n/in/set/$g` mirrors the source
  preamp gain: write `io/in/<grp>/<n>/g` (resolved through `in/conn`, honouring the active alt source) and mark the
  strip `shared-source` when more than one channel, or a direct output patch, uses the same source.
- **Slot-positional model params.** `eq/5` means a different thing in every model. A model switch remaps all of
  them, which is why models are `discrete` and conditional. The adapter names leaves from `variants`.
- **Tags** (`S/tags`) carry DCA membership (`#Dn`) and mute-group membership (`#Mn`) next to free words. The
  `NOAI`/`AI` remit tags are free words in the same string, so they are read but never written.
- **Send taps.**
  - `ch/aux send/b/mode` PRE/POST/GRP; GRP counts as subgroup mode (`on` = assignment, level ignored).
  - Bus and main sends use `pre` true/false.
  - Channel PRE tap = `ptap`: IN, FILT, 3, 4, 5, PFL, AFL, POST. 3/4/5 are after the 1st/2nd/3rd block of `proc`.
    So `tap_after` = in | filter | proc[0..2] | proc[3] (PFL) | fader (AFL) | pan (POST). In the chain, the G, E, D
    and I letters map to gate, eq, dyn and insert.2, and insert.1 sits after filter.
  - Aux inputs have no `ptap`: PRE = after dyn (pre-fader).
- **Fixture caveat.** PUBLIC.snap is firmware 1.13: channels have `ptap` and `proc`; bus `busmode` mirrors the
  per-send mode.
- **Output patch legs.** `io/out/<grp>/n = {grp: BUS|MAIN|MTX, in: k}` addresses mono legs: bus `(k+1)/2`. Groups
  map to output kinds:
  - LCL, AUX, A, B, C, SC, CRD, MOD, AES -> `physical`;
  - USB -> `computer`;
  - REC -> `recorder`.
- **Re-input.** A channel or aux with `in/conn/grp = BUS|MAIN|MTX` re-inputs leg `in` (bus `(in+1)/2`). This drives
  `feeds_protected` and `re_input_to_monitor`.
- **Talkback.** `cfg/talk/{A,B}/B1..B16` are the destination buses (-> `talkback_dest`). `cfg/talk/assign`
  `CH40|AUX8` is the talkback source channel (-> input role comms).
- **FX hosts.** `S/{preins,postins}/ins = FXn` (insert.1 / insert.2). An FX slot is free when no insert names it.
  Time-based models for `fx_insert = time` are HALL, ROOM, CHAMBER, PLATE, CONCERT, AMBI, V-ROOM, V-REV, V-PLATE,
  GATED, REVERSE, DEL/REV, SHIMMER, SPRING, ST-DL, TAP-DL, TAPE-DL, OILCAN, BBD-DL, VSS3, BPLATE, DIMCRS, CHORUS,
  FLANGER, PITCH, D-PITCH, DOUBLE. External loop model: `EXT`.
- **DCA mute.** With `cfg/dcamgrp` on, a DCA mute also mutes members' pre-fader sends, which is why DCA mute is
  forbidden by default.
- **Solo is destructive** when `cfg/solo/mode` = SIP, or STUDIO when solo-mute is active (`cfg/solo/mute`). The
  adapter reports `solo_destructive`.
- **OSC read-only.** `$ctl/osc/ronly` true means writes will fail. The adapter must detect this and report it (the
  remit forbids writing it).
- **Main path.** `S/main/m/on`, and bus->bus (`bus/n/send/b/on`, b in 1..8) for indirect paths. Mains reach matrices
  only.

### A.4 Evidence extraction for PUBLIC.snap (reproducible)

These rules produced the fixture rows in `role-detection.toml`:
- `active_senders` counts `ch|aux/*/send/b` with `on` and (`lvl > -90` or mode GRP);
- `pre_fraction` and `group_fraction` come from each send's `mode`;
- `feeds_main` comes from `bus/n/main/*/on`, with indirect paths propagated over `bus/n/send/b/on` (lvl > -90);
- `outputs` come from `io/out` legs;
- `talkback_dest` comes from `cfg/talk/{A,B}/Bn`;
- `fx_insert` comes from the `preins`/`postins` slot's `fx/k/mdl`;
- `re_input_to_monitor` is a second pass: a channel or aux sourced from the bus with an active send to a bus
  classified monitor/comms in pass one.

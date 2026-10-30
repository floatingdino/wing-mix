# Controlling a WING as a client: OSC vs native, meters, RTA, solo, topology (M002)

Status: research output for M002, 2026-10-30. Input for M011 (mix-wing adapter) and M012 (mix-sim).

Sources: the sister repo `../wing-simulator` (`docs/research/protocol.md`, `wing-edit-schema.md`, `rta.md`,
`signal-flow.md`; `crates/wing-proto`, `crates/wing-model`, `crates/wing-engine/src/mix/logic.rs`,
`apps/wing-sim`, `apps/wing-render`), the Behringer/Maillot *WING Remote Protocols* PDF (as summarised in
protocol.md), and live probes against `wing-sim 0.3.0` (commit 957b406) on this Mac.

Confidence tags: **[SIM]** verified against a running wing-sim with the scripts in `tools/wing-probe/`;
**[DOC]** Remote Protocols PDF / protocol.md; **[SRC]** read in wing-simulator source; **[I]** inference, verify
on a real console.

---

## 0. Recommendation (summary)

| Need | Use | Why |
|---|---|---|
| Discovery | UDP 2222 `WING?` (`wing_proto::discovery`) | Only mechanism; reply carries ip/name/model/fw [DOC][SIM] |
| Parameter writes | **Native TCP 2222** (`D7 <hash> <value>`), OSC 2223 acceptable | Both work for every class [SIM]. Native shares one connection with events + meter control |
| Parameter reads / initial sync | **Native** `D7 <hash> DC` per leaf, or OSC `,s "*"` node dump for bulk text | OSC node dump = one packet per subtree, handy for topology; native for typed values |
| Change subscription (other clients / console surface moved something) | **Native TCP** | Every TCP client gets change events unsolicited, up to 16–24 clients. OSC has **one** event subscriber per console (last `/*s|/*S|/*b` wins, 10 s renew) and would fight Companion / TouchOSC [DOC][SIM] |
| Meters (levels, GR, gate/dyn state) | **Native only**: request on TCP channel 3, data via UDP | OSC has no meter transport [DOC][SRC] |
| RTA | **Native only** (`AA` meter block) + OSC/native writes of `cfg/rta/rtasrc|rtatap` | One console-wide RTA; selecting its source is a parameter write [SIM][SRC] |
| Solo / monitor | Write `*/$solo`, `$stat/solo`, `cfg/solo/*`, `cfg/mon/N/src` (either protocol) | Plain parameters [SIM] |
| Topology (names, colours, tags/DCA, routing) | Native reads of known paths (or OSC `*` dumps); schema from `wing-model` | Fixed tree, no dynamic node discovery needed |

**Decision: mix-wing speaks the native protocol for everything after discovery** (one TCP connection: get/set/
events/keepalive, channel 3 meter control, one UDP meter socket), using `wing_proto::nrp` (codec: `NrpEncoder`,
`NrpDecoder`, `Token`, `MeterToken`, `MeterPacket`) and `wing_model` (`Schema` / `ParamStore::lookup`, `wing_hash`)
for path → hash, model-slot names, ranges and value laws. OSC is kept as a debug/fallback path (and is what
`tools/wing-probe` uses for human-readable dumps). Rationale: meters force native anyway; native change events
are multi-client; OSC's single-subscriber rule is a real conflict on stages that run Companion.

Caveat the adapter must handle: native ids are **slot-positional** for model-dependent blocks (`ch/1/eq/8` is
STD band 2 gain `2g`; the same id is another parameter in SOUL). Resolve names through the active model
(`ParamStore::active_model`, `wing-tree.json` `variants`) and re-resolve after any `mdl` change event.

---

## 1. Transport facts a client needs

| Item | Behaviour | Tag |
|---|---|---|
| Native connect | TCP 2222, send `DF D1` (select channel 1); no banner | [DOC][SIM] |
| Keepalive | Console drops a client after 10 s silence; send `DF D1` every ~7 s (libwing) | [DOC] (sim: `--idle-timeout`, default 10 s) |
| Clients | 16 (fw 3.0.5) / 24 (fw 3.1.2) TCP clients | [DOC] |
| Own-write echo | A native `set` is **not** echoed to the setter; other native clients and the OSC subscriber get it. `D8` toggle **is** answered to the toggler | [SIM] |
| Shadow nodes | Changes of `mute`/`fdr` are followed by `$mute`/`$fdr` events (`$mute` 0/1/2 = off/own/via group-DCA) | [DOC][SIM] |
| OSC | UDP 2223, messages only, no bundles; get = address without args; set `,f` (engineering), `,i` (int/enum index), `,s` (token/text); sets not acknowledged, errors as `/* ,s "VALUE ERROR"` etc. | [DOC][SIM] |
| OSC events | `/*S` (native-typed), `/*s` (text triplet), `/*b` (native blob); renew < 10 s; **one subscriber** | [DOC][SIM] |
| OSC event addresses | positional for model slots: a `/ch/2/eq/lg` set is reported as `/ch/2/eq/1` | [SIM] |
| Firmware gate | WING-Edit refuses fw < 3.1; the tree served is the 3.3.4 one (unified gate/dyn model lists) | [SRC] |

### Value encodings

| Kind (schema `type`) | Native set | Native/OSC read | Notes |
|---|---|---|---|
| fader (`-144..10 dB`, 1024 steps) | `D5 f32` dB | f32 dB; `-144` = −∞ (`"-oo"`) | Position law in wing-edit-schema.md ("Fader law") |
| linf / logf (dB, Hz, ms, %, Q) | `D5 f32` engineering | f32, **quantised to the step grid** (80 Hz → 80.196, 350 Hz → 350.57) | Read back after write; compare in steps, not exact |
| `D6 f32` | normalised 0..1 position | — | avoid; use engineering units |
| int / bool | `00..3F` single byte, else `D3 i16` / `D4 i32` | int | |
| enum (list) | string token (`80+len`…) **or** int index | token string | Tokens: `PEQ`, `SHV`, `POST`, `FX1`, `BUS.1`, ratio `"4.0"` [SIM] |
| fenum (float list, e.g. `dyn ratio`) | token string or index | token | |
| string | `80+len` / `D1` | string | names max 16, tags max 80 |

OSC get replies: float leaf `,sff "<text>" <norm> <value>`, int leaf `,sfi`, enum/string `,s`.

---

## 2. Address tables per parameter class

`<s>` = strip: `ch/1..40`, `aux/1..8`, `bus/1..16`, `main/1..4`, `mtx/1..8`. Full dumps with ranges and defaults:
`tools/wing-probe/captures/tree_*.txt` (generated with `dump_tree.py`). Hashes = `wing_hash(path)`; positional
paths shown where the name depends on the model.

### 2.1 Input stage
| Param | Path | Range / enc |
|---|---|---|
| Input trim (digital) | `ch/N/in/set/trim` (also aux/bus/main/mtx `in/set/trim`) | −18..+18 dB, 0.1 dB |
| Polarity | `ch/N/in/set/inv` | 0/1 |
| Balance (stereo) | `ch/N/in/set/bal` | −9..+9 dB |
| Input delay | `ch/N/in/set/dlyon`, `dly`, `dlymode` | |
| Source patch | `ch/N/in/conn/grp` (`LCL, AUX, A, B, C, SC, USB, …`), `ch/N/in/conn/in` 1..64 | routing: **forbidden** in remit |
| Preamp gain (analogue) | `io/in/<grp>/<n>/g` (LCL: −2.5..45 dB, 2.5 dB steps), `vph` phantom; strip mirror `ch/N/in/set/$g` read-only | physical; remit conditional/forbidden (affects every consumer of the input) |

### 2.2 Filter
`ch/N/flt/`: `lc` 0/1, `lcf` 20..2000 Hz log, `lcs` {6,12,18,24}; `hc`, `hcf` 50..20k, `hcs` {6,12}; tool filter
`tf` 0/1 + `mdl` {TILT, MAX, AP1, AP2} with slot params `flt/1..` (TILT: `tilt` ±6 dB). No filter on aux/bus/main.

### 2.3 Gate (ch only; unified dynamics block)
`ch/N/gate/`: `on`, `mdl` (32 models: GATE, DUCK, E88, …, COMP, EXP, …), `mix` %, `gain` dB, slots `1..19`.
GATE model: `thr`=1 (−80..0 dB), `range`=2 (3..60), `att`=3 ms, `hld`=4, `rel`=5, `acc`=6, `ratio`=7.
Key: `ch/N/gatesc/{type,f,q,src,tap,$solo}`.

### 2.4 EQ
`<s>/eq/`: `on`, `mdl` (ch: STD SOUL E88 E84 F110 PULSAR MACH4; bus/main/mtx: … PIA), `mix` %, `$solo`,
`$solobd` (band listen). STD on ch (slots): `lg lf lq leq`(1–4), `1g 1f 1q`(5–7) … `4g 4f 4q`(14–16),
`hg hf hq heq`(17–20); gains ±15 dB, f 20..20k log (961 steps), q 0.44..10 log, `leq/heq` {PEQ, SHV}.
Bus/main/mtx STD has 6 bands + `tilt` (27 slots). Channel pre-EQ: `ch/N/peq/{on,1g,1f,1q,…3q}`.

### 2.5 Dynamics
`<s>/dyn/`: `on`, `mdl` (same 32-model list), `mix`, `gain` (make-up −6..12). COMP: `thr`=1 (−60..0), `ratio`=2
(fenum 1.1..100), `knee`=3, `det`=4 {PEAK,RMS}, `att`=5, `hld`=6, `rel`=7, `env`=8, `auto`=9.
Crossover `<s>/dynxo/{type,f,q,depth,$solo}`, key `<s>/dynsc/{type,f,q,src,tap,$solo}`.
Block order: `ch/N/proc` (GEDI…), tap for PRE sends `ch/N/ptap`.

### 2.6 Inserts and FX slots
| Param | Path |
|---|---|
| Insert 1 | `<s>/preins/{on, ins}` (`NONE, FX1..FX16`), status `$stat` {-, OK, N/A} |
| Insert 2 | `<s>/postins/{on, ins, mode (FX/AUTO_X/AUTO_Y), w}` |
| FX slot | `fx/K/mdl` (fx 1–8 full 65 models, 9–16 lite 39), `fx/K/fxmix` %, slots `fx/K/1..40` (HALL: `pdel size dcy mult damp lc hc shp sprd diff mspd`) |
| FX host (read-only) | `fx/K/$a_chn` (owning strip, rtasrc numbering), `$a_pos` (0 Ins1, 1 Ins2), `$esrc` |
WING has no FX sends: a "send effect" is an FX inserted on a bus fed by sends [SRC signal-flow].

### 2.7 Sends, pan, fader, mute
| Param | Path | Range |
|---|---|---|
| Bus send | `ch|aux/N/send/1..16/{on, lvl, pon, mode, plink, pan}` | lvl fader dB; mode {PRE, POST, GRP}; pan ±100 |
| Matrix send | `<s>/send/MX1..MX8/…` (bus/main) | |
| Main send | `<s>/main/1..4/{on, lvl, pre}` | |
| Fader / mute / pan / width | `<s>/fdr`, `<s>/mute`, `<s>/pan`, `<s>/wid` | fdr −144..10; pan ±100; wid −150..150 % |
| DCA | `dca/1..16/{fdr, mute, name, col, $solo, $sololed}`; membership in strip `tags` `#D<n>`; mute groups `#M<n>` (`mgrp/1..8/mute`) | |
| Bus | `bus/N/{fdr, mute, pan, wid, busmono, eq/*, dyn/*, preins, postins, dly/*, main/*, send/MX*}` | |
| Main / matrix | `main/N/…`, `mtx/N/…` (same shape; mtx has `dir.1/2` direct inputs) | |

### 2.8 Solo and monitor
| Param | Path | Semantics |
|---|---|---|
| Strip solo | `<s>/$solo` 0/1 (also `dca/N/$solo`) | writable by clients despite the `$` [SIM] |
| Solo indicator | `<s>/$sololed` 0 off / 1 own / 2 via DCA | console-owned [SIM] |
| Any solo active | `$stat/solo` | writing 0 **clears every solo** [SIM] |
| Solo config | `cfg/solo/{mode LIVE|STUDIO|SIP, mon PH|SPK|PH+SPK, chtap/bustap/maintap/mtxtap PFL|AFL, srcsolo}` | |
| Key / EQ-band / XO listen | `ch/N/gatesc/$solo`, `<s>/dynsc/$solo`, `<s>/eq/$solo`+`$solobd`, `<s>/dynxo/$solo` | |
| Solo safe | `<s>/solosafe` | exempt from SIP muting |
| Monitor A/B | `cfg/mon/1|2/{src (OFF, MAIN.n, MTX.n, BUS.n, AUX.n), srclvl, srcmix, dim, pfldim, eq/*, lim, dly}`; `lvl` = PHONES/SPEAKERS knob | |

### 2.9 Topology
| Info | Path |
|---|---|
| Names / colours / icons | `<s>/name` (≤16), `<s>/col` 1..18, `<s>/icon`, `<s>/tags` (≤80: `#Dn`, `#Mn`, user tags) |
| Input source of a channel | `ch/N/in/conn/{grp,in}` + `io/in/<grp>/<n>/{name, g, vph, mode}` |
| Output patch | `io/out/<GRP>/<n>/{grp, in}`, GRP ∈ `LCL AUX A B C SC USB CRD MOD REC AES`; e.g. `io/out/LCL/3 = {BUS, 5}` = local out 3 carries bus 5 (leg numbering per routing tables, signal-flow §12) |
| Bus stereo/mono | `bus/N/busmono`; stereo linking `clink` (ch), `cfg/mainlink` |
| Send style per bus | `ch/*/send/<b>/mode` (PRE = typical monitor mix) |
| Console identity | discovery reply / OSC `/?` |

**Monitor-bus detection heuristic for M004/M011** [I]: score a bus as monitor (IEM/wedge) when (a) its name/tags
match `IEM|MON|WEDGE|WDG|EAR|IEM\d|MIX\d`; (b) most channel sends to it are `mode PRE` (or `pre` taps); (c) it is
patched to a physical output (`io/out/{LCL,AUX,A,B,C,SC,CRD,MOD,AES}/*` with `grp=BUS`, `in` = its leg) and **not**
assigned to a main (`bus/N/main/*/on = 0`); (d) not a GRP subgroup. FX buses: a bus with `preins|postins` holding a
reverb/delay FX and POST sends. Matrices are always forbidden. The user confirms the classification.

### 2.10 Status
`$stat/{solo, sip, lock, time, usbstate, …}`; `$ctl/$stat/selidx` (WING-Edit/console selected strip) — read only
for display; never drive the surface's selection.

---

## 3. Meters and RTA (native only)

Request on TCP channel 3, data as UDP to the client's port, ~every 50 ms (20 packets/s [SIM]):

```
DF D3  D3 <udp port u16>  D4 <report id u32> DC  <type> <idx0> <idx1> … [<type> …]  DE   then DF D1
renew: DF D3 D4 <id>  (within 5 s; libwing every 3 s)
UDP:   <id u32> { int16 BE }*   (blocks in request order)
```

| Type token | Instances | Words per instance |
|---|---|---|
| `A0` ch / `A1` aux / `A2` bus / `A3` main / `A4` mtx (V1) | idx 0-based | 8: in L/R, out L/R, gate key, gate gain, dyn key, dyn gain |
| `AB` ch / `AC` aux / `AD` bus / `AE` main / `AF` mtx (V2) | idx | 11: in L/R, out L/R, gate key, gate gain, **gate LED 0/1**, dyn key, dyn gain, **dyn state 0/1**, automix gain |
| `A5` dca | idx | 4: pre L/R, post L/R |
| `A6` fx | idx | 10: in L/R, out L/R, 6 model state words |
| `A7` source groups / `A8` output groups | group idx | n per group |
| `A9` monitor | **no index** | 6: solo L/R, mon1 L/R, mon2 L/R |
| `AA` RTA | **no index** | 120 semitone bands E0 (20.6 Hz)…D#10; A4 = idx 53 |

Scaling: level words = dBFS × 256 (−128 dB floor). Gate/dyn gain words = GR-meter *position* × 256 under the
**selected model's law** (60 dB full scale GATE/DUCK, 20 dB most compressors, some negative/VU); decode with the
model read from `<s>/gate|dyn/mdl` (`wing_blocks::meter::DynMeterLaw`). RTA words = dB × 128 (doubled when
`cfg/rta/rtadet` ≠ PEAK) [SRC rta.md, M].

What the mix engine gets per strip: **input** level (after input stage), **output** level (channel: AFL = post
fader/mute, pre pan; bus/main/mtx: POST), gate and dyn GR + open/active flags (V2), key levels. There is **no
pre-fader word**: derive PFL as out − fader (when unmuted), solo the strip and read `A9` solo L/R with
`chtap = PFL`, or point the RTA at `PREFDR`.

**RTA**: one per console. Select with `cfg/rta/rtasrc` (1..76; [I] likely ch 1–40, aux 41–48, bus 49–64, main 65–68, mtx
69–76) and `cfg/rta/rtatap` (IN, POST, FILT, PREEQ, POSTEQ, PREFDR, GATEK, DYNK, DYNXO, PRETAP, SOLO, MON.PH,
MON.SPK, FXIN, FXOUT); the console mirrors them into read-only `$src $tap $dec $det`. Writing `rtasrc` steals the
RTA from WING-Edit's display (it shows "RTA in use…"), so mix-wing should restore the prior `rtasrc/rtatap`
after use, like solo. For our own analysis the captured audio (M021) is the primary spectrum source; the console
RTA is a convenience (per-strip spectrum without soloing, 50 ms, semitone resolution).

Lifetime [SIM]: a report streams 5 s after the last `D4`; **renewing an expired id does not restart it** — re-send
the full definition. One report per TCP client (a new definition supersedes the old, T086); request everything
the engine needs in one definition (e.g. 40 ch V2 + 16 bus V2 + 4 main V2 + A9 + AA ≈ 836 words = 1.7 KB/packet).

Simulator caveat: a headless wing-sim without the HAL driver sends **synthetic** meters (plausible noise); real
meters/GR/RTA need an audio session (driver, or a dummy driver, §6).

---

## 4. Solo semantics (what the actuator must do)

Verified [SIM] and from `wing_engine::mix::logic` [SRC]:
- Remote `$solo` writes are **additive** whatever `$ctl/cfg/soloexcl` says (exclusive solo applies only to the
  console's own buttons). To inspect one channel exclusively: remember the set of soloed strips (`*/$solo`,
  `dca/*/$solo`, key/band listens), write `$stat/solo = 0` (clears all), then `ch/N/$solo = 1`; on exit clear and
  re-apply the remembered set exactly. Solo is never stored in scenes.
- Tap per strip type from `cfg/solo/{chtap,bustap,maintap,mtxtap}` (PFL/AFL); destination monitor(s) from
  `cfg/solo/mon` (PH = mon 1, SPK = mon 2). The solo bus replaces the monitor source while any solo is active.
- `LIVE`/`STUDIO`: the mix (mains) is untouched. `SIP` mutes non-soloed strips **on the mains** — the actuator
  must refuse to solo (or temporarily switch mode only with user consent) when `cfg/solo/mode = SIP` during a show.
  Never change `cfg/solo/*` or `cfg/mon/*` automatically (remit: forbidden).
- What we analyse while soloed: if our capture input is the monitor output (or `A9` solo meters / RTA tap SOLO),
  soloing isolates the strip. If capture is the main mix (USB 1/2 = MAIN.1), solo changes nothing audible there —
  use per-channel USB direct outs or the RTA instead. M021/M040 must know which tap the capture input carries.
- Not modelled in the sim: `$stat/sip`, `$presolo`, `cfg/solo/mute`, `$flip`; SIP mutes are internal (not shown in
  `$mute`) [SIM][SRC].

---

## 5. Captured round trips (acceptance evidence)

`tools/wing-probe/roundtrip.py` writes each case via OSC (by name), reads it back via OSC and native (positional
hash), checks a second native client sees the change event, then writes via native and reads via OSC. Result
against wing-sim 0.3.0: **61/61 OK**, covering trim, polarity, preamp gain, filter, gate, EQ, dyn, inserts, FX
slot, bus/main sends, fader, pan/width, mute, bus/main/mtx/DCA, solo + solo tap, monitor source, RTA source,
names/colours/tags, output patch. Capture: `tools/wing-probe/captures/roundtrip-wing-sim-0.3.0.txt`.
Meters: `tools/wing-probe/meters.py` (V2 ch + monitor + RTA; synthetic in headless sim), capture
`captures/meters-wing-sim-0.3.0-synthetic.txt`.

Reproduce (never against the user's running daemon on 2222/2223 or a live console):

```
target=/Users/sam/code/wing-simulator/target/release/wing-sim
$target --name PROBESIM --port 32222 --no-discovery --osc-port 32223 --no-hostio \
        --shm-name /wingmix-probe-none --idle-timeout 0 --no-autosave --state-dir <tmp> &
python3 tools/wing-probe/roundtrip.py --osc-port 32223 --native-port 32222
python3 tools/wing-probe/meters.py --native-port 32222 --channels 1,2
python3 tools/wing-probe/dump_tree.py 127.0.0.1 32223 /ch/1      # address/range dumps
```

`--shm-name` pointing at a non-existent segment keeps the daemon off the real driver (the user's own wing-sim owns
it) while still running the control-side solo/DCA logic (`--no-audio` disables that logic: `$sololed`,
`$stat/solo` stay 0 — use the bogus shm name instead).

---

## 6. How mix-sim should drive wing-engine offline (M012)

**Use the daemon's own console in-process** (bit-identical to wing-sim, `apps/wing-render/tests/determinism.rs`).
Dependencies (git, pinned rev): `wing-model`, `wing-engine`, `wing-sim` (lib `wing_sim`), optionally `wing-render`
(lib `wing_render`, for `load_scene`/input plans/WAV/LUFS helpers).

```rust
use std::sync::Arc;
use wing_model::{ParamStore, Snapshot, Value};
use wing_sim::audio::{prepare, ConsoleControl};
use wing_sim::audio::console::{Console, ConsoleConfig};
use wing_sim::audio::driver::SESSION_WARM_UP_CYCLES;
use wing_sim::audio::stats::AudioStats;
use wing_engine::meters::{LevelBank, strip_cell, word};
use wing_engine::rta::RtaFeed;          // optional

// 1. scene
let mut store = ParamStore::new();
Snapshot::load(&store, &snap_text)?;            // or wing_render::load_scene(Some(path))
// 2. control logic (routing tables, DCA/mute groups/solo, monitor knobs) — KEEP `ctl`
let mut ctl: ConsoleControl = prepare(&mut store);
ctl.refresh(&store);
// 3. console with meters
let bank = LevelBank::shared();
let cfg = ConsoleConfig { sample_rate: 48_000, block: 64, workers: 0 /* deterministic, or N */,
    rt_workers: false, workgroup: None, meters: Some(bank.clone()), rta: None, probe: None };
let mut console = Console::build(cfg, &store, &mut ctl, None, Vec::new(), Arc::new(AudioStats::default()))?;
console.warm_up(SESSION_WARM_UP_CYCLES);
// 4. loop: apply actions between blocks, render, read taps + meters
let id = store.lookup("ch/3/eq/8").unwrap();    // or by name: lookup resolves model names
store.set(id, Value::Float(-3.0))?;              // set_text/set_enum_name for tokens
ctl.refresh(&store);                             // needed for routing, solo, DCA, mute groups
console.cycle(|ch, buf| buf.copy_from_slice(&usb_in[ch][off..off + 64]), |_, _| {});
let main_l = console.port(console.nodes().tap_port(main1_left).unwrap());   // TapChannel from
                                                 // wing_sim::record::parse_tap_groups("main/1,mon/1")
let gr = bank.take(strip_cell(strip_idx, word::DYN_GR)); // linear peak / GR dB since last take
```

Notes:
- Inputs: USB In 1..48 feed the strips only if the scene patches them (`ch/N/in/conn/grp=USB`, `in=N`); fixtures
  must set that (the factory/`PUBLIC.snap` scene patches LCL). Parameter changes reach the RT side through the
  store's change queue on the next `cycle` (strip params smoothed, models swap click-free).
- **Do not use `wing_render::build_console`/`render` for closed loop**: it drops `ConsoleControl`, so solo, DCA,
  mute-group and routing changes made after the build are never published. It is fine for one-shot renders
  (`render(store, &plan, &RenderConfig{taps, …}, |_| {})`, or the `wing-render` CLI with `--set path=value`).
  Proposed upstream follow-up: `build_console_with` returning the `ConsoleControl`.
- Taps available: `main/1..4`, `bus/1..16`, `matrix/1..8`, `mon/1..2` (incl. solo), `usb_out/n`; `LevelBank` cells
  per strip: `IN_L/R`, `PRE_L/R` (PFL), `OUT_L/R`, `GATE_KEY/GR`, `DYN_KEY/GR`, plus solo, FX and source/output
  legs (`wing_engine::meters`). In-process meters are therefore richer than the wire (PFL available).
- Speed [SIM]: `wing-render` rendered 16 ch × 10 s through `PUBLIC.snap` to main/1 + mon/1 in 0.79 s (12.6×
  realtime, 8 threads) — M012's "< 2 s" target is met by the existing engine; with `workers: 0` expect ~2–3×
  slower but deterministic scheduling.
- Solo offline works: the same render with `ch/3/$solo=1` produced signal on `mon_1` [SIM].
- A `SimSurface` should translate generic ParamIds to wing paths with the **same mapping code as mix-wing**
  (shared `wing_model` path layer), so conformance tests cover the mapping too.

### Other ways to get audio out without the HAL driver
| Option | Status | Use |
|---|---|---|
| In-process `Console` (above) | works today | M012 / `cargo test` closed loop |
| `wing-render` CLI (`--scene --inputs --taps --set`) | works today | fixtures, golden files, quick manual checks |
| Live daemon + **dummy driver**: create a private shm (`wing_shm::Segment::open(name, OpenOptions{create:true,..})`) and clock it with `wing_shm::sim::DriverSim` (write USB inputs, read USB outputs), start wing-sim with `--shm-name <name> --no-hostio` | not built; all pieces exist [SRC] | M011/M052 end-to-end over the real protocol with **real** meters/GR/RTA, no driver install |
| Real HAL driver + "Wing Simulator" CoreAudio device | user's machine only | manual integration (M052), human action |
| Daemon recorder `/wingsim/record/start ,ss <taps> <dir>` | works with an audio session | capture taps of a live daemon to WAV |

---

## 7. Gaps, risks, blockers

- No blockers for M011/M012.
- Real-console unknowns [I]: exact behaviour of `$solo` writes from remote clients on hardware (sim follows the
  user's observation: additive), whether hardware echoes sets, meter UDP source port, SIP indicators. Verify with
  `roundtrip.py` against real WING hardware (human action, on a non-show scene: the script writes ch 1, bus 1,
  main 1, fx 1, io/out/LCL/1).
- Model-slot ids: after a `mdl` change, previously cached slot meanings are invalid; the adapter must listen for
  `*/mdl` events and re-map.
- Synthetic meters in a headless sim: integration tests asserting real meter values need the dummy driver
  (follow-up) or the in-process console.
- OSC single subscriber: if mix-wing used `/*S` it would silently steal (or lose) Companion's feed — another reason
  for native.

## Proposed follow-ups
- M011: native client on `wing_proto::nrp` with keepalive, meter renew/redefine, model-aware path map; dummy-driver
  harness (`DriverSim`) for real-audio integration tests.
- Upstream (wing-simulator, by its owner): expose `ConsoleControl` from `wing_render::build_console_with`.

## Sources
- wing-simulator docs/research: protocol.md, wing-edit-schema.md, rta.md, signal-flow.md (2026-10-25..27)
- *WING Remote Protocols* V3.0.6-30 (Maillot/Behringer):
  https://cdn.mediavalet.com/aunsw/musictribe/M7u1XZvRGkWxJAVwCKE5BA/7qQ-l43rn0SEnqyb5ZcETg/Original/WING%20Remote%20Protocols_V3.0.6-30.pdf
- behringer.world forum on OSC single-subscriber / meter renew: https://behringer.world/viewtopic.php?p=12397,
  https://www.behringer.world/viewtopic.php?p=14622
- Live probes: `tools/wing-probe/captures/` (this task).

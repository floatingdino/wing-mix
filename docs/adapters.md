# How to add a console

An adapter makes one console look like a [`mix_surface::Surface`](../crates/mix-surface/src/surface.rs): generic strips, generic
`ParamId`s with specs, read/write, change events, optionally solo, meters and RTA. The engine, planner, actuator and
remit policy only ever see that model. `crates/mix-x32` (Behringer X32/M32 over OSC) is the worked example below;
`crates/mix-wing` + `crates/mix-wing-map` is the full reference adapter.

## 1. Checklist

1. **New crate** `crates/mix-<console>` depending on `mix-core` and `mix-surface` (add it to the workspace members).
   Keep the native-protocol code (codec, transport) and the *mapping* (address <-> `ParamId`, value laws) in separate
   modules; WING splits the mapping into `mix-wing-map` because `mix-sim` reuses it, you only need that if a second
   consumer exists.
2. **Strips** (`Topology`): one `Strip` per native strip with a `StripKind` (`input`, `aux`, `bus`, `main`, `matrix`,
   `dca`, `fx`, `fx_return`), `StripId` = `<kind>.<n>` (1-based), name, tags, `chain` (the blocks the strip has, in signal
   order; this is also its capability list), `sends`, `inserts`, `outputs`. Only model what you can read and write.
3. **Params**: every readable/writable thing gets a `ParamId` `<strip>/<block>[/<sub>]/<leaf>` and a `ParamSpec` with a
   `ParamClass`. The class is what the remit decides on (never the leaf name), so classify conservatively. Reuse the
   existing leaf vocabulary (`fader`, `mute`, `pan`, `in/trim`, `filter/lowcut.{on,freq}`, `eq/on`,
   `eq/band.N.{gain,freq,q}`, `send/<dest>/{level,on}`): the planner (`mix-engine::plan::state`) finds params by those
   names.
4. **Values**: convert to engineering units (dB, Hz, Q, -100..100 pan). `write` must validate (`ParamSpec::coerce`), quantise
   to what the console stores and *return the stored value*; reject unknown ids, read-only params, wrong types and
   out-of-range values before anything is sent.
5. **Events**: `subscribe()` delivers changes made by anyone else as `Origin::External`; own writes are not delivered (or
   `Origin::Local`). Names/routing/send changes -> `TopologyChanged`.
6. **Capabilities**: report only what exists (`Capabilities::default()` = no solo, meters or RTA). Checks for absent
   capabilities are skipped by the conformance suite.
7. **Remit policy** `data/remit/<console>-default.toml` (format `wing-mix-remit/1`, docs/research/remit.md), with
   `surface = "<info().surface>"`. Copy the WING file, delete rules for classes your adapter never produces, adapt the
   detection vocabulary and locks.
8. **Tests**: run the shared conformance suite against a fake of the console's protocol (below), and run role detection +
   policy evaluation + `PolicyGate::new` over your topology.
9. Document anything the generic model could not express in the crate docs and in the "Assumptions" table here.

## 2. Test setup: fake console + conformance

```rust
let fake = FakeX32::start();                       // in-process UDP server, written from the protocol docs
let s = X32Surface::connect(fake.addr(), &opts)?;
struct Driver<'a> { fake: &'a FakeX32, surface: &'a X32Surface }
impl ConformanceDriver for Driver<'_> {            // plays "another client / the console surface"
    fn external_write(&self, p: &ParamId, v: Value) -> bool {
        let (addr, arg) = self.surface.native_of(p, &v).unwrap();
        self.fake.console_set(&addr, arg)
    }
}
conformance::run_all(&s, &driver, &ConformanceOptions::default());
```

`run_all` checks topology integrity, spec/read consistency for every param, write round trip + quantisation + no own-write
`External` event, rejections without side effects, solo (if `capabilities.solo`), meters/RTA (if present), external events
and model remaps (if `model_probe` is set). The default write probe is the first writable float fader/trim/eq/pan param of
the first strip with a numeric range, so list a param with a *stepped* grid first (X32: `in/trim`), or set
`ConformanceOptions::write_probe`. The driver's `external_write` is the only console-side hook; return `false` to skip.

Write the fake from the protocol documentation, not from your adapter's tables (the X32 fake derives defaults from address
*patterns* and models `/xremote` expiry, UDP loss and echo behaviour), and add a handful of tests with literal native
addresses/values so a wrong mapping cannot hide behind a symmetric encode/decode.

## 3. Worked example: Behringer X32/M32 (`crates/mix-x32`)

Protocol: plain OSC over UDP, console port 10023 (unofficial protocol document by P.-G. Maillot; addresses were checked
against public X32 tooling docs, value laws are from that document and are **not hardware-verified**, see Follow-ups).
Query = address with no arguments, the console answers with the value; set = address with one argument; `/xremote` makes the
console push other clients' changes for ~10 s (renewed every 8 s); `/info` identifies the console.

| generic | X32 | value law |
|---|---|---|
| `input.N` (1-32) | `/ch/NN` | |
| `bus.N` (1-16) | `/bus/NN` | |
| `main.1` / `main.2` | `/main/st` / `/main/m` | mono has no pan |
| `<s>/fader` | `<base>/mix/fader` | float 0..1, 1024 steps, 4-segment law, 0 dB = 0.75 |
| `<s>/mute` | `<base>/mix/on` | int, **inverted** (1 = on = not muted) |
| `<s>/pan` | `<base>/mix/pan` | 0..1 -> -100..100, 201 steps |
| `input.N/in/trim` | `/ch/NN/preamp/trim` | -18..18 dB, 0.25 dB steps |
| `input.N/filter/lowcut.{on,freq}` | `/ch/NN/preamp/{hpon,hpf}` | 20..400 Hz log, 101 steps |
| `<s>/eq/on`, `eq/band.K.{freq,gain,q,type}` | `<base>/eq/on`, `<base>/eq/K/{f,g,q,type}` | 20..20k Hz log (201), +-15 dB (121), Q 10..0.3 log (72), type int 0..5 |
| `input.N/send/bus.M/{level,on}` | `/ch/NN/mix/MM/{level,on}` | send-level law, 161 steps |
| `<s>/send/main.{1,2}/on` | `<base>/mix/{st,mono}` | main assignment |
| send tap (topology only) | `/ch/NN/mix/MM/type` | 3 = pre, 4 = post, 5 = group |
| name | `<base>/config/name` | `#WORD` tokens become strip tags (`#NOAI`/`#AI`) |

Adapter shape: one UDP socket + one thread that updates a mirror (address -> last argument), emits events for *changed*
values only (so echoes of own writes are silent, whether or not the console echoes them), and renews `/xremote`. `connect`
requires `/info`, then primes the mirror with paced queries and retries (tested with dropped datagrams). Not implemented:
solo, meters (`/meters`), RTA, gate/dynamics/insert/FX/DCA/matrix/aux-in strips, routing; those capabilities are simply
absent. Run `cargo test -p mix-x32`.

## 4. Assumptions in the generic stack found while writing the X32 adapter

A *surface* assumption is something `mix-surface` or a consumer expects of every adapter without the trait saying so.

| # | where | assumption | handled how |
|---|---|---|---|
| A1 | `mix-engine::control` (`remit_context`) and `mix-actuator::gate::PolicyGate::build` | `surface.solo_state()?` is called unconditionally, so a surface without solo makes the gate fail closed (every write forbidden) | adapter returns "non-destructive, nothing active" from `solo_state()` and `Ok` from `clear_solo()`; `set_solo` returns `Unsupported`. Proper fix: consult `capabilities.solo` (follow-up F1) |
| A2 | `SendTap` has `Pre/Post/Group` only | a send whose tap cannot be determined must be reported as something | adapter reports `Pre` (fail safe: looks like a monitor feed, bus stays locked until the user confirms its role) (F2) |
| A3 | `mix-engine::plan::state::StripSettings` | EQ = fixed low shelf (`eq/low.*`) + peaking `eq/band.N.*` + fixed high shelf (`eq/high.*`), every `band.N` is PEQ; WING `STD` Q convention and EQ designs (`WingTransfer`) | X32 any band can be shelf/cut/PEQ/VEQ (`band.K.type`); the planner would predict all of them as PEQ with WING's Q. Needs an EQ-shape-aware transfer model (F3) |
| A4 | `mix-engine::control::mod.rs`, `plan::plan` | `WingTransfer` is the only effect-prediction model wired in (the `TransferModel` trait is the seam but is not injectable from the public entry points) | none needed for fader/pan/mute; EQ/HPF predictions use WING DSP (F3) |
| A5 | `apps/wing-mixd` (`rta.rs`) | the daemon connects `mix_wing::WingSurface` concretely and drives RTA through WING-only `claim_rta`; no surface factory | a second console cannot be selected at runtime yet (F4) |
| A6 | `Policy.surface` | the field is parsed but never compared with `SurfaceInfo.surface`; callers hardcode `wing-default.toml` paths (tests, daemon) | x32 test asserts they match (F5) |
| A7 | `ParamClass` | no class for a discrete EQ band shape; `eq_model` (conditional, discrete) is the closest | X32 `band.K.type` classed `eq_model`; no `ParamsRemapped` is needed because the param set is static |
| A8 | role detection inputs | `Strip.outputs` (physical-out routing) and strip tags are expected to exist; the X32 adapter fills neither from the console (outputs empty, tags from `#WORD` in the 12-char name) | detection falls back to names and send evidence; default `MixBus NN` names carry none, so unnamed buses end up `unknown` (locked until confirmed) |
| A9 | policy format id | `wing-mix-remit/1` is the project's name, not a WING dependency; policy comments/notes mention WING | none |

Things that did **not** need WING knowledge: `Topology`/`Strip`/`StripId` grammar, `ParamSpec` + `coerce`, `Origin`
event contract, the conformance suite, `ScoringDetector` + `RemitContext` + `evaluate` (coupling and revive logic worked on
X32 sends/taps unchanged), `PolicyGate`.

## 5. Follow-ups (not done here)

- F1: `mix-engine::control` / `PolicyGate::build`: skip `solo_state()` when `!capabilities.solo` (then adapters may return `Unsupported`).
- F2: add `SendTap::Unknown` (treated as protected by detection) instead of overloading `Pre`.
- F3: EQ-shape-aware generic EQ view + a console-selectable `TransferModel` (`X32Transfer`: Behringer EQ designs).
- F4: console factory in `wing-mixd` (`--console wing|x32`), RTA/meter claim through the `Surface` trait only.
- F5: load the policy by `SurfaceInfo.surface` and refuse a mismatch.
- F6: X32: `/meters` (`/meters/1` input blob), gate/dynamics, aux-in/FX-return/matrix/DCA strips, solo (`/-stat/solo`, `/-stat/solosw/NN`),
  disconnect detection/`Reconnected` events, verification of every value law and the send `type` mapping on real hardware
  (human action; add to docs/HUMAN-ACTIONS.md when scheduled).

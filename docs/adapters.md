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
Status after M061 ("fixed" = the generic stack no longer assumes it; the X32 adapter was changed accordingly):

| # | where | assumption | status |
|---|---|---|---|
| A1 | `mix-engine::control` (`remit_context`) and `mix-actuator::gate::PolicyGate::build` | `solo_state()?` was called unconditionally, so a surface without solo failed closed | **fixed (F1)**: `Surface::solo_destructive()` consults `capabilities.solo`; `RemitContext.solo_available` forbids solo/listen inspections, the planner emits no `Inspect` steps (no "denied" note either), the actuator refuses `Job::Inspect` without calling the surface. An adapter without solo returns `Unsupported` from `solo_state`/`set_solo`/`clear_solo`. |
| A2 | `SendTap` had `Pre/Post/Group` only | a send whose tap cannot be determined had to be reported as something | **fixed (F2)**: `SendTap::Unknown`; role detection counts it like `Pre` (monitor-looking feed: the bus stays protected until the user confirms its role), the remit sees it tapped at the earliest point. |
| A3 | `mix-engine::plan::state::StripSettings` | EQ = fixed low shelf + 8 PEQs + fixed high shelf with WING's Q convention | **fixed (F3)**: band shape is data. `eq/<slot>.type` (when present) gives each band's `BandKind` (`PEQ`/`VEQ` peak, `SHV`/`LSHV`/`HSHV` shelf, `LCUT`/`HCUT` cut); consoles without per-band types (WING peak bands) are peaking. The planner finds a console's shelf on any band whose type is that shelf, else retypes the free band at the matching spectrum end (needs the `type` param to offer `LSHV`/`HSHV`); cut bands are never retuned. |
| A4 | `mix-engine::control`, `plan::plan` | `WingTransfer` was the only wired-in effect model | **fixed (F3)**: `Controller::new` picks `plan::transfer_for_surface(info.surface)`: `WingTransfer` for `wing`/`sim`, `GenericTransfer` (RBJ-cookbook peak/shelf/cut per band shape, Butterworth cuts, X32/M32 cuts 12 dB/oct; compressor/gate/pan/de-esser delegate to WING's static curves, an approximation) otherwise. `plan_with`/`Controller::with_transfer` accept any `TransferModel`; `plan()` is the WING shortcut. Not hardware-verified for the X32. |
| A5 | `apps/wing-mixd` | the daemon connected `WingSurface` concretely and drove RTA through WING-only `claim_rta` | **fixed (F4)**: the daemon holds `Arc<dyn Surface>`; `--console sim\|wing:<ip>\|x32:<ip>[:port]` (or `connect_console`); the RTA feed uses `Surface::claim_rta` (default: save `rta_source`, set, restore; WING overrides it with its exact raw restore) and only exists when `capabilities.rta` does. |
| A6 | `Policy.surface` | parsed but never compared with `SurfaceInfo.surface` | **fixed (F5)**: `Policy::check_surface(&SurfaceInfo)`; the daemon picks the embedded default policy by surface (or `--policy`) and refuses a mismatch at connect and again at session start (an emulation matches by `SurfaceInfo.model`: the sim, model `WING`, runs the `wing` policy). `PolicyGate::new` itself does not check (test surfaces have free-form names). |
| A7 | `ParamClass` | no class for a discrete EQ band shape; `eq_model` (conditional, discrete) is the closest | unchanged: X32 `band.K.type` classed `eq_model`; the planner writes it only to retype a free band into a shelf, and the remit decides whether that is allowed |
| A8 | role detection inputs | `Strip.outputs` and strip tags are expected to exist; the X32 adapter fills neither from the console | unchanged: detection falls back to names and send evidence; default `MixBus NN` names carry none, so unnamed buses end up `unknown` (locked until confirmed) |
| A9 | policy format id | `wing-mix-remit/1` is the project's name, not a WING dependency; policy comments/notes mention WING | none |

Things that did **not** need WING knowledge: `Topology`/`Strip`/`StripId` grammar, `ParamSpec` + `coerce`, `Origin`
event contract, the conformance suite, `ScoringDetector` + `RemitContext` + `evaluate` (coupling and revive logic worked on
X32 sends/taps unchanged), `PolicyGate`.

## 5. Follow-ups (not done)

- F1-F5: done in M061 (see the table above).
- F6: X32: `/meters` (`/meters/1` input blob), gate/dynamics, aux-in/FX-return/matrix/DCA strips, solo (`/-stat/solo`, `/-stat/solosw/NN`),
  disconnect detection/`Reconnected` events, verification of every value law and the send `type` mapping on real hardware
  (human action; add to docs/HUMAN-ACTIONS.md when scheduled). With solo added, set `capabilities.solo = true` and implement
  `solo_state`/`set_solo`/`clear_solo`; nothing else needs to change.
- F7: a real X32 EQ/dynamics transfer model (`GenericTransfer` is a cookbook approximation) once hardware measurements exist.

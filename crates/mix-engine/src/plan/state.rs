//! [`ConsoleState`]: the planner's snapshot of the console (param specs + current values + topology), and typed
//! per-strip views ([`StripSettings`]) of the generic params the effect model understands.
//!
//! Param names follow the generic mapping (docs/research/remit.md appendix A.1, mix-sim/mix-wing): `fader`, `pan`,
//! `filter/lowcut.{on,freq,slope}`, `filter/highcut.*`, `eq/{on,mix,model}`, `eq/{low,high}.{gain,freq,q,type}`,
//! `eq/band.N.{gain,freq,q}`, `dyn/{on,model,threshold,ratio,knee,detector,makeup,mix}`, `gate/{on,threshold,
//! range,ratio,attack,hold,release}`, `dyn/{attack,hold,release,envelope,auto}`. Strips whose EQ/dynamics model is
//! not the standard one (`STD` / `COMP`, gate slot `GATE`) are reported as unmodelled for that block, so the planner
//! never proposes moves it cannot predict; the one exception is the `DS902` de-esser, modelled in either dynamics
//! slot ([`DeEsserSettings`]).
//!
//! Model-dependent params (M035): a WING model switch re-maps the slot's params, so a snapshot taken under `GATE`
//! has no specs for the `DS902` leaves. [`ConsoleState::apply`] registers the specs of a model when a candidate
//! switches to it ([`model_specs`]), so remit checks and value coercion see the params the console will have.

use std::collections::BTreeMap;

use mix_core::action::ParamChange;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::model::{Block, ParamSpec, Strip, Topology};
use mix_surface::surface::{Surface, SurfaceResult};

/// Specs and values of every param of the strips the planner may consider, plus the topology.
#[derive(Clone, Debug, Default)]
pub struct ConsoleState {
    pub topology: Topology,
    pub specs: BTreeMap<ParamId, ParamSpec>,
    pub values: BTreeMap<ParamId, Value>,
}

impl ConsoleState {
    /// Snapshot `surface`: topology, then specs and values of every param of every strip (read-only and
    /// unreadable params are kept as specs only).
    pub fn capture(surface: &dyn Surface) -> SurfaceResult<Self> {
        let topology = surface.topology()?;
        let mut st = ConsoleState {
            topology,
            ..Default::default()
        };
        for s in &st.topology.strips {
            for spec in surface.params(&s.id)? {
                if let Ok(v) = surface.read(&spec.id) {
                    st.values.insert(spec.id.clone(), v);
                }
                st.specs.insert(spec.id.clone(), spec);
            }
        }
        Ok(st)
    }

    /// Build from parts (tests, recorded sessions).
    pub fn from_parts(topology: Topology, params: Vec<(ParamSpec, Value)>) -> Self {
        let mut st = ConsoleState {
            topology,
            ..Default::default()
        };
        for (spec, v) in params {
            st.values.insert(spec.id.clone(), v);
            st.specs.insert(spec.id.clone(), spec);
        }
        st
    }

    pub fn get(&self, id: &ParamId) -> Option<&Value> {
        self.values.get(id)
    }

    pub fn spec(&self, id: &ParamId) -> Option<&ParamSpec> {
        self.specs.get(id)
    }

    pub fn has(&self, strip: &StripId, rest: &str) -> bool {
        self.specs.contains_key(&ParamId::of(strip, rest))
    }

    pub fn f64(&self, strip: &StripId, rest: &str) -> Option<f64> {
        self.values.get(&ParamId::of(strip, rest))?.as_f64()
    }

    pub fn bool(&self, strip: &StripId, rest: &str) -> Option<bool> {
        self.values.get(&ParamId::of(strip, rest))?.as_bool()
    }

    pub fn str(&self, strip: &StripId, rest: &str) -> Option<&str> {
        self.values.get(&ParamId::of(strip, rest))?.as_str()
    }

    pub fn strip(&self, id: &StripId) -> Option<&Strip> {
        self.topology.get(id)
    }

    /// Coerce `v` to the param's spec (clamp numeric ranges, map enum numbers to the nearest option).
    pub fn coerce(&self, id: &ParamId, v: Value) -> Option<Value> {
        let spec = self.specs.get(id)?;
        let v = match (&spec.kind, v) {
            (mix_surface::model::ValueKind::Float, Value::Float(x)) => {
                let (lo, hi) = spec.range.unwrap_or((f64::MIN, f64::MAX));
                Value::Float(x.clamp(lo, hi))
            }
            (mix_surface::model::ValueKind::Int, v) => {
                let x = v.as_f64()?.round();
                let (lo, hi) = spec.range.unwrap_or((f64::MIN, f64::MAX));
                Value::Int(x.clamp(lo, hi) as i64)
            }
            (mix_surface::model::ValueKind::Enum { options }, Value::Float(x)) => {
                // numeric enums (WING ratio "4.0", slope "24"): nearest option
                let best = options
                    .iter()
                    .filter_map(|o| o.parse::<f64>().ok().map(|n| (o, n)))
                    .min_by(|a, b| (a.1 - x).abs().total_cmp(&(b.1 - x).abs()))?;
                Value::Enum(best.0.clone())
            }
            (_, v) => v,
        };
        spec.coerce(&v).ok()
    }

    /// Apply changes (later ones win); unknown params are ignored.
    pub fn apply(&mut self, changes: &[ParamChange]) {
        for c in changes {
            self.apply_one(c);
        }
    }

    /// Apply one change; a switch of a dynamics slot model the planner knows registers that model's params
    /// (with their console defaults, unless already present).
    pub fn apply_one(&mut self, c: &ParamChange) {
        if !self.specs.contains_key(&c.param) {
            return;
        }
        self.values.insert(c.param.clone(), c.to.clone());
        let (Some(strip), Some(block)) = (c.param.strip(), c.param.block()) else {
            return;
        };
        if c.param.leaf() == "model" && matches!(block, "gate" | "dyn") {
            if let Some(m) = c.to.as_str() {
                for (spec, v) in model_specs(&strip, block, m) {
                    if !self.specs.contains_key(&spec.id) {
                        self.values.insert(spec.id.clone(), v);
                        self.specs.insert(spec.id.clone(), spec);
                    }
                }
            }
        }
    }

    /// Typed settings of a strip.
    pub fn settings(&self, id: &StripId) -> StripSettings {
        StripSettings::read(self, id)
    }
}

/// Channel/bus EQ band type as the effect model understands it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandKind {
    Peq,
    Shelf,
    /// Any other type (cuts, crossovers): treated as fixed (not modelled for changes).
    Other,
}

/// One EQ band (engineering units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EqBand {
    pub gain_db: f64,
    pub freq_hz: f64,
    pub q: f64,
    pub kind: BandKind,
}

/// Generic band slots of the standard EQ.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum BandSlot {
    Low,
    Peak(u8),
    High,
}

impl BandSlot {
    /// Param prefix inside the `eq` block (`low`, `band.2`, `high`).
    pub fn prefix(self) -> String {
        match self {
            BandSlot::Low => "low".into(),
            BandSlot::Peak(n) => format!("band.{n}"),
            BandSlot::High => "high".into(),
        }
    }
}

/// The standard EQ of a strip (`None` fields = the strip has no such band).
#[derive(Clone, Debug, PartialEq)]
pub struct EqSettings {
    pub on: bool,
    /// Gain scale (%), WING `eq.mix` law: scales every PEQ/shelf gain.
    pub mix_pct: f64,
    pub bands: Vec<(BandSlot, EqBand)>,
    /// Bus/main tilt (dB), when present.
    pub tilt_db: Option<f64>,
}

impl EqSettings {
    pub fn band(&self, slot: BandSlot) -> Option<&EqBand> {
        self.bands.iter().find(|(s, _)| *s == slot).map(|(_, b)| b)
    }
}

/// Low/high cut.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CutSettings {
    pub on: bool,
    pub freq_hz: f64,
    /// dB/oct (6/12/18/24).
    pub slope: u32,
}

/// The standard compressor (`COMP` model).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DynSettings {
    pub on: bool,
    pub threshold_db: f64,
    pub ratio: f64,
    /// Knee steps 0..5.
    pub knee: u8,
    pub makeup_db: f64,
    pub mix_pct: f64,
    pub peak_detector: bool,
    /// Attack (ms): transients shorter than this pass partly uncompressed.
    pub attack_ms: f64,
    /// Hold (ms).
    pub hold_ms: f64,
    /// Release (ms).
    pub release_ms: f64,
    /// Envelope `LIN` (constant dB rate) instead of `LOG` (one-pole).
    pub lin_env: bool,
    /// WING AUTO (programme-dependent dual release; attack instant with the RMS detector).
    pub auto: bool,
}

impl DynSettings {
    /// A compressor with WING's COMP defaults for the fields a caller does not set.
    pub fn new(on: bool, threshold_db: f64, ratio: f64) -> Self {
        DynSettings {
            on,
            threshold_db,
            ratio,
            knee: 3,
            makeup_db: 0.0,
            mix_pct: 100.0,
            peak_detector: false,
            attack_ms: 50.0,
            hold_ms: 20.0,
            release_ms: 150.0,
            lin_env: false,
            auto: true,
        }
    }
}

/// The standard gate (`GATE` model) as a downward expander/gate on the detected level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateSettings {
    pub on: bool,
    pub threshold_db: f64,
    pub range_db: f64,
    /// Expansion ratio 1:N below threshold (`f64::INFINITY` = gate).
    pub ratio: f64,
    /// Attack, hold, release (ms, WING 10-90 % times).
    pub attack_ms: f64,
    pub hold_ms: f64,
    pub release_ms: f64,
}

impl GateSettings {
    /// A gate with WING's GATE defaults for the timing fields.
    pub fn new(on: bool, threshold_db: f64, range_db: f64, ratio: f64) -> Self {
        GateSettings {
            on,
            threshold_db,
            range_db,
            ratio,
            attack_ms: 10.0,
            hold_ms: 10.0,
            release_ms: 200.0,
        }
    }
}

/// Dynamics slot holding a de-esser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynSlot {
    Gate,
    Dyn,
}

impl DynSlot {
    /// Generic block name (`gate` / `dyn`).
    pub fn block(self) -> &'static str {
        match self {
            DynSlot::Gate => "gate",
            DynSlot::Dyn => "dyn",
        }
    }
}

/// The WING `DS902` de-esser (dbx 902 style: HF/total energy ratio, no threshold), in a dynamics slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DeEsserSettings {
    pub on: bool,
    pub slot: DynSlot,
    /// Detector/shelf frequency (800..8000 Hz).
    pub freq_hz: f64,
    /// Maximum reduction (0..12 dB).
    pub range_db: f64,
    /// `HF` (dynamic high shelf) or `FULL` (wide-band gain).
    pub hf_mode: bool,
    /// Wet/dry mix of the slot (%).
    pub mix_pct: f64,
}

/// Model whose slot params the planner knows without a snapshot (see [`model_specs`]).
pub const DEESSER_MODEL: &str = "DS902";

/// Specs of the params a dynamics slot (`<strip>/<block>/`) has under `model`, for models the planner proposes
/// switching to (`DS902`: `f` 800..8000 Hz, `range` 0..12 dB, `mode` FULL/HF; wing-blocks `DeEsser902`).
pub fn model_specs(strip: &StripId, block: &str, model: &str) -> Vec<(ParamSpec, Value)> {
    use mix_core::units::Unit;
    use mix_surface::ParamClass;
    if model != DEESSER_MODEL {
        return Vec::new();
    }
    let id = |leaf: &str| ParamId::of(strip, &format!("{block}/{leaf}"));
    let dep = |mut s: ParamSpec| {
        s.model_dependent = true;
        s
    };
    vec![
        (
            dep(ParamSpec::float(
                id("f"),
                ParamClass::Dynamics,
                Unit::Hz,
                800.0,
                8000.0,
            )),
            Value::Float(5000.0),
        ),
        (
            dep(ParamSpec::float(
                id("range"),
                ParamClass::Dynamics,
                Unit::Db,
                0.0,
                12.0,
            )),
            Value::Float(4.0),
        ),
        (
            dep(ParamSpec::enumeration(
                id("mode"),
                ParamClass::Dynamics,
                &["FULL", "HF"],
            )),
            Value::Enum("HF".into()),
        ),
    ]
}

/// Everything about one strip the effect model reads.
#[derive(Clone, Debug, PartialEq)]
pub struct StripSettings {
    pub id: StripId,
    /// Fader (dB); `None` when the strip has no fader param.
    pub fader_db: Option<f64>,
    pub mute: bool,
    pub pan: Option<f64>,
    pub width_pct: Option<f64>,
    pub trim_db: Option<f64>,
    pub lowcut: Option<CutSettings>,
    pub highcut: Option<CutSettings>,
    /// `None` when the strip has no EQ or a non-standard model (not modelled).
    pub eq: Option<EqSettings>,
    /// `None` when absent or not the standard compressor.
    pub dynamics: Option<DynSettings>,
    pub gate: Option<GateSettings>,
    /// A `DS902` de-esser in the gate or dynamics slot.
    pub deesser: Option<DeEsserSettings>,
    /// Blocks in signal order (topology chain).
    pub chain: Vec<Block>,
}

fn band_kind(s: Option<&str>) -> BandKind {
    match s {
        None | Some("PEQ") => BandKind::Peq,
        Some("SHV") => BandKind::Shelf,
        _ => BandKind::Other,
    }
}

impl StripSettings {
    pub fn read(st: &ConsoleState, id: &StripId) -> Self {
        let f = |rest: &str| st.f64(id, rest);
        let b = |rest: &str| st.bool(id, rest);
        let cut = |p: &str| -> Option<CutSettings> {
            Some(CutSettings {
                on: b(&format!("filter/{p}.on"))?,
                freq_hz: f(&format!("filter/{p}.freq"))?,
                slope: st
                    .str(id, &format!("filter/{p}.slope"))
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(if p == "lowcut" { 24 } else { 12 }),
            })
        };
        let eq =
            (st.has(id, "eq/on") && st.str(id, "eq/model").is_none_or(|m| m == "STD")).then(|| {
                let mut bands = Vec::new();
                let mut slots = vec![BandSlot::Low];
                slots.extend((1..=8).map(BandSlot::Peak));
                slots.push(BandSlot::High);
                for slot in slots {
                    let p = slot.prefix();
                    let (Some(g), Some(fr)) =
                        (f(&format!("eq/{p}.gain")), f(&format!("eq/{p}.freq")))
                    else {
                        continue;
                    };
                    let kind = match slot {
                        BandSlot::Peak(_) => BandKind::Peq,
                        _ => band_kind(st.str(id, &format!("eq/{p}.type"))),
                    };
                    bands.push((
                        slot,
                        EqBand {
                            gain_db: g,
                            freq_hz: fr,
                            q: f(&format!("eq/{p}.q")).unwrap_or(0.7),
                            kind,
                        },
                    ));
                }
                EqSettings {
                    on: b("eq/on").unwrap_or(false),
                    mix_pct: f("eq/mix").unwrap_or(100.0),
                    bands,
                    tilt_db: f("eq/tilt"),
                }
            });
        let dynamics = (st.has(id, "dyn/on")
            && st.str(id, "dyn/model").is_none_or(|m| m == "COMP"))
        .then(|| {
            Some(DynSettings {
                on: b("dyn/on")?,
                threshold_db: f("dyn/threshold")?,
                ratio: st
                    .str(id, "dyn/ratio")
                    .and_then(|s| s.parse().ok())
                    .or_else(|| f("dyn/ratio"))?,
                knee: f("dyn/knee").unwrap_or(0.0).clamp(0.0, 5.0) as u8,
                makeup_db: f("dyn/makeup").unwrap_or(0.0),
                mix_pct: f("dyn/mix").unwrap_or(100.0),
                peak_detector: st.str(id, "dyn/detector") == Some("PEAK"),
                attack_ms: f("dyn/attack").unwrap_or(10.0),
                hold_ms: f("dyn/hold").unwrap_or(20.0),
                release_ms: f("dyn/release").unwrap_or(150.0),
                lin_env: st.str(id, "dyn/envelope") == Some("LIN"),
                auto: b("dyn/auto").unwrap_or(false),
            })
        })
        .flatten();
        let gate = (st.has(id, "gate/on") && st.str(id, "gate/model").is_none_or(|m| m == "GATE"))
            .then(|| {
                Some(GateSettings {
                    on: b("gate/on")?,
                    threshold_db: f("gate/threshold")?,
                    range_db: f("gate/range").unwrap_or(60.0),
                    ratio: match st.str(id, "gate/ratio") {
                        Some("1:1.5") => 1.5,
                        Some("1:2") => 2.0,
                        Some("1:3") => 3.0,
                        Some("1:4") => 4.0,
                        _ => f64::INFINITY,
                    },
                    attack_ms: f("gate/attack").unwrap_or(10.0),
                    hold_ms: f("gate/hold").unwrap_or(10.0),
                    release_ms: f("gate/release").unwrap_or(200.0),
                })
            })
            .flatten();
        let deesser = [DynSlot::Gate, DynSlot::Dyn].into_iter().find_map(|slot| {
            let blk = slot.block();
            if st.str(id, &format!("{blk}/model")) != Some(DEESSER_MODEL) {
                return None;
            }
            Some(DeEsserSettings {
                on: b(&format!("{blk}/on"))?,
                slot,
                freq_hz: f(&format!("{blk}/f")).unwrap_or(5000.0),
                range_db: f(&format!("{blk}/range")).unwrap_or(4.0),
                hf_mode: st.str(id, &format!("{blk}/mode")) != Some("FULL"),
                mix_pct: f(&format!("{blk}/mix")).unwrap_or(100.0),
            })
        });
        StripSettings {
            id: id.clone(),
            fader_db: f("fader"),
            mute: b("mute").unwrap_or(false),
            pan: f("pan"),
            width_pct: f("width"),
            trim_db: f("in/trim"),
            lowcut: cut("lowcut"),
            highcut: cut("highcut"),
            eq,
            dynamics,
            gate,
            deesser,
            chain: st.strip(id).map(|s| s.chain.clone()).unwrap_or_default(),
        }
    }

    /// Effective fader gain (dB), −144 (off) when muted.
    pub fn level_db(&self) -> f64 {
        if self.mute {
            return -144.0;
        }
        self.fader_db.unwrap_or(0.0)
    }

    /// `true` when `a` comes before `b` in the chain (unknown order: `true`).
    pub fn before(&self, a: &Block, b: &Block) -> bool {
        match (
            self.chain.iter().position(|x| x == a),
            self.chain.iter().position(|x| x == b),
        ) {
            (Some(x), Some(y)) => x < y,
            _ => true,
        }
    }
}

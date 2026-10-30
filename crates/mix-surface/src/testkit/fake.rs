//! [`FakeSurface`]: an in-memory [`Surface`] with a configurable topology that records every write.

use crate::model::{
    Block, FxCategory, FxSlotInfo, InsertInfo, OutputKind, ParamClass, ParamSpec, SendInfo,
    SendTap, Strip, StripKind, Taper, Topology, ValueKind,
};
use crate::surface::{
    Capabilities, MeterCaps, MeterFrame, Origin, RtaCaps, RtaSource, SoloMode, SoloState,
    SoloTarget, StripMeters, Surface, SurfaceError, SurfaceEvent, SurfaceInfo, SurfaceResult,
};
use crate::testkit::conformance::ConformanceDriver;
use mix_core::ids::{ParamId, StripId};
use mix_core::units::Unit;
use mix_core::value::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, MutexGuard};

/// Params of one model of a model-dependent block.
pub type ModelParams = Vec<(ParamSpec, Value)>;

/// A block whose params depend on its model (slot-positional on real consoles).
#[derive(Debug, Clone)]
struct ModelBinding {
    model_param: ParamId,
    strip: StripId,
    block: String,
    variants: BTreeMap<String, ModelParams>,
}

#[derive(Debug)]
struct State {
    info: SurfaceInfo,
    topology: Topology,
    specs: BTreeMap<ParamId, ParamSpec>,
    values: BTreeMap<ParamId, Value>,
    writes: Vec<(ParamId, Value)>,
    subscribers: Vec<Sender<SurfaceEvent>>,
    meter_subscribers: Vec<Sender<MeterFrame>>,
    solo_mode: SoloMode,
    solo: BTreeSet<SoloTarget>,
    meters: MeterFrame,
    rta: Option<RtaSource>,
    models: Vec<ModelBinding>,
}

impl State {
    fn emit(&mut self, ev: SurfaceEvent) {
        self.subscribers.retain(|s| s.send(ev.clone()).is_ok());
    }
}

/// In-memory surface for tests. Writes go through the same validation a real adapter does; every accepted write is
/// recorded ([`FakeSurface::writes`]). [`FakeSurface::external_set`] simulates the engineer or another client.
#[derive(Debug)]
pub struct FakeSurface {
    state: Mutex<State>,
}

impl FakeSurface {
    /// Empty surface (add strips and params with the builder methods).
    pub fn new(name: &str) -> Self {
        FakeSurface {
            state: Mutex::new(State {
                info: SurfaceInfo {
                    surface: "fake".into(),
                    name: name.into(),
                    model: "FAKE".into(),
                    firmware: "0".into(),
                    capabilities: Capabilities::default(),
                },
                topology: Topology::default(),
                specs: BTreeMap::new(),
                values: BTreeMap::new(),
                writes: Vec::new(),
                subscribers: Vec::new(),
                meter_subscribers: Vec::new(),
                solo_mode: SoloMode::NonDestructive,
                solo: BTreeSet::new(),
                meters: MeterFrame::default(),
                rta: None,
                models: Vec::new(),
            }),
        }
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_capabilities(&self, caps: Capabilities) {
        self.st().info.capabilities = caps;
    }

    pub fn add_strip(&self, strip: Strip) {
        self.st().topology.strips.push(strip);
    }

    /// Replace the topology (e.g. the engineer renamed or re-routed something) and emit `TopologyChanged`.
    pub fn update_topology(&self, f: impl FnOnce(&mut Topology)) {
        let mut st = self.st();
        f(&mut st.topology);
        st.emit(SurfaceEvent::TopologyChanged);
    }

    pub fn add_param(&self, spec: ParamSpec, initial: Value) {
        let mut st = self.st();
        st.values.insert(spec.id.clone(), initial);
        st.specs.insert(spec.id.clone(), spec);
    }

    /// Register a model-dependent block: `model_param` (an enum whose options are the keys of `variants`) selects
    /// which params exist under `<strip>/<block>/`. The params of the current model are installed now.
    pub fn add_model_block(
        &self,
        model_param: ParamId,
        strip: StripId,
        block: &str,
        variants: BTreeMap<String, ModelParams>,
    ) {
        let mut st = self.st();
        let current = st
            .values
            .get(&model_param)
            .and_then(|v| v.as_str())
            .map(str::to_string);
        if let Some(params) = current.and_then(|m| variants.get(&m)) {
            for (spec, v) in params {
                st.values.insert(spec.id.clone(), v.clone());
                st.specs.insert(spec.id.clone(), spec.clone());
            }
        }
        st.models.push(ModelBinding {
            model_param,
            strip,
            block: block.to_string(),
            variants,
        });
    }

    /// Every accepted write through the [`Surface`] API, in order.
    pub fn writes(&self) -> Vec<(ParamId, Value)> {
        self.st().writes.clone()
    }

    pub fn clear_writes(&self) {
        self.st().writes.clear();
    }

    /// Simulate a change by someone else: validated like a write, not recorded in `writes`, emitted as External.
    pub fn external_set(&self, param: &ParamId, value: Value) -> SurfaceResult<Value> {
        let mut st = self.st();
        let stored = Self::store(&mut st, param, value, true)?;
        st.emit(SurfaceEvent::ParamChanged {
            param: param.clone(),
            value: stored.clone(),
            origin: Origin::External,
        });
        Ok(stored)
    }

    /// Console-side solo mode change (e.g. the engineer switched to solo-in-place).
    pub fn set_solo_mode(&self, mode: SoloMode) {
        let mut st = self.st();
        st.solo_mode = mode;
        st.emit(SurfaceEvent::SoloChanged);
    }

    /// Set the meter frame returned by `meters()` and push it to meter subscribers.
    pub fn set_meters(&self, mut frame: MeterFrame) {
        let mut st = self.st();
        frame.seq = st.meters.seq + 1;
        st.meter_subscribers
            .retain(|s| s.send(frame.clone()).is_ok());
        st.meters = frame;
    }

    /// Validate and store a value; on a model param, re-resolve the block's params.
    fn store(
        st: &mut State,
        param: &ParamId,
        value: Value,
        external: bool,
    ) -> SurfaceResult<Value> {
        let spec = st
            .specs
            .get(param)
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))?;
        if spec.read_only && !external {
            return Err(SurfaceError::ReadOnly(param.clone()));
        }
        let stored = spec.coerce(&value)?;
        let old = st.values.insert(param.clone(), stored.clone());
        if let Some(b) = st.models.iter().find(|b| &b.model_param == param).cloned() {
            if old.as_ref() != Some(&stored) {
                let prefix = format!("{}/{}/", b.strip, b.block);
                let stale: Vec<ParamId> = st
                    .specs
                    .values()
                    .filter(|s| s.model_dependent && s.id.as_str().starts_with(&prefix))
                    .map(|s| s.id.clone())
                    .collect();
                for id in stale {
                    st.specs.remove(&id);
                    st.values.remove(&id);
                }
                let model = stored.as_str().unwrap_or_default().to_string();
                for (spec, v) in b.variants.get(&model).cloned().unwrap_or_default() {
                    st.values.insert(spec.id.clone(), v);
                    st.specs.insert(spec.id.clone(), spec);
                }
                st.emit(SurfaceEvent::ParamsRemapped {
                    strip: b.strip.clone(),
                    block: b.block.clone(),
                    model,
                });
            }
        }
        Ok(stored)
    }

    /// A small generic console used by tests and examples:
    /// - `input.1..4` (KICK, SNARE, VOX, GTR): in/filter/gate/eq (model STD or SOUL)/dyn/insert.1/fader/pan,
    ///   post send to `bus.1`, pre send (tapped after dyn) to `bus.2`, main assign to `main.1`;
    /// - `bus.1` DRUMS (group, to main), `bus.2` IEM 1 (pre-fed, physical output), `bus.3` VERB (fx.1 inserted);
    /// - `main.1`, `matrix.1`, `dca.1`, `fx.1` (HALL on bus.3) and `fx.2` (free); `system/clock/source`.
    pub fn demo() -> Self {
        let f = FakeSurface::new("DEMO");
        f.set_capabilities(Capabilities {
            solo: true,
            listen_points: vec!["eq.band.1".into(), "dyn.key".into()],
            meters: Some(MeterCaps {
                input: true,
                output: true,
                pre_fader: true,
                gain_reduction: true,
                solo_bus: true,
            }),
            rta: Some(RtaCaps {
                bands_hz: vec![100.0, 1000.0, 10000.0],
            }),
            external_fx_models: vec!["EXT".into()],
        });
        let sid = |k: &str, n: u32| StripId::new(k, n);
        let chain: Vec<Block> = [
            "in", "filter", "gate", "eq", "dyn", "insert.1", "fader", "pan",
        ]
        .iter()
        .map(|b| Block::parse(b))
        .collect();
        let fl = |id: String, class, unit, lo, hi, v: f64| {
            (ParamSpec::float(id, class, unit, lo, hi), Value::Float(v))
        };
        let fader = |id: String| {
            (
                ParamSpec::float(id, ParamClass::Fader, Unit::Db, -144.0, 10.0)
                    .with_taper(Taper::Fader),
                Value::Float(0.0),
            )
        };
        let boolean = |id: String, class, v: bool| (ParamSpec::boolean(id, class), Value::Bool(v));
        let eq_std = |s: &StripId| -> ModelParams {
            let mut v = Vec::new();
            for b in 1..=2 {
                let base = format!("{s}/eq/band.{b}");
                v.push(fl(
                    format!("{base}.gain"),
                    ParamClass::Eq,
                    Unit::Db,
                    -15.0,
                    15.0,
                    0.0,
                ));
                v.push(fl(
                    format!("{base}.freq"),
                    ParamClass::Eq,
                    Unit::Hz,
                    20.0,
                    20000.0,
                    500.0 * b as f64,
                ));
                v.push(fl(
                    format!("{base}.q"),
                    ParamClass::Eq,
                    Unit::Q,
                    0.44,
                    10.0,
                    0.7,
                ));
            }
            v.into_iter()
                .map(|(sp, val)| {
                    let sp = if sp.unit == Unit::Db {
                        sp.with_step(0.25)
                    } else {
                        sp.with_taper(Taper::Log)
                    };
                    (sp.model_dependent(), val)
                })
                .collect()
        };
        let eq_soul = |s: &StripId| -> ModelParams {
            ["low.gain", "mid.gain", "high.gain"]
                .iter()
                .map(|leaf| {
                    let (sp, v) = fl(
                        format!("{s}/eq/{leaf}"),
                        ParamClass::Eq,
                        Unit::Db,
                        -15.0,
                        15.0,
                        0.0,
                    );
                    (sp.with_step(0.25).model_dependent(), v)
                })
                .collect()
        };

        for (n, name) in [(1, "KICK"), (2, "SNARE"), (3, "VOX"), (4, "GTR")] {
            let id = sid("input", n);
            let mut s = Strip::new(id.clone(), StripKind::Input, name);
            s.chain = chain.clone();
            s.sends = vec![
                SendInfo {
                    dest: sid("bus", 1),
                    on: true,
                    level_db: 0.0,
                    tap: SendTap::Post,
                    tap_after: Block::Pan,
                },
                SendInfo {
                    dest: sid("bus", 2),
                    on: true,
                    level_db: -5.0,
                    tap: SendTap::Pre,
                    tap_after: Block::Dyn,
                },
                SendInfo {
                    dest: sid("main", 1),
                    on: true,
                    level_db: 0.0,
                    tap: SendTap::Post,
                    tap_after: Block::Pan,
                },
            ];
            s.inserts = vec![InsertInfo {
                point: Block::Insert(1),
                slot: None,
                on: false,
            }];
            f.add_strip(s);
            let params = vec![
                (
                    ParamSpec::float(
                        format!("{id}/in/trim"),
                        ParamClass::Trim,
                        Unit::Db,
                        -18.0,
                        18.0,
                    )
                    .with_step(0.1),
                    Value::Float(0.0),
                ),
                boolean(format!("{id}/in/polarity"), ParamClass::Polarity, false),
                boolean(format!("{id}/filter/lowcut.on"), ParamClass::Filter, false),
                {
                    let (sp, v) = fl(
                        format!("{id}/filter/lowcut.freq"),
                        ParamClass::Filter,
                        Unit::Hz,
                        20.0,
                        2000.0,
                        80.0,
                    );
                    (sp.with_taper(Taper::Log), v)
                },
                fl(
                    format!("{id}/gate/threshold"),
                    ParamClass::Dynamics,
                    Unit::Db,
                    -80.0,
                    0.0,
                    -80.0,
                ),
                boolean(format!("{id}/eq/on"), ParamClass::Eq, true),
                (
                    ParamSpec::enumeration(
                        format!("{id}/eq/model"),
                        ParamClass::EqModel,
                        &["STD", "SOUL"],
                    ),
                    Value::Enum("STD".into()),
                ),
                boolean(format!("{id}/dyn/on"), ParamClass::Dynamics, false),
                fl(
                    format!("{id}/dyn/threshold"),
                    ParamClass::Dynamics,
                    Unit::Db,
                    -60.0,
                    0.0,
                    -10.0,
                ),
                fl(
                    format!("{id}/dyn/ratio"),
                    ParamClass::Dynamics,
                    Unit::Ratio,
                    1.1,
                    100.0,
                    3.0,
                ),
                boolean(format!("{id}/insert.1/on"), ParamClass::InsertEnable, false),
                (
                    ParamSpec::enumeration(
                        format!("{id}/insert.1/slot"),
                        ParamClass::InsertSlot,
                        &["none", "fx.1", "fx.2"],
                    ),
                    Value::Enum("none".into()),
                ),
                fader(format!("{id}/fader")),
                boolean(format!("{id}/mute"), ParamClass::Mute, false),
                fl(
                    format!("{id}/pan"),
                    ParamClass::Pan,
                    Unit::Pan,
                    -100.0,
                    100.0,
                    0.0,
                ),
                fader(format!("{id}/send/bus.1/level")),
                boolean(format!("{id}/send/bus.1/on"), ParamClass::SendOn, true),
                (
                    ParamSpec::enumeration(
                        format!("{id}/send/bus.1/tap"),
                        ParamClass::SendTap,
                        &["pre", "post", "group"],
                    ),
                    Value::Enum("post".into()),
                ),
                (
                    ParamSpec::float(
                        format!("{id}/send/bus.2/level"),
                        ParamClass::SendLevel,
                        Unit::Db,
                        -144.0,
                        10.0,
                    ),
                    Value::Float(-5.0),
                ),
                boolean(format!("{id}/send/main.1/on"), ParamClass::SendOn, true),
                (
                    ParamSpec::float(
                        format!("{id}/send/main.1/level"),
                        ParamClass::SendLevel,
                        Unit::Db,
                        -144.0,
                        10.0,
                    ),
                    Value::Float(0.0),
                ),
                (
                    ParamSpec {
                        id: format!("{id}/meta/name").as_str().into(),
                        class: ParamClass::Metadata,
                        kind: ValueKind::Text { max_len: Some(16) },
                        unit: Unit::None,
                        range: None,
                        taper: Taper::Discrete,
                        step: None,
                        read_only: false,
                        model_dependent: false,
                    },
                    Value::Text(name.into()),
                ),
                (
                    ParamSpec::float(
                        format!("{id}/meter/dyn_gr"),
                        ParamClass::Readonly,
                        Unit::Db,
                        -60.0,
                        0.0,
                    )
                    .read_only(),
                    Value::Float(0.0),
                ),
            ];
            for (sp, v) in params {
                // Send level to bus.1 is a send_level, not a fader.
                let sp = if sp.id.as_str().ends_with("send/bus.1/level") {
                    ParamSpec {
                        class: ParamClass::SendLevel,
                        ..sp
                    }
                } else {
                    sp
                };
                f.add_param(sp, v);
            }
            let mut variants = BTreeMap::new();
            variants.insert("STD".to_string(), eq_std(&id));
            variants.insert("SOUL".to_string(), eq_soul(&id));
            f.add_model_block(ParamId::of(&id, "eq/model"), id.clone(), "eq", variants);
        }

        let mut drums = Strip::new(sid("bus", 1), StripKind::Bus, "DRUMS");
        drums.chain = vec![
            Block::In,
            Block::Eq,
            Block::Dyn,
            Block::Insert(1),
            Block::Fader,
            Block::Pan,
        ];
        drums.sends = vec![SendInfo {
            dest: sid("main", 1),
            on: true,
            level_db: 0.0,
            tap: SendTap::Post,
            tap_after: Block::Pan,
        }];
        drums.inserts = vec![InsertInfo {
            point: Block::Insert(1),
            slot: None,
            on: false,
        }];
        f.add_strip(drums);
        let mut iem = Strip::new(sid("bus", 2), StripKind::Bus, "IEM 1");
        iem.chain = vec![Block::In, Block::Eq, Block::Dyn, Block::Fader, Block::Pan];
        iem.outputs = vec![OutputKind::Physical];
        iem.talkback_dest = true;
        f.add_strip(iem);
        let mut verb = Strip::new(sid("bus", 3), StripKind::Bus, "VERB");
        verb.chain = vec![
            Block::In,
            Block::Eq,
            Block::Insert(1),
            Block::Fader,
            Block::Pan,
        ];
        verb.inserts = vec![InsertInfo {
            point: Block::Insert(1),
            slot: Some(sid("fx", 1)),
            on: true,
        }];
        verb.sends = vec![SendInfo {
            dest: sid("main", 1),
            on: true,
            level_db: 0.0,
            tap: SendTap::Post,
            tap_after: Block::Pan,
        }];
        f.add_strip(verb);
        for b in 1..=3 {
            let id = sid("bus", b);
            f.add_param(fader(format!("{id}/fader")).0, Value::Float(0.0));
            f.add_param(
                ParamSpec::boolean(format!("{id}/mute"), ParamClass::Mute),
                Value::Bool(false),
            );
            f.add_param(
                ParamSpec::float(
                    format!("{id}/eq/band.1.gain"),
                    ParamClass::Eq,
                    Unit::Db,
                    -15.0,
                    15.0,
                )
                .with_step(0.25),
                Value::Float(0.0),
            );
            if b != 2 {
                f.add_param(
                    ParamSpec::boolean(format!("{id}/send/main.1/on"), ParamClass::SendOn),
                    Value::Bool(true),
                );
                f.add_param(
                    ParamSpec::float(
                        format!("{id}/send/main.1/level"),
                        ParamClass::SendLevel,
                        Unit::Db,
                        -144.0,
                        10.0,
                    ),
                    Value::Float(0.0),
                );
            }
        }
        let mut main = Strip::new(sid("main", 1), StripKind::Main, "MAIN");
        main.chain = vec![Block::In, Block::Eq, Block::Dyn, Block::Fader, Block::Pan];
        main.outputs = vec![OutputKind::Physical];
        f.add_strip(main);
        f.add_param(fader("main.1/fader".into()).0, Value::Float(0.0));
        f.add_param(
            ParamSpec::boolean("main.1/mute", ParamClass::Mute),
            Value::Bool(false),
        );
        let mut mtx = Strip::new(sid("matrix", 1), StripKind::Matrix, "ZONE");
        mtx.chain = vec![Block::Fader];
        mtx.outputs = vec![OutputKind::Physical];
        f.add_strip(mtx);
        f.add_param(fader("matrix.1/fader".into()).0, Value::Float(0.0));
        f.add_strip(Strip::new(sid("dca", 1), StripKind::Dca, "BAND"));
        f.add_param(fader("dca.1/fader".into()).0, Value::Float(0.0));
        f.add_param(
            ParamSpec::boolean("dca.1/mute", ParamClass::Mute),
            Value::Bool(false),
        );
        for (n, model, cat) in [
            (1, "HALL", FxCategory::Time),
            (2, "PLATE", FxCategory::Time),
        ] {
            let id = sid("fx", n);
            let mut s = Strip::new(id.clone(), StripKind::Fx, "");
            s.fx = Some(FxSlotInfo {
                model: model.into(),
                category: cat,
            });
            f.add_strip(s);
            f.add_param(
                ParamSpec::enumeration(
                    format!("{id}/model"),
                    ParamClass::FxModel,
                    &["HALL", "PLATE", "EXT"],
                ),
                Value::Enum(model.into()),
            );
            f.add_param(
                ParamSpec::float(
                    format!("{id}/mix"),
                    ParamClass::FxMix,
                    Unit::Percent,
                    0.0,
                    100.0,
                ),
                Value::Float(100.0),
            );
            f.add_param(
                ParamSpec::float(
                    format!("{id}/param.decay"),
                    ParamClass::FxParam,
                    Unit::Seconds,
                    0.1,
                    10.0,
                ),
                Value::Float(2.0),
            );
        }
        f.add_param(
            ParamSpec::enumeration("system/clock/source", ParamClass::Clock, &["INT", "AES50A"]),
            Value::Enum("INT".into()),
        );
        let strips: Vec<StripId> = f
            .st()
            .topology
            .strips
            .iter()
            .filter(|s| s.kind != StripKind::Fx)
            .map(|s| s.id.clone())
            .collect();
        f.set_meters(MeterFrame {
            seq: 0,
            strips: strips.into_iter().map(StripMeters::silent).collect(),
            solo_db: Some([-128.0; 2]),
            rta_db: Some(vec![-128.0; 3]),
        });
        f
    }
}

impl Surface for FakeSurface {
    fn info(&self) -> SurfaceInfo {
        self.st().info.clone()
    }

    fn topology(&self) -> SurfaceResult<Topology> {
        Ok(self.st().topology.clone())
    }

    fn params(&self, strip: &StripId) -> SurfaceResult<Vec<ParamSpec>> {
        let st = self.st();
        if st.topology.get(strip).is_none() {
            return Err(SurfaceError::UnknownStrip(strip.clone()));
        }
        Ok(st
            .specs
            .values()
            .filter(|s| s.id.strip().as_ref() == Some(strip))
            .cloned()
            .collect())
    }

    fn param_spec(&self, param: &ParamId) -> SurfaceResult<ParamSpec> {
        self.st()
            .specs
            .get(param)
            .cloned()
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))
    }

    fn read(&self, param: &ParamId) -> SurfaceResult<Value> {
        self.st()
            .values
            .get(param)
            .cloned()
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))
    }

    fn write(&self, param: &ParamId, value: Value) -> SurfaceResult<Value> {
        let mut st = self.st();
        let stored = Self::store(&mut st, param, value, false)?;
        st.writes.push((param.clone(), stored.clone()));
        Ok(stored)
    }

    fn subscribe(&self) -> Receiver<SurfaceEvent> {
        let (tx, rx) = channel();
        self.st().subscribers.push(tx);
        rx
    }

    fn solo_state(&self) -> SurfaceResult<SoloState> {
        let st = self.st();
        Ok(SoloState {
            mode: st.solo_mode,
            active: st.solo.iter().cloned().collect(),
        })
    }

    fn set_solo(&self, target: &SoloTarget, on: bool) -> SurfaceResult<()> {
        let mut st = self.st();
        if st.topology.get(target.strip_id()).is_none() {
            return Err(SurfaceError::UnknownStrip(target.strip_id().clone()));
        }
        if let SoloTarget::Listen { point, .. } = target {
            if !st.info.capabilities.listen_points.contains(point) {
                return Err(SurfaceError::Unsupported("listen point"));
            }
        }
        if on {
            if st.solo_mode == SoloMode::Destructive {
                return Err(SurfaceError::DestructiveSolo);
            }
            st.solo.insert(target.clone());
        } else {
            st.solo.remove(target);
        }
        Ok(())
    }

    fn clear_solo(&self) -> SurfaceResult<()> {
        self.st().solo.clear();
        Ok(())
    }

    fn meters(&self) -> SurfaceResult<MeterFrame> {
        Ok(self.st().meters.clone())
    }

    fn subscribe_meters(&self, _strips: &[StripId]) -> SurfaceResult<Receiver<MeterFrame>> {
        let (tx, rx) = channel();
        self.st().meter_subscribers.push(tx);
        Ok(rx)
    }

    fn rta_source(&self) -> SurfaceResult<Option<RtaSource>> {
        Ok(self.st().rta.clone())
    }

    fn set_rta_source(&self, source: Option<&RtaSource>) -> SurfaceResult<()> {
        let mut st = self.st();
        if let Some(s) = source {
            if st.topology.get(&s.strip).is_none() {
                return Err(SurfaceError::UnknownStrip(s.strip.clone()));
            }
        }
        st.rta = source.cloned();
        Ok(())
    }
}

impl ConformanceDriver for FakeSurface {
    fn external_write(&self, param: &ParamId, value: Value) -> bool {
        self.external_set(param, value).is_ok()
    }

    fn set_destructive_solo(&self, on: bool) -> bool {
        self.set_solo_mode(if on {
            SoloMode::Destructive
        } else {
            SoloMode::NonDestructive
        });
        true
    }
}

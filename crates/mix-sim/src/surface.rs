//! [`SimSurface`]: a [`Surface`] backed by the wing-sim console running in-process.
//!
//! Construction follows docs/research/wing-control.md section 6: scene into a `ParamStore`,
//! `wing_sim::audio::prepare` (the [`ConsoleControl`] is kept so routing, solo, DCA and mute-group changes made after
//! the build are published), `Console::build` with a [`LevelBank`], warm-up, then per engine block `Console::cycle`
//! with USB In 1..48 fed from the loaded [`Multitrack`]. Parameter writes go to the store (the console picks them up
//! from the change queue on the next cycle, smoothed like on the daemon) followed by `ctl.refresh`.
//!
//! Rendering is offline: [`SimSurface::render`] runs as many cycles as requested back to back and returns the
//! requested taps; it advances the input position, so a closed loop alternates `render` and `write`. Meters are the
//! peak levels of the last render (in-process, so pre-fader levels and gain reduction are available).

use crate::map::{self, Binding, SimStrip, BUSES, CHANNELS, MAINS};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::model::{Block, SendInfo, SendTap, Strip, Topology};
use mix_surface::surface::{
    Capabilities, MeterCaps, MeterFrame, Origin, SoloMode, SoloState, SoloTarget, StripMeters,
    Surface, SurfaceError, SurfaceEvent, SurfaceInfo, SurfaceResult,
};
use mix_surface::testkit::ConformanceDriver;
use std::collections::BTreeMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use wing_engine::meters::{solo_cell, strip_cell, word, LevelBank};
use wing_engine::routing::{Side, Strip as EStrip};
use wing_engine::Port;
use wing_model::{NodeId, ParamStore, Snapshot, Value as WValue};
use wing_sim::audio::console::{Console, ConsoleConfig};
use wing_sim::audio::driver::SESSION_WARM_UP_CYCLES;
use wing_sim::audio::stats::AudioStats;
use wing_sim::audio::{prepare, ConsoleControl};

/// Store origin of writes through the [`Surface`] API.
const ORIGIN_SURFACE: wing_model::Origin = 0x4d58_0001;
/// Store origin of simulated console-side changes ([`SimSurface::external_set`]).
const ORIGIN_EXTERNAL: wing_model::Origin = 0x4d58_0002;
/// Store origin of fixture setup ([`SimSurface::patch_usb`], scene application).
const ORIGIN_SETUP: wing_model::Origin = 0x4d58_0003;
/// USB inputs of the console.
pub const USB_INPUTS: usize = 48;
/// Floor for dB conversion of meter levels.
const METER_FLOOR_DB: f32 = -128.0;

/// Console options.
#[derive(Debug, Clone)]
pub struct SimConfig {
    /// Input channels exposed as strips (`input.1..=channels`, max 40). Buses 1..16 and mains 1..4 are always exposed.
    pub channels: u8,
    pub sample_rate: u32,
    /// Engine block (frames per cycle). The daemon uses 64.
    pub block: usize,
    /// Engine worker threads besides the rendering thread (0 = everything on the caller's thread).
    pub workers: usize,
    /// Scene (`.snap` text) loaded before the console is built; `None` = factory defaults.
    pub scene: Option<String>,
}

impl Default for SimConfig {
    fn default() -> Self {
        SimConfig {
            channels: CHANNELS,
            sample_rate: 48_000,
            block: 64,
            workers: 0,
            scene: None,
        }
    }
}

/// One mono input track patched to a USB input.
#[derive(Debug, Clone)]
pub struct Track {
    pub name: String,
    /// WING input channel (1..40) the track is patched to.
    pub channel: u8,
    /// USB input (1..48) carrying the track.
    pub usb: u8,
    pub audio: Vec<f32>,
}

/// Multitrack input for the console (all tracks at `sample_rate`, starting together).
#[derive(Debug, Clone, Default)]
pub struct Multitrack {
    pub sample_rate: u32,
    pub tracks: Vec<Track>,
}

impl Multitrack {
    /// Length in frames (longest track).
    pub fn frames(&self) -> usize {
        self.tracks.iter().map(|t| t.audio.len()).max().unwrap_or(0)
    }

    pub fn seconds(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }
}

/// Where a render reads its output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TapPoint {
    /// Main 1..4 post fader.
    Main(u8),
    /// Bus 1..16 post fader.
    Bus(u8),
    /// Solo bus (what is soloed, before the monitor level).
    Solo,
    /// Monitor 1 (phones) or 2 (speakers) output, incl. solo.
    Monitor(u8),
    /// Input channel 1..40 post processing (after fader and pan).
    InputPost(u8),
}

/// Stereo audio of one tap.
#[derive(Debug, Clone, PartialEq)]
pub struct TapAudio {
    pub tap: TapPoint,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl TapAudio {
    /// RMS level in dBFS over both sides.
    pub fn rms_db(&self) -> f64 {
        let n = (self.left.len() + self.right.len()).max(1) as f64;
        let e: f64 = self
            .left
            .iter()
            .chain(&self.right)
            .map(|&x| (x as f64) * (x as f64))
            .sum();
        10.0 * (e / n).max(1e-20).log10()
    }

    pub fn peak(&self) -> f32 {
        self.left
            .iter()
            .chain(&self.right)
            .fold(0.0f32, |m, &x| m.max(x.abs()))
    }
}

/// Result of [`SimSurface::render`].
#[derive(Debug, Clone, PartialEq)]
pub struct Rendered {
    pub sample_rate: u32,
    /// Input position (frames) at the start of this render.
    pub start_frame: u64,
    pub frames: usize,
    pub taps: Vec<TapAudio>,
}

impl Rendered {
    pub fn tap(&self, t: TapPoint) -> Option<&TapAudio> {
        self.taps.iter().find(|a| a.tap == t)
    }
}

struct Inner {
    store: ParamStore,
    ctl: ConsoleControl,
    console: Console,
    bank: Arc<LevelBank>,
    strips: Vec<SimStrip>,
    params: BTreeMap<ParamId, Binding>,
    subscribers: Vec<Sender<SurfaceEvent>>,
    meter_subscribers: Vec<Sender<MeterFrame>>,
    meters: MeterFrame,
    input: Multitrack,
    /// USB input (0-based) -> track index.
    usb_map: [Option<usize>; USB_INPUTS],
    pos: u64,
    solo_mode_node: NodeId,
}

impl Inner {
    fn emit(&mut self, ev: SurfaceEvent) {
        self.subscribers.retain(|s| s.send(ev.clone()).is_ok());
    }

    fn rebuild_strip(&mut self, strip: SimStrip) {
        let prefix = format!("{}/", strip.id());
        self.params
            .retain(|id, _| !id.as_str().starts_with(&prefix));
        for b in map::strip_bindings(&self.store, strip) {
            self.params.insert(b.mapped.id.clone(), b);
        }
    }

    fn binding(&self, id: &ParamId) -> SurfaceResult<&Binding> {
        self.params
            .get(id)
            .ok_or_else(|| SurfaceError::UnknownParam(id.clone()))
    }

    /// Validate and store; re-resolve model-dependent params when a model selector changed.
    fn store_param(
        &mut self,
        id: &ParamId,
        value: Value,
        origin: wing_model::Origin,
        allow_read_only: bool,
    ) -> SurfaceResult<Value> {
        let b = self.binding(id)?.clone();
        let spec = b.spec(&self.store)?;
        if spec.read_only && !allow_read_only {
            return Err(SurfaceError::ReadOnly(id.clone()));
        }
        let v = spec.coerce(&value)?;
        let changed = map::write_value(&self.store, &b, &v, origin)?;
        self.ctl.refresh(&self.store);
        let is_model = matches!(
            spec.class,
            mix_surface::model::ParamClass::EqModel | mix_surface::model::ParamClass::DynamicsModel
        );
        if changed && is_model {
            let strip = SimStrip::from_id(&id.strip().expect("strip param")).expect("sim strip");
            self.rebuild_strip(strip);
            let block = id.block().unwrap_or_default().to_string();
            self.emit(SurfaceEvent::ParamsRemapped {
                strip: strip.id(),
                block,
                model: v.as_str().unwrap_or_default().to_string(),
            });
        }
        map::read_value(&self.store, &b)
    }

    fn solo_node(&self, strip: SimStrip) -> NodeId {
        self.store
            .lookup(&format!("{}/$solo", strip.wing()))
            .expect("schema has $solo")
    }

    fn solo_mode(&self) -> SoloMode {
        // cfg/solo/mode: LIVE (0), STUDIO (1), SIP (2). `cfg/solo/mute` is not modelled by the simulator.
        match self.store.get(self.solo_mode_node) {
            Some(WValue::Enum(2)) => SoloMode::Destructive,
            _ => SoloMode::NonDestructive,
        }
    }

    fn solo_state(&self) -> SoloState {
        let active = self
            .strips
            .iter()
            .filter(|s| self.store.get(self.solo_node(**s)) == Some(WValue::Int(1)))
            .map(|s| SoloTarget::strip(s.id()))
            .collect();
        SoloState {
            mode: self.solo_mode(),
            active,
        }
    }

    fn set_solo_raw(&mut self, strip: SimStrip, on: bool) {
        let n = self.solo_node(strip);
        let _ = self
            .store
            .set_from(n, WValue::Int(on as i32), ORIGIN_SURFACE);
        self.ctl.refresh(&self.store);
    }

    fn take_meters(&mut self) -> MeterFrame {
        let db = |v: f32| {
            if v > 0.0 {
                (20.0 * v.log10()).clamp(METER_FLOOR_DB, 24.0)
            } else {
                METER_FLOOR_DB
            }
        };
        let bank = &self.bank;
        let strips = self
            .strips
            .iter()
            .map(|s| {
                let i = s.engine().index().expect("strip index");
                let w = |k| bank.take(strip_cell(i, k));
                let is_input = matches!(s, SimStrip::Input(_));
                StripMeters {
                    strip: s.id(),
                    input_db: [db(w(word::IN_L)), db(w(word::IN_R))],
                    output_db: [db(w(word::OUT_L)), db(w(word::OUT_R))],
                    pre_fader_db: Some([db(w(word::PRE_L)), db(w(word::PRE_R))]),
                    gate_gr_db: is_input.then(|| w(word::GATE_GR).max(0.0)),
                    dyn_gr_db: Some(w(word::DYN_GR).max(0.0)),
                    gate_open: None,
                    dyn_active: None,
                }
            })
            .collect();
        let solo = [db(bank.take(solo_cell(0))), db(bank.take(solo_cell(1)))];
        MeterFrame {
            seq: self.meters.seq + 1,
            strips,
            solo_db: Some(solo),
            rta_db: None,
        }
    }

    fn port_of(&self, t: TapPoint, side: Side) -> Option<Port> {
        let mix = &self.console.nodes().mix;
        match t {
            TapPoint::Main(n) if (1..=MAINS).contains(&n) => mix.post(EStrip::Main(n), side),
            TapPoint::Bus(n) if (1..=BUSES).contains(&n) => mix.post(EStrip::Bus(n), side),
            TapPoint::InputPost(n) if (1..=CHANNELS).contains(&n) => mix.post(EStrip::Ch(n), side),
            TapPoint::Solo => Some(mix.solo_port(1, side)),
            TapPoint::Monitor(m @ 1..=2) => Some(mix.monitor_port(m as usize, side)),
            _ => None,
        }
    }
}

/// The in-process WING console as a [`Surface`]. See the module docs.
pub struct SimSurface {
    inner: Mutex<Inner>,
    cfg: SimConfig,
}

impl SimSurface {
    /// Build a console with `cfg` (factory scene unless `cfg.scene` is set).
    pub fn new(cfg: SimConfig) -> Result<Self, String> {
        if !(1..=CHANNELS).contains(&cfg.channels) {
            return Err(format!("channels must be 1..={CHANNELS}"));
        }
        let mut store = ParamStore::new();
        if let Some(text) = &cfg.scene {
            Snapshot::load(&store, text).map_err(|e| format!("scene: {e}"))?;
        }
        let mut ctl = prepare(&mut store);
        ctl.refresh(&store);
        let bank = LevelBank::shared();
        let ccfg = ConsoleConfig {
            sample_rate: cfg.sample_rate,
            block: cfg.block,
            workers: cfg.workers,
            rt_workers: false,
            workgroup: None,
            meters: Some(bank.clone()),
            rta: None,
            probe: None,
        };
        let mut console = Console::build(
            ccfg,
            &store,
            &mut ctl,
            None,
            Vec::new(),
            Arc::new(AudioStats::default()),
        )?;
        console.warm_up(SESSION_WARM_UP_CYCLES);
        bank.clear();
        let strips: Vec<SimStrip> = (1..=cfg.channels)
            .map(SimStrip::Input)
            .chain((1..=BUSES).map(SimStrip::Bus))
            .chain((1..=MAINS).map(SimStrip::Main))
            .collect();
        let solo_mode_node = store.lookup("cfg/solo/mode").ok_or("no cfg/solo/mode")?;
        let mut inner = Inner {
            store,
            ctl,
            console,
            bank,
            strips: strips.clone(),
            params: BTreeMap::new(),
            subscribers: Vec::new(),
            meter_subscribers: Vec::new(),
            meters: MeterFrame::default(),
            input: Multitrack::default(),
            usb_map: [None; USB_INPUTS],
            pos: 0,
            solo_mode_node,
        };
        for s in strips {
            inner.rebuild_strip(s);
        }
        inner.meters = inner.take_meters();
        Ok(SimSurface {
            inner: Mutex::new(inner),
            cfg,
        })
    }

    /// Factory scene, `channels` input strips, deterministic single-threaded engine.
    pub fn with_channels(channels: u8) -> Result<Self, String> {
        Self::new(SimConfig {
            channels,
            ..SimConfig::default()
        })
    }

    pub fn config(&self) -> &SimConfig {
        &self.cfg
    }

    fn st(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Set a raw WING tree path from display/snapshot text (`ch/1/in/conn/grp` = `USB`). Setup only: bypasses the
    /// generic mapping and the remit (patching, solo config, names). Rebuilds the affected param specs.
    pub fn set_wing(&self, path: &str, text: &str) -> Result<(), String> {
        let mut guard = self.st();
        let st = &mut *guard;
        let id = st
            .store
            .lookup(path)
            .ok_or_else(|| format!("unknown WING path {path}"))?;
        st.store
            .set_text(id, text, ORIGIN_SETUP)
            .map_err(|e| format!("{path} = {text}: {e}"))?;
        st.ctl.refresh(&st.store);
        let strips = st.strips.clone();
        for s in strips {
            if path.starts_with(&format!("{}/", s.wing())) {
                st.rebuild_strip(s);
            }
        }
        Ok(())
    }

    /// Read a raw WING path as its snapshot/wire token (`USB`, `-6.0`).
    pub fn get_wing(&self, path: &str) -> Option<String> {
        let st = self.st();
        let id = st.store.lookup(path)?;
        let mut s = String::new();
        st.store.format_token(id, &mut s).ok()?;
        Some(s)
    }

    /// Patch WING channel `ch` (1..40) to USB input `usb` (1..48).
    pub fn patch_usb(&self, ch: u8, usb: u8) -> Result<(), String> {
        if !(1..=CHANNELS).contains(&ch) || !(1..=USB_INPUTS as u8).contains(&usb) {
            return Err(format!("patch ch {ch} <- USB {usb}: out of range"));
        }
        self.set_wing(&format!("ch/{ch}/in/conn/grp"), "USB")?;
        self.set_wing(&format!("ch/{ch}/in/conn/in"), &usb.to_string())
    }

    /// Load the input multitrack: patches every track's channel to its USB input and rewinds to frame 0.
    pub fn load_inputs(&self, input: Multitrack) -> Result<(), String> {
        if input.sample_rate != self.cfg.sample_rate {
            return Err(format!(
                "multitrack at {} Hz, console at {} Hz",
                input.sample_rate, self.cfg.sample_rate
            ));
        }
        let mut usb_map = [None; USB_INPUTS];
        for (i, t) in input.tracks.iter().enumerate() {
            self.patch_usb(t.channel, t.usb)?;
            let slot = &mut usb_map[t.usb as usize - 1];
            if slot.is_some() {
                return Err(format!("USB input {} used twice", t.usb));
            }
            *slot = Some(i);
        }
        let mut st = self.st();
        st.input = input;
        st.usb_map = usb_map;
        st.pos = 0;
        Ok(())
    }

    /// Input position in frames.
    pub fn position(&self) -> u64 {
        self.st().pos
    }

    /// Move the input position (frames).
    pub fn seek(&self, frame: u64) {
        self.st().pos = frame;
    }

    /// Render `seconds` (rounded up to whole engine blocks) and return `taps`. Inputs past the end of the multitrack
    /// are silence. Updates the meter frame (peaks over this render) and notifies meter subscribers.
    pub fn render(&self, seconds: f64, taps: &[TapPoint]) -> Rendered {
        let frames = (seconds * self.cfg.sample_rate as f64).round().max(0.0) as usize;
        self.render_frames(frames, taps)
    }

    /// [`SimSurface::render`] in frames (rounded up to whole blocks).
    pub fn render_frames(&self, frames: usize, taps: &[TapPoint]) -> Rendered {
        let mut guard = self.st();
        let st = &mut *guard;
        let block = st.console.block();
        let blocks = frames.div_ceil(block);
        let total = blocks * block;
        let ports: Vec<[Option<Port>; 2]> = taps
            .iter()
            .map(|&t| [st.port_of(t, Side::L), st.port_of(t, Side::R)])
            .collect();
        let mut out: Vec<TapAudio> = taps
            .iter()
            .map(|&tap| TapAudio {
                tap,
                left: Vec::with_capacity(total),
                right: Vec::with_capacity(total),
            })
            .collect();
        let start = st.pos;
        let mut pos = start as usize;
        let tracks = &st.input.tracks;
        let usb_map = st.usb_map;
        for _ in 0..blocks {
            st.console.cycle(
                |ch, buf| match usb_map.get(ch).copied().flatten() {
                    Some(t) => {
                        let a = &tracks[t].audio;
                        let n = buf.len();
                        let avail = a.len().saturating_sub(pos).min(n);
                        buf[..avail].copy_from_slice(&a[pos..pos + avail]);
                        buf[avail..].fill(0.0);
                    }
                    None => buf.fill(0.0),
                },
                |_, _| {},
            );
            for (o, p) in out.iter_mut().zip(&ports) {
                match p[0] {
                    Some(p) => o.left.extend_from_slice(st.console.port(p)),
                    None => o.left.extend(std::iter::repeat_n(0.0, block)),
                }
                match p[1] {
                    Some(p) => o.right.extend_from_slice(st.console.port(p)),
                    None => o.right.extend(std::iter::repeat_n(0.0, block)),
                }
            }
            pos += block;
        }
        st.pos = pos as u64;
        let frame = st.take_meters();
        st.meter_subscribers
            .retain(|s| s.send(frame.clone()).is_ok());
        st.meters = frame;
        Rendered {
            sample_rate: self.cfg.sample_rate,
            start_frame: start,
            frames: total,
            taps: out,
        }
    }

    /// Simulate a change made on the console or by another client: validated like a write (read-only params
    /// included), emitted as [`Origin::External`].
    pub fn external_set(&self, param: &ParamId, value: Value) -> SurfaceResult<Value> {
        let mut st = self.st();
        let stored = st.store_param(param, value, ORIGIN_EXTERNAL, true)?;
        st.emit(SurfaceEvent::ParamChanged {
            param: param.clone(),
            value: stored.clone(),
            origin: Origin::External,
        });
        Ok(stored)
    }

    /// Console-side solo mode (destructive = SIP, otherwise LIVE). Emits `SoloChanged`.
    pub fn set_solo_mode(&self, mode: SoloMode) {
        let mut guard = self.st();
        let st = &mut *guard;
        let idx = match mode {
            SoloMode::Destructive => 2,
            SoloMode::NonDestructive => 0,
        };
        let n = st.solo_mode_node;
        let _ = st.store.set_from(n, WValue::Enum(idx), ORIGIN_EXTERNAL);
        st.ctl.refresh(&st.store);
        st.emit(SurfaceEvent::SoloChanged);
    }

    /// Apply a list of generic writes (fixture scenes). Stops at the first error.
    pub fn apply(&self, writes: &[(ParamId, Value)]) -> SurfaceResult<()> {
        for (p, v) in writes {
            self.write(p, v.clone())?;
        }
        Ok(())
    }
}

impl Surface for SimSurface {
    fn info(&self) -> SurfaceInfo {
        SurfaceInfo {
            surface: "sim".into(),
            name: "wing-sim (in-process)".into(),
            model: "WING".into(),
            firmware: "wing-sim 957b406".into(),
            capabilities: Capabilities {
                solo: true,
                listen_points: Vec::new(),
                meters: Some(MeterCaps {
                    input: true,
                    output: true,
                    pre_fader: true,
                    gain_reduction: true,
                    solo_bus: true,
                }),
                rta: None,
                external_fx_models: vec!["EXT".into()],
            },
        }
    }

    fn topology(&self) -> SurfaceResult<Topology> {
        let st = self.st();
        let get_bool = |path: &str| {
            st.store
                .lookup(path)
                .and_then(|n| st.store.get(n))
                .is_some_and(|v| v == WValue::Int(1))
        };
        let get_f = |path: &str| {
            st.store
                .lookup(path)
                .and_then(|n| st.store.get_f64(n))
                .unwrap_or(-144.0)
        };
        let get_tok = |path: &str| -> String {
            let mut s = String::new();
            if let Some(n) = st.store.lookup(path) {
                let _ = st.store.format_token(n, &mut s);
            }
            s
        };
        let get_str = |path: &str| -> String {
            st.store
                .lookup(path)
                .and_then(|n| st.store.with_str(n, str::to_string))
                .unwrap_or_default()
        };
        let mut strips = Vec::new();
        for &s in &st.strips {
            let w = s.wing();
            let mut strip = Strip::new(s.id(), s.kind(), get_str(&format!("{w}/name")));
            strip.tags = get_str(&format!("{w}/tags"))
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty() && !t.starts_with('#'))
                .map(str::to_string)
                .collect();
            let input = matches!(s, SimStrip::Input(_));
            strip.chain = if input {
                vec![
                    Block::In,
                    Block::Filter,
                    Block::Gate,
                    Block::Eq,
                    Block::Dyn,
                    Block::Fader,
                    Block::Pan,
                ]
            } else {
                vec![Block::In, Block::Eq, Block::Dyn, Block::Fader, Block::Pan]
            };
            if input {
                for b in 1..=BUSES {
                    let mode = get_tok(&format!("{w}/send/{b}/mode"));
                    let tap = match mode.as_str() {
                        "PRE" => SendTap::Pre,
                        "GRP" => SendTap::Group,
                        _ => SendTap::Post,
                    };
                    strip.sends.push(SendInfo {
                        dest: StripId::new("bus", b as u32),
                        on: get_bool(&format!("{w}/send/{b}/on")),
                        level_db: get_f(&format!("{w}/send/{b}/lvl")),
                        tap,
                        // ptap default 5 = after the third block of GEDI (dyn); simplification.
                        tap_after: if tap == SendTap::Pre {
                            Block::Dyn
                        } else {
                            Block::Pan
                        },
                    });
                }
            }
            if !matches!(s, SimStrip::Main(_)) {
                for m in 1..=MAINS {
                    let pre = get_bool(&format!("{w}/main/{m}/pre"));
                    strip.sends.push(SendInfo {
                        dest: StripId::new("main", m as u32),
                        on: get_bool(&format!("{w}/main/{m}/on")),
                        level_db: get_f(&format!("{w}/main/{m}/lvl")),
                        tap: if pre { SendTap::Pre } else { SendTap::Post },
                        tap_after: if pre { Block::Dyn } else { Block::Pan },
                    });
                }
            }
            strips.push(strip);
        }
        Ok(Topology { strips })
    }

    fn params(&self, strip: &StripId) -> SurfaceResult<Vec<mix_surface::ParamSpec>> {
        let st = self.st();
        let s = SimStrip::from_id(strip)
            .filter(|s| st.strips.contains(s))
            .ok_or_else(|| SurfaceError::UnknownStrip(strip.clone()))?;
        // keep chain order as built by the mapping
        map::strip_bindings(&st.store, s)
            .into_iter()
            .map(|b| b.spec(&st.store))
            .collect()
    }

    fn param_spec(&self, param: &ParamId) -> SurfaceResult<mix_surface::ParamSpec> {
        let st = self.st();
        st.binding(param)?.spec(&st.store)
    }

    fn read(&self, param: &ParamId) -> SurfaceResult<Value> {
        let st = self.st();
        let b = st.binding(param)?;
        map::read_value(&st.store, b)
    }

    fn write(&self, param: &ParamId, value: Value) -> SurfaceResult<Value> {
        self.st().store_param(param, value, ORIGIN_SURFACE, false)
    }

    fn subscribe(&self) -> Receiver<SurfaceEvent> {
        let (tx, rx) = channel();
        self.st().subscribers.push(tx);
        rx
    }

    fn solo_state(&self) -> SurfaceResult<SoloState> {
        Ok(self.st().solo_state())
    }

    fn set_solo(&self, target: &SoloTarget, on: bool) -> SurfaceResult<()> {
        let mut st = self.st();
        let strip = match target {
            SoloTarget::Strip { strip } => strip,
            SoloTarget::Listen { .. } => return Err(SurfaceError::Unsupported("listen points")),
        };
        let s = SimStrip::from_id(strip)
            .filter(|s| st.strips.contains(s))
            .ok_or_else(|| SurfaceError::UnknownStrip(strip.clone()))?;
        if on && st.solo_mode() == SoloMode::Destructive {
            return Err(SurfaceError::DestructiveSolo);
        }
        st.set_solo_raw(s, on);
        Ok(())
    }

    fn clear_solo(&self) -> SurfaceResult<()> {
        let mut st = self.st();
        for s in st.strips.clone() {
            st.set_solo_raw(s, false);
        }
        Ok(())
    }

    fn meters(&self) -> SurfaceResult<MeterFrame> {
        Ok(self.st().meters.clone())
    }

    fn subscribe_meters(&self, strips: &[StripId]) -> SurfaceResult<Receiver<MeterFrame>> {
        // Frames are pushed after every render; filtering by strip is left to the caller (cheap, few strips).
        let _ = strips;
        let (tx, rx) = channel();
        self.st().meter_subscribers.push(tx);
        Ok(rx)
    }
}

impl ConformanceDriver for SimSurface {
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

//! [`WingSurface`]: the [`Surface`] implementation for a WING (or wing-sim) over the native protocol.
//!
//! - Reads come from the mirror store (kept current by change events); writes are validated against the generic
//!   spec, applied to the mirror (same quantisation as the console) and sent as `D7 <hash> <value>`.
//! - Change events of other clients / the console surface arrive as [`Origin::External`], own writes as
//!   [`Origin::Local`]. A model-selector change (either origin) emits [`SurfaceEvent::ParamsRemapped`]; names, tags,
//!   send assignment/tap/level-over-floor, patching, inserts, FX models and talkback emit
//!   [`SurfaceEvent::TopologyChanged`]; solo leaves emit [`SurfaceEvent::SoloChanged`].
//! - Solo: `*/$solo` (strips and DCAs), listens (`eq/$solo` + `$solobd`, `gatesc/$solo`, `dynsc/$solo`,
//!   `dynxo/$solo`). State reads are fresh gets. `set_solo(_, true)` is refused while `cfg/solo/mode` is SIP (or
//!   STUDIO with `cfg/solo/mute`). Clearing writes 0 to every active solo explicitly; it never writes
//!   `$stat/solo` (the console's "clear solo" would race with re-applied solos).
//! - RTA: `cfg/rta/rtasrc` (1..40 ch, 41..48 aux, 49..64 bus, 65..68 main, 69..76 mtx, FX slot for FX taps,
//!   0 off) + `rtatap`; [`WingSurface::claim_rta`] restores both exactly on drop.

use crate::client::{ClientMsg, NativeClient, ORIGIN_LOCAL, ORIGIN_REMOTE};
use crate::convert;
use crate::map::{self, Index, Mapped};
use crate::meters::{rta_bands_hz, MeterEngine};
use crate::topology;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::model::{ParamClass, ParamSpec, Topology};
use mix_surface::surface::{
    Capabilities, MeterCaps, MeterFrame, Origin, RtaCaps, RtaSource, SoloMode, SoloState,
    SoloTarget, Surface, SurfaceError, SurfaceEvent, SurfaceInfo, SurfaceResult,
};
use mix_surface::testkit::ConformanceDriver;
use std::collections::{BTreeSet, HashMap};
use std::net::SocketAddr;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;
use wing_model::{NodeId, ParamStore, Schema, Value as WValue};

/// Connection options.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// TCP connect and per-request timeout.
    pub timeout: Duration,
    /// Timeout of the initial full sync.
    pub sync_timeout: Duration,
    /// UDP port to probe for the console identity (default: the native port, as on a real WING). `None` skips.
    pub discovery_port: Option<u16>,
    /// Reconnect automatically after the connection drops (default on). `None`: stay disconnected.
    pub reconnect: Option<ReconnectPolicy>,
}

/// Backoff of the automatic reconnect: retry after `initial`, doubling up to `max`, until the surface is dropped.
///
/// While the link is down every call fails with [`SurfaceError::Disconnected`] and `SurfaceEvent::Disconnected`
/// has been emitted. Once the TCP connection is back and the parameter mirror is fully resynchronised the surface
/// emits [`SurfaceEvent::Reconnected`] followed by [`SurfaceEvent::TopologyChanged`]; the console may have changed
/// anything meanwhile (or have been rebooted), so consumers re-read what they care about. The meter report is
/// redefined by the meter receiver on its own once packets resume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconnectPolicy {
    pub initial: Duration,
    pub max: Duration,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        ReconnectPolicy {
            initial: Duration::from_millis(250),
            max: Duration::from_secs(5),
        }
    }
}

impl Default for ConnectOptions {
    fn default() -> Self {
        ConnectOptions {
            timeout: Duration::from_secs(5),
            sync_timeout: Duration::from_secs(30),
            discovery_port: Some(wing_proto::discovery::DISCOVERY_PORT),
            reconnect: Some(ReconnectPolicy::default()),
        }
    }
}

/// Send level floor for "active" sends (remit.md 6.1, A.4).
const SEND_FLOOR_DB: f64 = -90.0;

/// Listen points this adapter supports (`eq.band.N` with N within the strip's `$solobd` range).
pub const LISTEN_POINTS: &[&str] = &[
    "eq.band.1",
    "eq.band.2",
    "eq.band.3",
    "eq.band.4",
    "eq.band.5",
    "eq.band.6",
    "eq.band.7",
    "eq.band.8",
    "gate.key",
    "dyn.key",
    "dyn.xover",
];

/// One solo/listen leaf.
#[derive(Debug, Clone)]
struct SoloLeaf {
    strip: StripId,
    /// `None` = strip solo; else the listen point (for EQ: `eq`, band from `$solobd`).
    point: Option<&'static str>,
    node: NodeId,
    band: Option<NodeId>,
}

fn solo_leaves() -> Vec<SoloLeaf> {
    let s = Schema::get();
    let mut v = Vec::new();
    for &(dom, kind, n) in map::STRIP_DOMAINS {
        if dom == "fx" {
            continue;
        }
        for i in 1..=n {
            let strip = StripId::new(kind, i);
            let p = format!("{dom}/{i}");
            if let Some(node) = s.by_path(&format!("{p}/$solo")) {
                v.push(SoloLeaf {
                    strip: strip.clone(),
                    point: None,
                    node,
                    band: None,
                });
            }
            for (blk, point) in [
                ("eq", "eq"),
                ("gatesc", "gate.key"),
                ("dynsc", "dyn.key"),
                ("dynxo", "dyn.xover"),
            ] {
                if let Some(node) = s.by_path(&format!("{p}/{blk}/$solo")) {
                    v.push(SoloLeaf {
                        strip: strip.clone(),
                        point: Some(point),
                        node,
                        band: (blk == "eq")
                            .then(|| s.by_path(&format!("{p}/eq/$solobd")))
                            .flatten(),
                    });
                }
            }
        }
    }
    v
}

/// RTA tap tokens and generic names.
const RTA_TAPS: &[(&str, &str)] = &[
    ("IN", "input"),
    ("POST", "post"),
    ("FILT", "filter"),
    ("PREEQ", "pre_eq"),
    ("POSTEQ", "post_eq"),
    ("PREFDR", "pre_fader"),
    ("GATEK", "gate_key"),
    ("DYNK", "dyn_key"),
    ("DYNXO", "dyn_xover"),
    ("PRETAP", "pre_tap"),
    ("SOLO", "solo"),
    ("MON.PH", "mon_ph"),
    ("MON.SPK", "mon_spk"),
    ("FXIN", "fx_in"),
    ("FXOUT", "fx_out"),
];

/// `rtasrc` number of a strip (1..76) or FX slot (FX taps).
fn rta_number(strip: &StripId, fx_tap: bool) -> Option<i32> {
    let n = strip.index()? as i32;
    let (base, max) = match (strip.kind_name(), fx_tap) {
        ("fx", true) => (0, 16),
        ("input", false) => (0, 40),
        ("aux", false) => (40, 8),
        ("bus", false) => (48, 16),
        ("main", false) => (64, 4),
        ("matrix", false) => (68, 8),
        _ => return None,
    };
    (1..=max).contains(&n).then_some(base + n)
}

fn rta_strip(n: i32, fx_tap: bool) -> Option<StripId> {
    if fx_tap {
        return (1..=16).contains(&n).then(|| StripId::new("fx", n as u32));
    }
    let (kind, k) = match n {
        1..=40 => ("input", n),
        41..=48 => ("aux", n - 40),
        49..=64 => ("bus", n - 48),
        65..=68 => ("main", n - 64),
        69..=76 => ("matrix", n - 68),
        _ => return None,
    };
    Some(StripId::new(kind, k as u32))
}

type Subs = Arc<Mutex<Vec<Sender<SurfaceEvent>>>>;
/// Config nodes this surface wrote itself (RTA claim/restore, solo config) and when. Console echoes of them
/// within [`OWN_WRITE_WINDOW`] are own-origin, not engineer moves (M063).
type OwnWrites = Arc<Mutex<HashMap<NodeId, std::time::Instant>>>;
const OWN_WRITE_WINDOW: Duration = Duration::from_secs(2);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A connected WING.
pub struct WingSurface {
    client: Arc<NativeClient>,
    index: Index,
    info: SurfaceInfo,
    subs: Subs,
    own_writes: OwnWrites,
    meters: Mutex<Option<Arc<MeterEngine>>>,
    timeout: Duration,
    solo: Vec<SoloLeaf>,
    solo_cfg: Vec<NodeId>,
    /// Solo mode before [`ConformanceDriver::set_destructive_solo`] switched it (driver role only).
    driver_prev_mode: Mutex<Option<String>>,
}

impl WingSurface {
    /// Connect to the native port at `addr` (`ip:2222` on a real console), sync the tree and start events.
    pub fn connect(addr: SocketAddr, opts: &ConnectOptions) -> SurfaceResult<WingSurface> {
        let identity = opts.discovery_port.and_then(|port| {
            crate::discovery::probe(SocketAddr::new(addr.ip(), port), Duration::from_millis(300))
                .ok()
                .and_then(|v| v.into_iter().next())
        });
        let (client, rx) = NativeClient::connect(addr, opts.timeout).map_err(io)?;
        client.sync(opts.sync_timeout).map_err(io)?;
        let index = Index::build(client.store());
        let subs: Subs = Arc::new(Mutex::new(Vec::new()));
        let own_writes: OwnWrites = Arc::new(Mutex::new(HashMap::new()));
        {
            let subs = subs.clone();
            let own = own_writes.clone();
            let weak = Arc::downgrade(&client);
            let link = Link {
                policy: opts.reconnect,
                timeout: opts.timeout,
                sync_timeout: opts.sync_timeout,
            };
            std::thread::Builder::new()
                .name("wing-events".into())
                .spawn(move || dispatch(rx, weak, subs, own, link))
                .map_err(io)?;
        }
        let (name, model, firmware) = match identity {
            Some(i) => (i.identity.name, i.identity.model, i.identity.firmware),
            None => ("WING".into(), "unknown".into(), "unknown".into()),
        };
        let s = Schema::get();
        let solo_cfg = ["cfg/solo/mode", "cfg/solo/mute"]
            .iter()
            .filter_map(|p| s.by_path(p))
            .collect();
        Ok(WingSurface {
            client,
            index,
            info: SurfaceInfo {
                surface: "wing".into(),
                name,
                model,
                firmware,
                capabilities: Capabilities {
                    solo: true,
                    listen_points: LISTEN_POINTS.iter().map(|s| s.to_string()).collect(),
                    meters: Some(MeterCaps {
                        input: true,
                        output: true,
                        pre_fader: false,
                        gain_reduction: true,
                        solo_bus: true,
                    }),
                    rta: Some(RtaCaps {
                        bands_hz: rta_bands_hz(),
                    }),
                    external_fx_models: map::EXTERNAL_FX_MODELS
                        .iter()
                        .map(|s| s.to_string())
                        .collect(),
                },
            },
            subs,
            own_writes,
            meters: Mutex::new(None),
            timeout: opts.timeout,
            solo: solo_leaves(),
            solo_cfg,
            driver_prev_mode: Mutex::new(None),
        })
    }

    /// The native client (mirror store, raw gets/sets).
    pub fn client(&self) -> &NativeClient {
        &self.client
    }

    fn store(&self) -> &ParamStore {
        self.client.store()
    }

    fn alive(&self) -> SurfaceResult<()> {
        if self.client.is_alive() {
            Ok(())
        } else {
            Err(SurfaceError::Disconnected)
        }
    }

    fn resolve(&self, param: &ParamId) -> SurfaceResult<(NodeId, Mapped)> {
        let st = self.store();
        if param.as_str().contains("/preamp/") {
            return map::resolve_preamp_alias(st, param)
                .ok_or_else(|| SurfaceError::UnknownParam(param.clone()));
        }
        let node = self
            .index
            .node(st, param)
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))?;
        let m = map::map_node(st, node)
            .filter(|m| &m.id == param)
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))?;
        Ok((node, m))
    }

    fn spec_of(&self, node: NodeId, m: &Mapped) -> SurfaceResult<ParamSpec> {
        let d = self
            .store()
            .desc(node)
            .ok_or_else(|| SurfaceError::UnknownParam(m.id.clone()))?;
        Ok(convert::spec(m, d))
    }

    /// Re-read params from the console (fresh gets) and return their values.
    pub fn refresh(&self, params: &[ParamId]) -> SurfaceResult<Vec<Value>> {
        let nodes: Vec<(NodeId, Mapped)> = params
            .iter()
            .map(|p| self.resolve(p))
            .collect::<Result<_, _>>()?;
        let ids: Vec<NodeId> = nodes.iter().map(|x| x.0).collect();
        self.client.refresh(&ids, self.timeout).map_err(io)?;
        nodes
            .iter()
            .map(|(n, m)| {
                convert::read(self.store(), *n, m)
                    .ok_or_else(|| SurfaceError::UnknownParam(m.id.clone()))
            })
            .collect()
    }

    /// Model token of the block a param belongs to (`input.1/gate/attack` -> `GATE`), if it is a model block.
    pub fn block_model(&self, param: &ParamId) -> Option<String> {
        let (node, _) = self.resolve(param).ok()?;
        let s = Schema::get();
        let block = s.parent(node)?;
        let g = s.group_of_block(block)?;
        let sel = s.child(block, g.selector)?;
        let mut t = String::new();
        self.store().format_token(sel, &mut t).ok()?;
        Some(t)
    }

    /// Convert a generic knowledge-base value for `param` to what this WING accepts (10-90 % times for GATE/DUCK
    /// attack/release, clamped into range). See [`convert::knowledge_to_wing`].
    pub fn adapt_value(&self, param: &ParamId, value: f64) -> SurfaceResult<Value> {
        let spec = self.param_spec(param)?;
        let model = self.block_model(param);
        Ok(Value::Float(convert::knowledge_to_wing(
            &spec,
            model.as_deref(),
            value,
        )))
    }

    /// Remember that we are about to write config node `n` ourselves: its echo is not an engineer move.
    fn mark_own(&self, n: NodeId) {
        let sc = Schema::get();
        let mut g = lock(&self.own_writes);
        g.retain(|_, t| t.elapsed() < OWN_WRITE_WINDOW);
        let now = std::time::Instant::now();
        g.insert(n, now);
        // the console mirrors rtasrc/rtatap into the live `$src`/`$tap` leaves, which change right after
        for (cfg, live) in [
            ("cfg/rta/rtasrc", "cfg/rta/$src"),
            ("cfg/rta/rtatap", "cfg/rta/$tap"),
        ] {
            if sc.by_path(cfg) == Some(n) {
                if let Some(l) = sc.by_path(live) {
                    g.insert(l, now);
                }
            }
        }
    }

    /// Set a node's raw value on the mirror and the console (inspection/config writes that bypass the param map).
    fn set_node(&self, node: NodeId, v: WValue) -> SurfaceResult<()> {
        self.alive()?;
        self.mark_own(node);
        let _g = self.client.write_guard();
        self.store()
            .set_from(node, v, ORIGIN_LOCAL)
            .map_err(|e| SurfaceError::Refused(e.to_string()))?;
        self.client.send_value(node).map_err(io)
    }

    fn set_token(&self, node: NodeId, tok: &str) -> SurfaceResult<()> {
        self.alive()?;
        self.mark_own(node);
        let _g = self.client.write_guard();
        self.store()
            .set_enum_name(node, tok, ORIGIN_LOCAL)
            .map_err(|e| SurfaceError::Refused(e.to_string()))?;
        self.client.send_value(node).map_err(io)
    }

    fn mode_destructive(&self) -> bool {
        let st = self.store();
        let r = topology::Reader(st);
        match r.token("cfg/solo/mode").as_deref() {
            Some("SIP") => true,
            Some("STUDIO") => r.on("cfg/solo/mute"),
            _ => false,
        }
    }

    fn solo_from_mirror(&self) -> SoloState {
        let st = self.store();
        let mut active = BTreeSet::new();
        for l in &self.solo {
            if st.get_f64(l.node).unwrap_or(0.0) == 0.0 {
                continue;
            }
            let t = match (l.point, l.band) {
                (None, _) => SoloTarget::strip(l.strip.clone()),
                (Some("eq"), Some(b)) => SoloTarget::Listen {
                    strip: l.strip.clone(),
                    point: format!("eq.band.{}", st.get_f64(b).unwrap_or(1.0) as i64),
                },
                (Some(p), _) => SoloTarget::Listen {
                    strip: l.strip.clone(),
                    point: p.to_string(),
                },
            };
            active.insert(t);
        }
        SoloState {
            mode: if self.mode_destructive() {
                SoloMode::Destructive
            } else {
                SoloMode::NonDestructive
            },
            active: active.into_iter().collect(),
        }
    }

    fn refresh_solo(&self) -> SurfaceResult<()> {
        self.alive()?;
        let mut ids: Vec<NodeId> = self.solo_cfg.clone();
        for l in &self.solo {
            ids.push(l.node);
            ids.extend(l.band);
        }
        self.client.refresh(&ids, self.timeout).map_err(io)
    }

    fn solo_leaf(&self, t: &SoloTarget) -> SurfaceResult<(&SoloLeaf, Option<i32>)> {
        match t {
            SoloTarget::Strip { strip } => self
                .solo
                .iter()
                .find(|l| &l.strip == strip && l.point.is_none())
                .map(|l| (l, None))
                .ok_or_else(|| SurfaceError::UnknownStrip(strip.clone())),
            SoloTarget::Listen { strip, point } => {
                let (key, band) = match point.strip_prefix("eq.band.") {
                    Some(n) => (
                        "eq",
                        Some(
                            n.parse::<i32>()
                                .map_err(|_| SurfaceError::Unsupported("listen point"))?,
                        ),
                    ),
                    None => (point.as_str(), None),
                };
                if !self.solo.iter().any(|l| &l.strip == strip) {
                    return Err(SurfaceError::UnknownStrip(strip.clone()));
                }
                let leaf = self
                    .solo
                    .iter()
                    .find(|l| &l.strip == strip && l.point == Some(key))
                    .ok_or(SurfaceError::Unsupported("listen point on this strip"))?;
                if let (Some(b), Some(bn)) = (band, leaf.band) {
                    let d = self
                        .store()
                        .desc(bn)
                        .ok_or(SurfaceError::Unsupported("eq band"))?;
                    if (b as f64) < d.min || (b as f64) > d.max {
                        return Err(SurfaceError::Unsupported("eq band out of range"));
                    }
                }
                Ok((leaf, band))
            }
        }
    }

    fn meter_engine(&self) -> SurfaceResult<Arc<MeterEngine>> {
        self.alive()?;
        let mut g = lock(&self.meters);
        if let Some(e) = g.as_ref() {
            return Ok(e.clone());
        }
        let e = MeterEngine::start(self.client.clone()).map_err(io)?;
        *g = Some(e.clone());
        Ok(e)
    }

    fn rta_nodes(&self) -> SurfaceResult<(NodeId, NodeId)> {
        let s = Schema::get();
        match (s.by_path("cfg/rta/rtasrc"), s.by_path("cfg/rta/rtatap")) {
            (Some(a), Some(b)) => Ok((a, b)),
            _ => Err(SurfaceError::Unsupported("rta")),
        }
    }

    /// Point the console RTA at `src` and restore the previous `rtasrc`/`rtatap` exactly when the claim drops.
    pub fn claim_rta(&self, src: &RtaSource) -> SurfaceResult<RtaClaim<'_>> {
        let (a, b) = self.rta_nodes()?;
        self.client.refresh(&[a, b], self.timeout).map_err(io)?;
        let st = self.store();
        let saved = (st.get_raw(a), st.get_raw(b));
        self.set_rta_source(Some(src))?;
        Ok(RtaClaim {
            surface: self,
            saved,
        })
    }

    fn restore_rta_raw(&self, saved: (u32, u32)) -> SurfaceResult<()> {
        let (a, b) = self.rta_nodes()?;
        for (n, raw) in [(b, saved.1), (a, saved.0)] {
            self.mark_own(n);
            let _g = self.client.write_guard();
            self.store()
                .set_raw(n, raw, ORIGIN_LOCAL)
                .map_err(|e| SurfaceError::Refused(e.to_string()))?;
            self.client.send_value(n).map_err(io)?;
        }
        Ok(())
    }
}

/// An RTA source claim; restores the previous source on drop.
pub struct RtaClaim<'a> {
    surface: &'a WingSurface,
    saved: (u32, u32),
}

impl RtaClaim<'_> {
    /// Restore now (also done on drop).
    pub fn release(self) -> SurfaceResult<()> {
        let r = self.surface.restore_rta_raw(self.saved);
        std::mem::forget(self);
        r
    }
}

impl Drop for RtaClaim<'_> {
    fn drop(&mut self) {
        let _ = self.surface.restore_rta_raw(self.saved);
    }
}

impl Drop for WingSurface {
    fn drop(&mut self) {
        if let Some(e) = lock(&self.meters).take() {
            e.stop();
        }
        self.client.close();
    }
}

fn io(e: std::io::Error) -> SurfaceError {
    match e.kind() {
        std::io::ErrorKind::NotConnected => SurfaceError::Disconnected,
        _ => SurfaceError::Io(e.to_string()),
    }
}

fn topology_relevant(m: &Mapped) -> bool {
    use ParamClass as C;
    match m.class {
        C::Metadata => m.id.leaf() == "name",
        C::Tags
        | C::SendOn
        | C::SendTap
        | C::RoutingInputPatch
        | C::RoutingOutputPatch
        | C::InsertSlot
        | C::InsertEnable
        | C::FxModel
        | C::Talkback
        | C::ProcessingOrder
        | C::TapPoint => true,
        _ => false,
    }
}

/// Reconnect settings handed to the event thread.
struct Link {
    policy: Option<ReconnectPolicy>,
    timeout: Duration,
    sync_timeout: Duration,
}

fn broadcast(subs: &Subs, events: Vec<SurfaceEvent>) {
    let mut g = lock(subs);
    for ev in events {
        g.retain(|tx| tx.send(ev.clone()).is_ok());
    }
}

/// Retry [`NativeClient::reconnect`] with backoff until it works or the surface is dropped. `true` on success.
fn reconnect_loop(client: &Weak<NativeClient>, link: &Link, policy: ReconnectPolicy) -> bool {
    let mut delay = policy.initial;
    loop {
        // sleep in short slices so a dropped surface ends the loop promptly
        let until = std::time::Instant::now() + delay;
        while std::time::Instant::now() < until {
            if client.upgrade().is_none_or(|c| c.is_stopped()) {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        let Some(c) = client.upgrade() else {
            return false;
        };
        if c.is_stopped() {
            return false;
        }
        if c.reconnect(link.timeout, link.sync_timeout).is_ok() {
            return true;
        }
        delay = (delay * 2).min(policy.max);
    }
}

fn dispatch(
    rx: Receiver<ClientMsg>,
    client: Weak<NativeClient>,
    subs: Subs,
    own: OwnWrites,
    link: Link,
) {
    let s = Schema::get();
    let solo_names = |id: NodeId| map::is_solo_leaf(s.name(id)) || s.path(id) == "$stat/solo";
    let mut above_floor: HashMap<u16, bool> = HashMap::new();
    while let Ok(first) = rx.recv() {
        let mut batch = vec![first];
        while let Ok(m) = rx.try_recv() {
            batch.push(m);
        }
        let Some(c) = client.upgrade() else { break };
        let st = c.store();
        let mut events = Vec::new();
        let (mut topo, mut solo, mut disc) = (false, false, false);
        for msg in batch {
            let ch = match msg {
                ClientMsg::Change(ch) => ch,
                ClientMsg::Overridden(ch) => {
                    // M068: the engineer's write that our in-flight write overwrote on the console: theirs, not an
                    // echo of ours (the mirror keeps our value, the console's final state)
                    let id = ch.node();
                    if let Some(m) = map::map_node(st, id) {
                        if let Some(value) = convert::read_raw(st, id, &m, ch.raw) {
                            events.push(SurfaceEvent::ParamChanged {
                                param: m.id,
                                value,
                                origin: Origin::External,
                            });
                        }
                    }
                    continue;
                }
                ClientMsg::Disconnected => {
                    // a stale message from a connection that has since been replaced is not a disconnect
                    disc |= !c.is_alive();
                    continue;
                }
            };
            let id = ch.node();
            let echo = ch.origin == ORIGIN_REMOTE
                && lock(&own)
                    .get(&id)
                    .is_some_and(|t| t.elapsed() < OWN_WRITE_WINDOW);
            let origin = if ch.origin == ORIGIN_REMOTE && !echo {
                Origin::External
            } else {
                Origin::Local
            };
            if solo_names(id) {
                solo = true;
                continue;
            }
            if map::is_model_selector(id) {
                if let Some((strip, block)) = map::remap_block_name(id) {
                    let mut model = String::new();
                    let _ = st.format_token(id, &mut model);
                    events.push(SurfaceEvent::ParamsRemapped {
                        strip,
                        block: block.to_string(),
                        model,
                    });
                }
            }
            // re-patching (or switching to the alt source) moves the strip's preamp aliases to another source
            if let Some(strip) = map::patch_strip_of(id) {
                topo = true;
                for (n, m) in map::preamp_aliases(st, &strip) {
                    if let Some(value) = convert::read(st, n, &m) {
                        events.push(SurfaceEvent::ParamChanged {
                            param: m.id,
                            value,
                            origin,
                        });
                    }
                }
            }
            let Some(m) = map::map_node(st, id) else {
                continue;
            };
            for (_, am) in map::aliases_of_source(st, id) {
                if let Some(value) = convert::read(st, id, &am) {
                    events.push(SurfaceEvent::ParamChanged {
                        param: am.id,
                        value,
                        origin,
                    });
                }
            }
            if m.class == ParamClass::SendLevel {
                let v = st.get_f64(id).unwrap_or(-144.0) > SEND_FLOOR_DB;
                if above_floor.insert(ch.id, v).is_some_and(|old| old != v) {
                    topo = true;
                }
            }
            if map::is_model_selector(id) && m.class == ParamClass::FxModel {
                topo = true;
            }
            topo |= topology_relevant(&m);
            if let Some(value) = convert::read(st, id, &m) {
                events.push(SurfaceEvent::ParamChanged {
                    param: m.id,
                    value,
                    origin,
                });
            }
        }
        if topo {
            events.push(SurfaceEvent::TopologyChanged);
        }
        if solo {
            events.push(SurfaceEvent::SoloChanged);
        }
        if disc {
            events.push(SurfaceEvent::Disconnected);
        }
        drop(c);
        broadcast(&subs, events);
        if disc {
            if let Some(policy) = link.policy {
                if reconnect_loop(&client, &link, policy) {
                    above_floor.clear();
                    broadcast(
                        &subs,
                        vec![SurfaceEvent::Reconnected, SurfaceEvent::TopologyChanged],
                    );
                }
            }
        }
    }
}

impl Surface for WingSurface {
    fn info(&self) -> SurfaceInfo {
        self.info.clone()
    }

    fn topology(&self) -> SurfaceResult<Topology> {
        self.alive()?;
        Ok(topology::build(self.store()))
    }

    fn params(&self, strip: &StripId) -> SurfaceResult<Vec<ParamSpec>> {
        let s = Schema::get();
        let root = map::strip_path(strip)
            .and_then(|p| s.by_path(&p))
            .ok_or_else(|| SurfaceError::UnknownStrip(strip.clone()))?;
        let st = self.store();
        let mut out = Vec::new();
        let mut stack = vec![root];
        let mut order = Vec::new();
        while let Some(n) = stack.pop() {
            if s.is_param(n) {
                order.push(n);
            } else {
                let kids: Vec<NodeId> = s.children(n).collect();
                stack.extend(kids.into_iter().rev());
            }
        }
        for n in order {
            let Some(m) = map::map_node(st, n) else {
                continue;
            };
            if m.id.strip().as_ref() != Some(strip) {
                continue;
            }
            out.push(self.spec_of(n, &m)?);
        }
        // `<strip>/preamp/{gain,phantom}`: the source feeding the strip, resolved through its patch
        for (n, m) in map::preamp_aliases(st, strip) {
            out.push(self.spec_of(n, &m)?);
        }
        Ok(out)
    }

    fn param_spec(&self, param: &ParamId) -> SurfaceResult<ParamSpec> {
        let (n, m) = self.resolve(param)?;
        self.spec_of(n, &m)
    }

    fn read(&self, param: &ParamId) -> SurfaceResult<Value> {
        let (n, m) = self.resolve(param)?;
        convert::read(self.store(), n, &m).ok_or_else(|| SurfaceError::UnknownParam(param.clone()))
    }

    fn write(&self, param: &ParamId, value: Value) -> SurfaceResult<Value> {
        let (n, m) = self.resolve(param)?;
        let spec = self.spec_of(n, &m)?;
        if spec.read_only {
            return Err(SurfaceError::ReadOnly(param.clone()));
        }
        spec.coerce(&value)?;
        self.alive()?;
        let d = self
            .store()
            .desc(n)
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))?;
        let w = convert::to_wire(&m, d, &value).ok_or(SurfaceError::TypeMismatch {
            param: param.clone(),
            got: value.type_name(),
        })?;
        let g = self.client.write_guard();
        convert::apply(self.store(), n, &w, ORIGIN_LOCAL)
            .map_err(|e| SurfaceError::Refused(format!("{param}: {e}")))?;
        self.client.send_value(n).map_err(io)?;
        drop(g);
        convert::read(self.store(), n, &m).ok_or_else(|| SurfaceError::UnknownParam(param.clone()))
    }

    /// One wire write for the whole batch (M064): every entry is validated and applied to the mirror, then the
    /// accepted values go out together with a single barrier.
    fn write_batch(&self, writes: &[(ParamId, Value)]) -> Vec<SurfaceResult<Value>> {
        if let Err(e) = self.alive() {
            return writes.iter().map(|_| Err(e.clone())).collect();
        }
        let g = self.client.write_guard();
        let staged: Vec<SurfaceResult<(NodeId, Mapped)>> = writes
            .iter()
            .map(|(param, value)| {
                let (n, m) = self.resolve(param)?;
                let spec = self.spec_of(n, &m)?;
                if spec.read_only {
                    return Err(SurfaceError::ReadOnly(param.clone()));
                }
                spec.coerce(value)?;
                let d = self
                    .store()
                    .desc(n)
                    .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))?;
                let w = convert::to_wire(&m, d, value).ok_or(SurfaceError::TypeMismatch {
                    param: param.clone(),
                    got: value.type_name(),
                })?;
                convert::apply(self.store(), n, &w, ORIGIN_LOCAL)
                    .map_err(|e| SurfaceError::Refused(format!("{param}: {e}")))?;
                Ok((n, m))
            })
            .collect();
        let nodes: Vec<NodeId> = staged.iter().flatten().map(|(n, _)| *n).collect();
        let sent = self.client.send_values(&nodes).map_err(io);
        drop(g);
        staged
            .into_iter()
            .zip(writes)
            .map(|(r, (param, _))| {
                let (n, m) = r?;
                sent.clone()?;
                convert::read(self.store(), n, &m)
                    .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))
            })
            .collect()
    }

    fn subscribe(&self) -> Receiver<SurfaceEvent> {
        let (tx, rx) = channel();
        lock(&self.subs).push(tx);
        rx
    }

    fn solo_state(&self) -> SurfaceResult<SoloState> {
        self.refresh_solo()?;
        Ok(self.solo_from_mirror())
    }

    fn set_solo(&self, target: &SoloTarget, on: bool) -> SurfaceResult<()> {
        let (leaf, band) = self.solo_leaf(target)?;
        if on {
            self.client
                .refresh(&self.solo_cfg, self.timeout)
                .map_err(io)?;
            if self.mode_destructive() {
                return Err(SurfaceError::DestructiveSolo);
            }
            if let (Some(b), Some(bn)) = (band, leaf.band) {
                self.set_node(bn, WValue::Int(b))?;
            }
        } else if let (Some(b), Some(bn)) = (band, leaf.band) {
            // turning off a band listen that is not the active band is a no-op
            if self.store().get_f64(bn).unwrap_or(0.0) as i32 != b {
                return Ok(());
            }
        }
        self.set_node(leaf.node, WValue::Int(on as i32))
    }

    fn clear_solo(&self) -> SurfaceResult<()> {
        self.refresh_solo()?;
        let st = self.store();
        let active: Vec<NodeId> = self
            .solo
            .iter()
            .filter(|l| st.get_f64(l.node).unwrap_or(0.0) != 0.0)
            .map(|l| l.node)
            .collect();
        for n in active {
            self.set_node(n, WValue::Int(0))?;
        }
        Ok(())
    }

    fn meters(&self) -> SurfaceResult<MeterFrame> {
        let e = self.meter_engine()?;
        e.latest(Duration::from_secs(2))
            .ok_or_else(|| SurfaceError::Io("no meter data from the console".into()))
    }

    fn subscribe_meters(&self, strips: &[StripId]) -> SurfaceResult<Receiver<MeterFrame>> {
        for s in strips {
            if map::strip_path(s).is_none() || s.kind_name() == "fx" {
                return Err(SurfaceError::UnknownStrip(s.clone()));
            }
        }
        Ok(self.meter_engine()?.subscribe(strips))
    }

    fn rta_source(&self) -> SurfaceResult<Option<RtaSource>> {
        let (a, b) = self.rta_nodes()?;
        self.client.refresh(&[a, b], self.timeout).map_err(io)?;
        let st = self.store();
        let n = st.get_f64(a).unwrap_or(0.0) as i32;
        if n == 0 {
            return Ok(None);
        }
        let mut tok = String::new();
        let _ = st.format_token(b, &mut tok);
        let fx = matches!(tok.as_str(), "FXIN" | "FXOUT");
        let tap = RTA_TAPS
            .iter()
            .find(|(t, _)| *t == tok)
            .map(|(_, g)| g.to_string())
            .unwrap_or_else(|| tok.to_ascii_lowercase());
        Ok(rta_strip(n, fx).map(|strip| RtaSource { strip, tap }))
    }

    /// Exact restore of the raw `rtasrc`/`rtatap` (see the inherent [`WingSurface::claim_rta`]).
    fn claim_rta(&self, source: &RtaSource) -> SurfaceResult<mix_surface::surface::RtaGuard<'_>> {
        let claim = WingSurface::claim_rta(self, source)?;
        Ok(mix_surface::surface::RtaGuard::new(move || drop(claim)))
    }

    fn set_rta_source(&self, source: Option<&RtaSource>) -> SurfaceResult<()> {
        let (a, b) = self.rta_nodes()?;
        match source {
            None => self.set_node(a, WValue::Int(0)),
            Some(src) => {
                let tok = RTA_TAPS
                    .iter()
                    .find(|(_, g)| *g == src.tap)
                    .map(|(t, _)| *t)
                    .ok_or_else(|| SurfaceError::Refused(format!("unknown RTA tap {}", src.tap)))?;
                let fx = matches!(tok, "FXIN" | "FXOUT");
                let n = rta_number(&src.strip, fx)
                    .ok_or_else(|| SurfaceError::UnknownStrip(src.strip.clone()))?;
                self.set_token(b, tok)?;
                self.set_node(a, WValue::Int(n))
            }
        }
    }
}

/// A second `WingSurface` acts as "another client" for the conformance suite.
impl ConformanceDriver for WingSurface {
    fn external_write(&self, param: &ParamId, value: Value) -> bool {
        // a round trip orders the write before whatever the other connection asks the console next
        self.write(param, value).is_ok() && self.client.barrier(self.timeout).is_ok()
    }

    fn set_destructive_solo(&self, on: bool) -> bool {
        let p = ParamId::new("system/cfg/solo/mode");
        if on {
            if let Ok(Value::Enum(prev)) = self.read(&p) {
                *lock(&self.driver_prev_mode) = Some(prev);
            }
            self.write(&p, Value::Enum("SIP".into())).is_ok()
                && self.client.barrier(self.timeout).is_ok()
        } else {
            let prev = lock(&self.driver_prev_mode)
                .take()
                .unwrap_or_else(|| "LIVE".into());
            self.write(&p, Value::Enum(prev)).is_ok() && self.client.barrier(self.timeout).is_ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rta_numbering_round_trips() {
        for (k, n) in [
            ("input", 40),
            ("aux", 8),
            ("bus", 16),
            ("main", 4),
            ("matrix", 8),
        ] {
            for i in 1..=n {
                let s = StripId::new(k, i);
                let num = rta_number(&s, false).unwrap();
                assert_eq!(rta_strip(num, false), Some(s));
            }
        }
        assert_eq!(rta_number(&StripId::new("bus", 1), false), Some(49));
        assert_eq!(rta_strip(12, true), Some(StripId::new("fx", 12)));
        assert!(rta_number(&StripId::new("dca", 1), false).is_none());
    }

    #[test]
    fn solo_leaves_cover_strips_and_listens() {
        let l = solo_leaves();
        let strips = l.iter().filter(|x| x.point.is_none()).count();
        assert_eq!(strips, 40 + 8 + 16 + 4 + 8 + 16);
        assert!(l
            .iter()
            .any(|x| x.strip == StripId::new("input", 1) && x.point == Some("gate.key")));
        assert!(l.iter().any(|x| x.point == Some("eq") && x.band.is_some()));
    }
}

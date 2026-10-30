//! The daemon core: an engine thread that owns the console connection, the input + analysis, the target, the
//! actuator session and the closed-loop controller, and executes control requests between controller ticks.
//!
//! Threads: source (device callback / file player / sim renderer) → SPSC ring → analysis thread
//! ([`crate::analysis`]) → engine thread (this module: requests, `Controller::tick` on the audio clock, events,
//! status.json once a second) → actuator runner thread (`mix_actuator::spawn`, its own 25 Hz clock). STOP and PANIC
//! are sent straight to the actuator handle by the dispatcher, so they never wait behind a plan being computed.
//! Reference/description loads run on a worker thread and report progress as events.

use std::collections::BTreeMap;
use std::net::{SocketAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, sync_channel, Receiver, RecvTimeoutError, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mix_actuator::{
    spawn as spawn_actuator, Actuator, ActuatorConfig, ActuatorEvent, ActuatorHandle, Job, MoveId,
    PolicyGate, SafetyConfig, SafetyGate, SafetyMonitor,
};
use mix_core::bands::{BandLayout, THIRD_OCT_CENTRES_HZ};
use mix_core::ids::{ParamId, StripId};
use mix_core::profile::TargetProfile;
use mix_core::value::Value as PValue;
use mix_engine::control::{ControlConfig, ControlEvent, Controller, Executor, Mode};
use mix_knowledge::KnowledgeBase;
use mix_sim::{Multitrack, SimConfig, SimSurface, Track};
use mix_surface::remit::{detect_topology, Policy, ScoringDetector};
use mix_surface::{Role, Surface};
use serde_json::{json, Value};

use crate::analysis::{lock, Analysis, Meters, SharedMeter};
use crate::events::EventHub;
use crate::gate::{LiveRolesGate, RolesHandle};
use crate::protocol::{
    ConnectConsole, InputKind, LiveContext, LoadDescription, LoadReference, Request, SelectInput,
    SimOptions, StartOptions, TapSel,
};
use crate::rta::RtaFeed;
use crate::server::{ControlServer, Dispatch};
use crate::source::Source;
use crate::status::{atomic_write, unix_now, DaemonStatus, RoleStore, SOCKET_FILE, STATUS_FILE};
use crate::taps::TapLayout;

/// How long a request may wait for the engine thread (a WING full sync can take a while).
pub const EXEC_TIMEOUT: Duration = Duration::from_secs(60);
/// The default WING remit policy (data/remit/wing-default.toml), embedded.
pub const DEFAULT_POLICY: &str = include_str!("../../../data/remit/wing-default.toml");
/// The default X32/M32 remit policy (data/remit/x32-default.toml), embedded.
pub const DEFAULT_POLICY_X32: &str = include_str!("../../../data/remit/x32-default.toml");

/// Embedded default policy of a surface (`SurfaceInfo::surface`); the in-process sim is a WING.
fn default_policy_for(surface: &str) -> Result<Policy, String> {
    let text = match surface {
        "wing" | "sim" => DEFAULT_POLICY,
        "x32" => DEFAULT_POLICY_X32,
        other => {
            return Err(format!(
                "no default remit policy for surface `{other}`: pass --policy"
            ))
        }
    };
    Policy::from_toml_str(text).map_err(|e| e.to_string())
}

/// `--console` spec -> connect request: `sim`, `wing:<ip>[:port]` or `x32:<ip>[:port]`.
pub fn parse_console_spec(spec: &str) -> Result<ConnectConsole, String> {
    let (kind, rest) = spec.split_once(':').unwrap_or((spec, ""));
    match (kind, rest) {
        ("sim", "") => Ok(ConnectConsole {
            console: "sim".into(),
            ..Default::default()
        }),
        ("wing" | "x32", addr) if !addr.is_empty() => {
            // `host:port` (a bare IPv6 address is not supported here)
            let (ip, port) = match addr.rsplit_once(':') {
                Some((h, p)) => (
                    h.to_string(),
                    Some(
                        p.parse::<u16>()
                            .map_err(|_| format!("bad port in `{spec}`"))?,
                    ),
                ),
                None => (addr.to_string(), None),
            };
            Ok(ConnectConsole {
                console: kind.into(),
                ip: Some(ip),
                port,
                ..Default::default()
            })
        }
        _ => Err(format!(
            "bad --console `{spec}` (sim, wing:<ip>[:port] or x32:<ip>[:port])"
        )),
    }
}

/// Daemon settings.
#[derive(Debug, Clone)]
pub struct Config {
    pub state_dir: PathBuf,
    /// Remit policy TOML (default: the embedded WING default).
    pub policy: Option<PathBuf>,
    /// Console to connect at start (`sim`, `wing:<ip>[:port]`, `x32:<ip>[:port]`); `None`: wait for `connect_console`.
    pub console: Option<String>,
    /// Serve `<state-dir>/control.sock`.
    pub socket: bool,
    /// status.json rewrite period.
    pub status_period: Duration,
}

impl Config {
    pub fn new(state_dir: impl Into<PathBuf>) -> Self {
        Config {
            state_dir: state_dir.into(),
            policy: None,
            console: None,
            socket: true,
            status_period: Duration::from_secs(1),
        }
    }
}

enum Msg {
    Req(Request, SyncSender<Result<Value, String>>),
    /// Bookkeeping after the dispatcher executed STOP/PANIC itself.
    After(Request),
    /// SIGINT/SIGTERM: shut down as for `quit`.
    Signal(&'static str),
}

/// What request handlers share with the engine thread.
pub struct Shared {
    tx: Mutex<Sender<Msg>>,
    actuator: Mutex<Option<Arc<ActuatorHandle>>>,
    hub: EventHub,
}

impl Shared {
    fn send(&self, m: Msg) -> bool {
        self.tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .send(m)
            .is_ok()
    }

    fn actuator(&self) -> Option<Arc<ActuatorHandle>> {
        self.actuator
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

impl Dispatch for Shared {
    fn dispatch(&self, req: Request) -> Result<Value, String> {
        match req {
            Request::EmergencyStop => {
                let a = self.actuator().ok_or("no session: nothing to stop")?;
                a.stop();
                self.send(Msg::After(Request::EmergencyStop));
                Ok(json!({"stopped": true}))
            }
            Request::Panic => {
                let a = self.actuator().ok_or("no session: nothing to revert")?;
                let id = a.panic();
                self.send(Msg::After(Request::Panic));
                Ok(json!({"panic": id}))
            }
            Request::Subscribe => Err("subscribe is only available on a socket connection".into()),
            req => {
                let (rtx, rrx) = sync_channel(1);
                if !self.send(Msg::Req(req, rtx)) {
                    return Err("daemon is shutting down".into());
                }
                rrx.recv_timeout(EXEC_TIMEOUT)
                    .unwrap_or_else(|_| Err("daemon did not answer".into()))
            }
        }
    }

    fn events(&self) -> Receiver<String> {
        self.hub.subscribe()
    }
}

/// A running daemon (in-process). Dropping it shuts it down.
pub struct DaemonHandle {
    shared: Arc<Shared>,
    server: Option<ControlServer>,
    join: Option<JoinHandle<()>>,
    state_dir: PathBuf,
}

impl DaemonHandle {
    /// Create the state dir, start the engine thread and (with `cfg.socket`) the control socket.
    pub fn spawn(cfg: Config) -> Result<DaemonHandle, String> {
        std::fs::create_dir_all(&cfg.state_dir)
            .map_err(|e| format!("{}: {e}", cfg.state_dir.display()))?;
        let explicit = match &cfg.policy {
            Some(p) => Some(Policy::load(p).map_err(|e| format!("{}: {e}", p.display()))?),
            None => None,
        };
        let console_req = cfg.console.as_deref().map(parse_console_spec).transpose()?;
        let (tx, rx) = channel();
        let shared = Arc::new(Shared {
            tx: Mutex::new(tx),
            actuator: Mutex::new(None),
            hub: EventHub::new(Some(&cfg.state_dir)),
        });
        let sh = shared.clone();
        let c = cfg.clone();
        let join = std::thread::Builder::new()
            .name("mixd-engine".into())
            .spawn(move || Daemon::new(c, explicit, sh, rx).run())
            .map_err(|e| e.to_string())?;
        if let Some(req) = console_req {
            let (rtx, rrx) = sync_channel(1);
            shared.send(Msg::Req(Request::ConnectConsole(req), rtx));
            let r = rrx
                .recv_timeout(EXEC_TIMEOUT)
                .unwrap_or_else(|_| Err("daemon did not answer".into()));
            if let Err(e) = r {
                shared.send(Msg::Req(Request::Quit, sync_channel(1).0));
                let _ = join.join();
                return Err(e);
            }
        }
        let server = if cfg.socket {
            let d: Arc<dyn Dispatch> = shared.clone();
            match ControlServer::spawn(&cfg.state_dir.join(SOCKET_FILE), d) {
                Ok(s) => Some(s),
                Err(e) => {
                    shared.send(Msg::Req(Request::Quit, sync_channel(1).0));
                    let _ = join.join();
                    return Err(format!("control socket: {e}"));
                }
            }
        } else {
            None
        };
        Ok(DaemonHandle {
            shared,
            server,
            join: Some(join),
            state_dir: cfg.state_dir,
        })
    }

    pub fn request(&self, req: Request) -> Result<Value, String> {
        self.shared.dispatch(req)
    }

    /// Event lines (JSON) from now on.
    pub fn subscribe(&self) -> Receiver<String> {
        self.shared.hub.subscribe()
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    pub fn socket_path(&self) -> Option<&Path> {
        self.server.as_ref().map(|s| s.path())
    }

    /// Block until the daemon quits (a `quit` request) or SIGINT/SIGTERM arrives (see [`crate::signals`]); a
    /// signal makes the engine thread shut down cleanly (STOP, restore inspection solo/RTA, final status), then
    /// dropping the handle removes the socket. A second signal exits at once.
    pub fn wait_or_signal(self) {
        let mut asked = false;
        while self.join.as_ref().is_some_and(|j| !j.is_finished()) {
            let n = crate::signals::count();
            if n >= 2 {
                std::process::exit(130);
            }
            if n >= 1 && !asked {
                asked = true;
                self.shared
                    .send(Msg::Signal(crate::signals::last_name().unwrap_or("signal")));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        self.wait();
    }

    /// Block until the daemon quits (a `quit` request).
    pub fn wait(mut self) {
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Drop for DaemonHandle {
    fn drop(&mut self) {
        if let Some(j) = self.join.take() {
            let (tx, _rx) = sync_channel(1);
            self.shared.send(Msg::Req(Request::Quit, tx));
            let _ = j.join();
        }
        self.server.take();
    }
}

/// Executor over the shared actuator handle.
struct Exec(Arc<ActuatorHandle>);

impl Executor for Exec {
    fn submit(&mut self, job: Job) -> MoveId {
        self.0.submit(job)
    }
    fn confirm(&mut self, key: &str) {
        self.0.confirm(key)
    }
}

struct SimConsole {
    sim: Arc<SimSurface>,
    inputs: Vec<u8>,
    loop_frames: u64,
    speed: f64,
}

struct Console {
    kind: &'static str,
    /// Role-persistence key (console identity).
    key: String,
    scene: String,
    name: String,
    surface: Arc<dyn Surface>,
    sim: Option<SimConsole>,
}

impl Console {
    fn role_key(&self) -> String {
        RoleStore::key(&self.key, &self.scene)
    }
}

struct Input {
    kind: InputKind,
    // Declaration order = drop order: stop the source, then the analysis thread.
    source: Source,
    analysis: Analysis,
}

#[derive(Clone)]
struct Target {
    /// What the controller steers towards: `raw` with the live context applied.
    profile: TargetProfile,
    /// As loaded (reference / description), before any context; the context is re-derived from this.
    raw: TargetProfile,
    kind: &'static str,
    label: String,
    info: Value,
    gen: u64,
}

type Loaded = (TargetProfile, &'static str, String, Value);

enum LoadMsg {
    Progress(String, f64),
    Done(Box<Result<Loaded, String>>),
}

struct Loading {
    what: String,
    stage: String,
    progress: f64,
    rx: Receiver<LoadMsg>,
    waiters: Vec<SyncSender<Result<Value, String>>>,
}

struct Ctl {
    ctl: Controller<'static>,
    rx: Receiver<ControlEvent>,
    meter: SharedMeter,
    gen: u64,
}

struct Session {
    actuator: Arc<ActuatorHandle>,
    monitor: SafetyMonitor,
    act_rx: Receiver<ActuatorEvent>,
    ctl: Option<Ctl>,
    opts: StartOptions,
    running: bool,
    stopped: bool,
    /// First `from` of every param the session changed (the PANIC target).
    baseline: BTreeMap<ParamId, PValue>,
    trips: Vec<Value>,
    feedback_guard: bool,
    /// Confirmed roles seen by the live remit gate (`confirm_roles` publishes here).
    roles: RolesHandle,
    /// The controller was built with older roles; rebuilt between batches.
    roles_stale: bool,
    /// Console RTA -> safety monitor (WING + feedback guard). Last field: dropped (RTA restored) after the
    /// actuator has stopped.
    rta: Option<RtaFeed>,
}

struct Daemon {
    cfg: Config,
    /// The policy loaded from `--policy`: used for every console (checked against it).
    explicit_policy: Option<Policy>,
    /// The policy in force: the explicit one, else the embedded default of the connected console's surface.
    policy: Policy,
    kb: &'static KnowledgeBase,
    shared: Arc<Shared>,
    rx: Receiver<Msg>,
    started: Instant,
    quit: bool,
    last_error: Option<String>,
    roles: RoleStore,
    mode: Mode,
    live_context: LiveContext,
    console: Option<Console>,
    input: Option<Input>,
    target: Option<Target>,
    target_gen: u64,
    loading: Option<Loading>,
    session: Option<Session>,
    next_status: Instant,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl Daemon {
    fn new(
        cfg: Config,
        explicit_policy: Option<Policy>,
        shared: Arc<Shared>,
        rx: Receiver<Msg>,
    ) -> Self {
        let policy = explicit_policy
            .clone()
            .unwrap_or_else(|| default_policy_for("wing").expect("embedded wing policy"));
        let live_context = crate::status::Settings::load(&cfg.state_dir).live_context;
        Daemon {
            roles: RoleStore::load(&cfg.state_dir),
            cfg,
            explicit_policy,
            policy,
            kb: KnowledgeBase::embedded(),
            shared,
            rx,
            started: Instant::now(),
            quit: false,
            last_error: None,
            mode: Mode::Observe,
            live_context,
            console: None,
            input: None,
            target: None,
            target_gen: 0,
            loading: None,
            session: None,
            next_status: Instant::now(),
        }
    }

    fn run(mut self) {
        self.emit(
            "daemon",
            &json!({"type": "started", "pid": std::process::id()}),
        );
        while !self.quit {
            match self.rx.recv_timeout(Duration::from_millis(10)) {
                Ok(m) => self.handle(m),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            while let Ok(m) = self.rx.try_recv() {
                self.handle(m);
                if self.quit {
                    break;
                }
            }
            self.poll_loading();
            self.tick();
            if Instant::now() >= self.next_status {
                self.next_status = Instant::now() + self.cfg.status_period;
                self.write_status(true);
            }
        }
        self.end_session();
        self.input = None;
        self.console = None;
        self.emit("daemon", &json!({"type": "stopped"}));
        self.write_status(false);
    }

    fn now(&self) -> f64 {
        self.input
            .as_ref()
            .map_or(0.0, |i| lock(&i.analysis.meters).clock_s())
    }

    fn emit(&self, source: &str, payload: &impl serde::Serialize) {
        self.shared.hub.emit(source, self.now(), payload);
    }

    fn handle(&mut self, m: Msg) {
        match m {
            Msg::After(Request::Panic) => {
                self.set_mode(Mode::Observe);
                self.emit("daemon", &json!({"type": "panic", "mode": "observe"}));
            }
            Msg::Signal(name) => {
                self.emit("daemon", &json!({"type": "signal", "signal": name}));
                self.quit = true;
            }
            Msg::After(_) => {
                self.emit("daemon", &json!({"type": "emergency_stop"}));
            }
            Msg::Req(req, reply) => {
                let name = req.name();
                match self.request(req, &reply) {
                    Some(Err(e)) => {
                        self.last_error = Some(format!("{name}: {e}"));
                        let _ = reply.send(Err(e));
                    }
                    Some(ok) => {
                        let _ = reply.send(ok);
                    }
                    None => {} // deferred (load with wait)
                }
            }
        }
    }

    /// `None`: the reply is deferred (kept by the handler).
    fn request(
        &mut self,
        req: Request,
        reply: &SyncSender<Result<Value, String>>,
    ) -> Option<Result<Value, String>> {
        Some(match req {
            Request::Ping => Ok(json!({
                "name": "wing-mixd",
                "version": env!("CARGO_PKG_VERSION"),
                "pid": std::process::id(),
            })),
            Request::Status => serde_json::to_value(self.status(true)).map_err(err),
            Request::ListDevices => Ok(self.list_devices()),
            Request::ListConsoles {
                discover,
                wait_ms,
                probe,
            } => self.list_consoles(discover, wait_ms, probe),
            Request::ConnectConsole(c) => self.connect(c),
            Request::DisconnectConsole => {
                self.disconnect();
                Ok(json!({"disconnected": true}))
            }
            Request::SelectInput(s) => self.select_input(s),
            Request::LoadReference(r) => {
                let wait = r.wait;
                let res = self.load_reference(r);
                return self.deferred(res, wait, reply);
            }
            Request::LoadDescription(d) => {
                let wait = d.wait;
                let res = self.load_description(d);
                return self.deferred(res, wait, reply);
            }
            Request::ListRoles => self.list_roles(),
            Request::ConfirmRoles { roles, scene } => self.confirm_roles(roles, scene),
            Request::SetMode { mode } => {
                self.set_mode(mode);
                Ok(json!({"mode": mode}))
            }
            Request::SetLiveContext { context } => self.set_live_context(context),
            Request::Start(o) => self.start(o),
            Request::Stop => self.stop(),
            Request::EmergencyStop | Request::Panic => Err("handled by the dispatcher".into()),
            Request::Resume => self.with_session(|s| {
                s.actuator.resume();
                Ok(json!({"resumed": true}))
            }),
            Request::Confirm { key } => self.with_session(|s| {
                let mut exec = Exec(s.actuator.clone());
                match &mut s.ctl {
                    Some(c) => c.ctl.confirm(&key, &mut exec),
                    None => exec.confirm(&key),
                }
                Ok(json!({"confirmed": key}))
            }),
            Request::Pending => Ok(self.pending()),
            Request::Undo => {
                self.with_session(|s| Ok(json!({"job": s.actuator.submit(Job::Undo)})))
            }
            Request::Snapshot => Ok(self.snapshot()),
            Request::Strips => self.strips(),
            Request::Params { ids } => self.params(&ids),
            Request::Subscribe => Err("subscribe is only available on a socket connection".into()),
            Request::Quit => {
                self.quit = true;
                Ok(json!({"stopping": true}))
            }
        })
    }

    fn with_session(
        &mut self,
        f: impl FnOnce(&mut Session) -> Result<Value, String>,
    ) -> Result<Value, String> {
        f(self.session.as_mut().ok_or("no session")?)
    }

    fn deferred(
        &mut self,
        res: Result<Value, String>,
        wait: bool,
        reply: &SyncSender<Result<Value, String>>,
    ) -> Option<Result<Value, String>> {
        match (res, &mut self.loading) {
            (Ok(_), Some(l)) if wait => {
                l.waiters.push(reply.clone());
                None
            }
            (res, _) => Some(res),
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Consoles and inputs

    fn list_devices(&self) -> Value {
        let mut out = json!({"devices": []});
        match mix_capture::list_input_devices() {
            Ok(list) => {
                out["devices"] = list
                    .into_iter()
                    .map(|d| {
                        json!({
                            "name": d.name,
                            "default": d.is_default,
                            "max_channels": d.max_channels,
                            "min_sample_rate": d.min_sample_rate,
                            "max_sample_rate": d.max_sample_rate,
                            "default_sample_rate": d.default_sample_rate,
                        })
                    })
                    .collect();
            }
            Err(e) => out["error"] = e.to_string().into(),
        }
        if let Some(sim) = self.console.as_ref().and_then(|c| c.sim.as_ref()) {
            let mut taps = vec!["main".to_string(), "solo".to_string()];
            taps.extend(sim.inputs.iter().map(|c| format!("input.{c}")));
            out["sim"] = json!({"taps": taps});
        }
        out
    }

    fn list_consoles(
        &self,
        discover: bool,
        wait_ms: Option<u64>,
        probe: Option<String>,
    ) -> Result<Value, String> {
        let wait = Duration::from_millis(wait_ms.unwrap_or(800));
        let mut found = Vec::new();
        if discover {
            found.extend(mix_wing::discovery::discover(wait).map_err(err)?);
        }
        if let Some(p) = probe {
            found.extend(mix_wing::discovery::probe(resolve(&p)?, wait).map_err(err)?);
        }
        let mut consoles = vec![json!({"kind": "sim", "name": "embedded wing-sim"})];
        consoles.extend(found.into_iter().map(|d| {
            json!({
                "kind": "wing",
                "ip": d.ip,
                "name": d.identity.name,
                "model": d.identity.model,
                "serial": d.identity.serial,
                "firmware": d.identity.firmware,
            })
        }));
        Ok(json!({"consoles": consoles}))
    }

    fn connect(&mut self, c: ConnectConsole) -> Result<Value, String> {
        let scene = c.scene.clone().unwrap_or_else(|| "default".into());
        let console = match c.console.as_str() {
            "sim" => connect_sim(c.sim.unwrap_or_default(), scene)?,
            "x32" => {
                let ip = c.ip.clone().ok_or("connect x32 needs `ip`")?;
                connect_x32(&ip, c.port, scene)?
            }
            other => {
                let ip = match (other, &c.ip) {
                    ("wing", Some(ip)) => ip.clone(),
                    ("wing", None) => return Err("connect wing needs `ip`".into()),
                    (ip, _) => ip.to_string(),
                };
                connect_wing(&ip, c.port, scene)?
            }
        };
        // The policy must be written for this console (F5): the explicit `--policy`, else the surface's default.
        let info = console.surface.info();
        let policy = match &self.explicit_policy {
            Some(p) => p.clone(),
            None => default_policy_for(&info.surface)?,
        };
        policy.check_surface(&info).map_err(err)?;
        self.policy = policy;
        self.disconnect();
        let out = json!({
            "kind": console.kind,
            "name": console.name,
            "key": console.key,
            "scene": console.scene,
            "inputs": console.sim.as_ref().map(|s| s.inputs.clone()),
        });
        self.console = Some(console);
        self.emit(
            "daemon",
            &json!({"type": "console_connected", "console": out}),
        );
        Ok(out)
    }

    fn disconnect(&mut self) {
        self.end_session();
        if self
            .input
            .as_ref()
            .is_some_and(|i| i.kind == InputKind::Sim)
        {
            self.input = None;
        }
        if self.console.take().is_some() {
            self.emit("daemon", &json!({"type": "console_disconnected"}));
        }
    }

    fn select_input(&mut self, s: SelectInput) -> Result<Value, String> {
        let default_taps = || {
            vec![TapSel {
                tap: "main".into(),
                channels: vec![1, 2],
            }]
        };
        let (source, consumer) = match s.source {
            InputKind::Sim => {
                if s.taps.is_some() {
                    return Err("sim input taps are fixed (main, solo, every input)".into());
                }
                let sim = self
                    .console
                    .as_ref()
                    .and_then(|c| c.sim.as_ref())
                    .ok_or("the sim input needs the sim console (connect_console sim)")?;
                Source::sim(sim.sim.clone(), &sim.inputs, sim.loop_frames, sim.speed)?
            }
            InputKind::Device => {
                let layout = TapLayout::from_selection(&s.taps.unwrap_or_else(default_taps))?;
                Source::device(s.device.as_deref(), s.sample_rate, layout)?
            }
            InputKind::File => {
                let path = s.path.as_deref().ok_or("file input needs `path`")?;
                let layout = TapLayout::from_selection(&s.taps.unwrap_or_else(default_taps))?;
                Source::file(Path::new(path), layout)?
            }
        };
        let meters = Meters::new(&source.layout, source.sample_rate)?;
        let surface = self.console.as_ref().map(|c| c.surface.clone());
        let analysis = Analysis::spawn(consumer, meters, surface)?;
        let out = json!({
            "kind": s.source,
            "description": source.description,
            "sample_rate": source.sample_rate,
            "taps": source.layout.labels(),
        });
        if let Some(sess) = &self.session {
            lock(&analysis.meters).safety = Some(sess.monitor.clone());
        }
        self.input = Some(Input {
            kind: s.source,
            source,
            analysis,
        });
        if self.session.as_ref().is_some_and(|s| s.ctl.is_some()) {
            self.build_ctl()?;
        }
        self.emit("daemon", &json!({"type": "input_selected", "input": out}));
        Ok(out)
    }

    // ---------------------------------------------------------------------------------------------------------
    // Targets

    fn begin_load(&mut self, what: String) -> Sender<LoadMsg> {
        let (tx, rx) = channel();
        if let Some(old) = self.loading.take() {
            for w in old.waiters {
                let _ = w.send(Err("superseded by another load".into()));
            }
        }
        self.loading = Some(Loading {
            what: what.clone(),
            stage: "queued".into(),
            progress: 0.0,
            rx,
            waiters: Vec::new(),
        });
        self.emit("daemon", &json!({"type": "load_started", "what": what}));
        tx
    }

    fn load_reference(&mut self, r: LoadReference) -> Result<Value, String> {
        let path = PathBuf::from(&r.path);
        if !path.is_file() {
            return Err(format!("{}: no such file", path.display()));
        }
        let tx = self.begin_load(format!("reference {}", r.path));
        let state = self.cfg.state_dir.clone();
        std::thread::Builder::new()
            .name("mixd-load".into())
            .spawn(move || {
                let res = load_reference_job(&path, &r, &state, &tx);
                let _ = tx.send(LoadMsg::Done(Box::new(res)));
            })
            .map_err(err)?;
        Ok(json!({"loading": true}))
    }

    fn load_description(&mut self, d: LoadDescription) -> Result<Value, String> {
        let base = match d.base.as_deref() {
            None => None,
            Some("current") => Some(
                self.target
                    .as_ref()
                    .ok_or("base `current`: no target loaded")?
                    .raw
                    .clone(),
            ),
            Some(path) => Some(read_profile(Path::new(path))?),
        };
        let base = match (base, &d.genre) {
            (Some(b), _) => Some(b),
            (None, Some(g)) => Some(mix_describe::genre_profile(g).ok_or_else(|| {
                format!(
                    "unknown genre `{g}` (known: {})",
                    mix_describe::genre_names().join(", ")
                )
            })?),
            (None, None) => None,
        };
        let tx = self.begin_load("description".into());
        let kb = self.kb;
        std::thread::Builder::new()
            .name("mixd-describe".into())
            .spawn(move || {
                let _ = tx.send(LoadMsg::Progress("interpreting".into(), 0.1));
                let llm = if d.llm {
                    mix_llm::LlmBridge::new(mix_llm::LlmConfig {
                        enabled: true,
                        model: d.model.clone(),
                        ..mix_llm::LlmConfig::default()
                    })
                } else {
                    mix_llm::LlmBridge::disabled()
                };
                let desc = mix_describe::describe(&d.text, base.as_ref(), kb, &llm);
                let info = json!({
                    "terms": desc.terms.iter().map(|t| json!({
                        "term": t.term, "text": t.text, "from_llm": t.from_llm,
                        "deltas": t.deltas.iter().map(|(f, v)| json!([f, v])).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "conflicts": desc.conflicts,
                    "unrecognised": desc.unrecognised,
                    "notes": desc.notes,
                    "llm_used": desc.llm_used,
                });
                let done = Ok((desc.profile, "description", d.text, info));
                let _ = tx.send(LoadMsg::Done(Box::new(done)));
            })
            .map_err(err)?;
        Ok(json!({"loading": true}))
    }

    fn poll_loading(&mut self) {
        let Some(l) = &mut self.loading else { return };
        let mut done = None;
        let mut progress = Vec::new();
        loop {
            match l.rx.try_recv() {
                Ok(LoadMsg::Progress(stage, p)) => {
                    l.stage = stage.clone();
                    l.progress = p;
                    progress.push((stage, p));
                }
                Ok(LoadMsg::Done(r)) => {
                    done = Some(*r);
                    break;
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    done = Some(Err("loader thread died".to_string()));
                    break;
                }
            }
        }
        let what = l.what.clone();
        for (stage, p) in progress {
            self.emit(
                "daemon",
                &json!({"type": "load_progress", "what": what, "stage": stage, "progress": p}),
            );
        }
        let Some(res) = done else { return };
        let waiters = self.loading.take().map(|l| l.waiters).unwrap_or_default();
        let result = match res {
            Ok((profile, kind, label, info)) => {
                self.target_gen += 1;
                let mut applied = profile.clone();
                mix_describe::apply_context(&mut applied, self.live_context.mix_context());
                self.target = Some(Target {
                    profile: applied,
                    raw: profile,
                    kind,
                    label: label.clone(),
                    info: info.clone(),
                    gen: self.target_gen,
                });
                let v = json!({"kind": kind, "label": label, "info": info});
                self.emit("daemon", &json!({"type": "target_loaded", "target": v}));
                if self.session.as_ref().is_some_and(|s| s.ctl.is_some()) {
                    if let Err(e) = self.build_ctl() {
                        self.last_error = Some(e);
                    }
                }
                Ok(v)
            }
            Err(e) => {
                self.last_error = Some(format!("load: {e}"));
                self.emit(
                    "daemon",
                    &json!({"type": "load_failed", "what": what, "error": e}),
                );
                Err(e)
            }
        };
        for w in waiters {
            let _ = w.send(result.clone());
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Roles

    fn list_roles(&self) -> Result<Value, String> {
        let c = self.console.as_ref().ok_or("no console")?;
        let topo = c.surface.topology().map_err(err)?;
        let det = ScoringDetector::new(self.policy.detection.clone());
        let found = detect_topology(&topo, &self.policy.detection, &det);
        let confirmed = self.roles.roles(&c.role_key());
        let strips: Vec<Value> = found
            .iter()
            .map(|(id, (_, d))| {
                let conf = confirmed.get(id).copied();
                json!({
                    "strip": id,
                    "name": topo.get(id).map(|s| s.name.clone()).unwrap_or_default(),
                    "detected": d.role.as_str(),
                    "effective": conf.unwrap_or(d.effective).as_str(),
                    "confidence": d.confidence,
                    "evidence": d.evidence,
                    "confirmed": conf.map(Role::as_str),
                    "needs_confirmation": d.needs_confirmation && conf.is_none(),
                })
            })
            .collect();
        Ok(json!({"key": c.role_key(), "strips": strips}))
    }

    fn confirm_roles(
        &mut self,
        roles: BTreeMap<String, String>,
        scene: Option<String>,
    ) -> Result<Value, String> {
        let c = self.console.as_mut().ok_or("no console")?;
        if let Some(s) = scene {
            c.scene = s;
        }
        let mut parsed = BTreeMap::new();
        for (s, r) in &roles {
            let id = StripId::parse(s).ok_or_else(|| format!("bad strip id `{s}`"))?;
            let role: Role = r.parse()?;
            parsed.insert(id, role);
        }
        let key = c.role_key();
        self.roles.confirm(&key, &parsed);
        self.roles
            .save(&self.cfg.state_dir)
            .map_err(|e| format!("roles: {e}"))?;
        // Mid-session (M053): the live remit gate sees the new roles on its next decision, so the actuator refuses
        // writes to a bus just confirmed as monitor at once; the controller is rebuilt between batches so its
        // plans use them too (the actuator session, journal and baseline are kept).
        let applies = "now";
        if let Some(s) = &mut self.session {
            let roles = self.roles.roles(&key);
            s.roles.set(roles);
            s.roles_stale = s.ctl.is_some();
        }
        self.emit(
            "daemon",
            &json!({"type": "roles_confirmed", "key": key, "roles": roles, "applies": applies}),
        );
        Ok(json!({"key": key, "confirmed": roles.len(), "applies": applies}))
    }

    // ---------------------------------------------------------------------------------------------------------
    // Session

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        if let Some(c) = self.session.as_mut().and_then(|s| s.ctl.as_mut()) {
            c.ctl.set_mode(mode);
        }
    }

    /// The live-context target setting (M037/M054): `mix_describe::apply_context` over the loaded target (the
    /// raw reference/description profile, so switching is an exact revert), the controller re-plans on the new
    /// target. Applied again on every new reference/description load (`poll_loading`).
    fn set_live_context(&mut self, context: LiveContext) -> Result<Value, String> {
        let changed = self.live_context != context;
        self.live_context = context;
        let applied = self.target.is_some();
        if changed {
            let st = crate::status::Settings {
                live_context: context,
            };
            if let Err(e) = st.save(&self.cfg.state_dir) {
                self.last_error = Some(format!("settings: {e}"));
            }
            if let Some(t) = &mut self.target {
                let mut p = t.raw.clone();
                mix_describe::apply_context(&mut p, context.mix_context());
                t.profile = p;
                self.target_gen += 1;
                t.gen = self.target_gen;
                if self.session.as_ref().is_some_and(|s| s.ctl.is_some()) {
                    if let Err(e) = self.build_ctl() {
                        self.last_error = Some(e);
                    }
                }
            }
            self.emit(
                "daemon",
                &json!({"type": "live_context_changed", "live_context": context, "applied": applied}),
            );
        }
        Ok(json!({
            "live_context": context,
            "applied": applied,
            "target": self.target.as_ref().map(|t| context_summary(&t.profile)),
        }))
    }

    fn start(&mut self, o: StartOptions) -> Result<Value, String> {
        let console = self.console.as_ref().ok_or("connect a console first")?;
        self.policy
            .check_surface(&console.surface.info())
            .map_err(err)?;
        let input = self.input.as_ref().ok_or("select an input first")?;
        if self.target.is_none() {
            return Err("load a reference or a description first".into());
        }
        if let Some(m) = o.mode {
            self.mode = m;
        }
        if self.session.is_none() {
            let feedback_guard = o.feedback_guard.unwrap_or(console.kind != "sim");
            let mut scfg = SafetyConfig::default();
            scfg.feedback.enabled = feedback_guard;
            let monitor = SafetyMonitor::new(scfg);
            let roles = self.roles.roles(&console.role_key());
            let surface = console.surface.clone();
            let handle = RolesHandle::new(roles.clone());
            let inner = PolicyGate::with_roles(self.policy.clone(), &*surface, roles)
                .map_err(|e| format!("remit: {e}"))?;
            let live = LiveRolesGate::new(inner, handle.clone(), surface.clone());
            let gate = SafetyGate::new(Box::new(live), surface.clone(), monitor.clone());
            let mut act = Actuator::new(surface.clone(), Box::new(gate), ActuatorConfig::default());
            act.set_safety(monitor.clone());
            let act_rx = act.subscribe();
            let actuator = Arc::new(spawn_actuator(act));
            // Console RTA -> feedback detector, only when the console has an RTA (`RtaFeed::start` checks the
            // capability). A failed claim is not fatal.
            let rta = match feedback_guard {
                true => match RtaFeed::start(surface.clone(), monitor.clone()) {
                    Ok(r) => r,
                    Err(e) => {
                        self.last_error = Some(format!("rta feed: {e}"));
                        None
                    }
                },
                _ => None,
            };
            lock(&input.analysis.meters).safety = Some(monitor.clone());
            *self
                .shared
                .actuator
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = Some(actuator.clone());
            self.session = Some(Session {
                actuator,
                monitor,
                act_rx,
                ctl: None,
                opts: o.clone(),
                running: false,
                stopped: false,
                baseline: BTreeMap::new(),
                trips: Vec::new(),
                feedback_guard,
                roles: handle,
                roles_stale: false,
                rta,
            });
        }
        let s = self.session.as_mut().expect("session");
        let rebuild = o != StartOptions::default() && o != s.opts || s.ctl.is_none();
        if o != StartOptions::default() {
            s.opts = o;
        }
        if rebuild || s.ctl.as_ref().map(|c| c.gen) != self.target.as_ref().map(|t| t.gen) {
            self.build_ctl()?;
        }
        let s = self.session.as_mut().expect("session");
        if s.stopped || !s.running {
            s.actuator.resume();
        }
        s.running = true;
        let mode = self.mode;
        self.set_mode(mode);
        self.emit("daemon", &json!({"type": "session_started", "mode": mode}));
        Ok(json!({"running": true, "mode": mode}))
    }

    /// (Re)build the controller for the current target, input and options (the actuator session is kept).
    fn build_ctl(&mut self) -> Result<(), String> {
        let (Some(console), Some(input), Some(target), Some(s)) = (
            self.console.as_ref(),
            self.input.as_ref(),
            self.target.as_ref(),
            self.session.as_mut(),
        ) else {
            return Ok(());
        };
        let o = &s.opts;
        let mut cfg = ControlConfig::from_kb(self.kb).with_mode(self.mode);
        if let Some(f) = o.time_scale {
            if f <= 0.0 {
                return Err("time_scale must be > 0".into());
            }
            cfg = cfg.scaled(f);
        }
        if let Some(w) = o.window_s {
            if w <= 0.0 {
                return Err("window_s must be > 0".into());
            }
            cfg.window_s = w;
        }
        if let Some(b) = o.max_batches {
            cfg.max_batches = b;
        }
        if let Some(t) = o.target_objective {
            cfg.target_objective = t;
        }
        let has_solo = lock(&input.analysis.meters).has_solo();
        cfg.inspect = o.inspect.unwrap_or(true) && has_solo;
        let names: BTreeMap<StripId, String> = console
            .surface
            .topology()
            .map(|t| t.strips.into_iter().map(|s| (s.id, s.name)).collect())
            .unwrap_or_default();
        let roles = self.roles.roles(&console.role_key());
        let mut ctl = Controller::new(
            self.kb,
            cfg,
            target.profile.clone(),
            self.policy.clone(),
            console.surface.clone(),
            s.actuator.subscribe(),
        )
        .with_roles(roles);
        let rx = ctl.subscribe();
        s.ctl = Some(Ctl {
            ctl,
            rx,
            meter: SharedMeter::new(input.analysis.meters.clone(), self.kb, names),
            gen: target.gen,
        });
        Ok(())
    }

    fn stop(&mut self) -> Result<Value, String> {
        let s = self.session.as_mut().ok_or("no session")?;
        s.running = false;
        s.actuator.stop();
        self.emit("daemon", &json!({"type": "session_stopped"}));
        Ok(json!({"running": false}))
    }

    fn end_session(&mut self) {
        *self
            .shared
            .actuator
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        if let Some(i) = &self.input {
            lock(&i.analysis.meters).safety = None;
        }
        if self.session.take().is_some() {
            self.emit("daemon", &json!({"type": "session_ended"}));
        }
    }

    fn tick(&mut self) {
        if self
            .session
            .as_ref()
            .is_some_and(|s| s.roles_stale && s.ctl.as_ref().is_some_and(|c| !c.ctl.is_busy()))
        {
            if let Some(s) = self.session.as_mut() {
                s.roles_stale = false;
            }
            if let Err(e) = self.build_ctl() {
                self.last_error = Some(e);
            }
        }
        let now = self.now();
        let Some(s) = &mut self.session else { return };
        let mut events: Vec<(&'static str, Value)> = Vec::new();
        if let Some(c) = &mut s.ctl {
            if s.running {
                let mut exec = Exec(s.actuator.clone());
                c.ctl.tick(now, &mut exec, &mut c.meter);
            }
            while let Ok(e) = c.rx.try_recv() {
                events.push(("control", serde_json::to_value(&e).unwrap_or(Value::Null)));
            }
        }
        while let Ok(e) = s.act_rx.try_recv() {
            match &e {
                ActuatorEvent::Done { changes, .. } | ActuatorEvent::Aborted { changes, .. } => {
                    for c in changes {
                        s.baseline
                            .entry(c.param.clone())
                            .or_insert_with(|| c.from.clone());
                    }
                }
                ActuatorEvent::Stopped => s.stopped = true,
                ActuatorEvent::Ready => s.stopped = false,
                ActuatorEvent::Safety { trip, reverted } => {
                    s.trips
                        .push(json!({"at_s": now, "trip": trip, "reverted": reverted}));
                }
                _ => {}
            }
            events.push(("actuator", serde_json::to_value(&e).unwrap_or(Value::Null)));
        }
        for (src, e) in events {
            self.shared.hub.emit(src, now, &e);
        }
    }

    fn pending(&self) -> Value {
        let p: Vec<Value> = self
            .session
            .as_ref()
            .and_then(|s| s.ctl.as_ref())
            .map(|c| {
                c.ctl
                    .pending()
                    .iter()
                    .map(|p| json!({"key": p.key, "summary": p.summary}))
                    .collect()
            })
            .unwrap_or_default();
        json!({ "pending": p })
    }

    fn params(&self, ids: &[String]) -> Result<Value, String> {
        let c = self.console.as_ref().ok_or("no console")?;
        let mut out = serde_json::Map::new();
        for id in ids {
            let v = c
                .surface
                .read(&ParamId::new(id.as_str()))
                .map_err(|e| format!("{id}: {e}"))?;
            out.insert(id.clone(), serde_json::to_value(v).map_err(err)?);
        }
        Ok(Value::Object(out))
    }

    // ---------------------------------------------------------------------------------------------------------
    // Snapshot and status

    /// Strip names for front ends (`snapshot` entries only carry ids). The model has no strip colours.
    fn strips(&self) -> Result<Value, String> {
        let c = self.console.as_ref().ok_or("no console")?;
        let topo = c.surface.topology().map_err(err)?;
        let strips: Vec<Value> = topo
            .strips
            .iter()
            .filter(|s| !s.name.trim().is_empty())
            .map(|s| json!({"id": s.id, "kind": s.kind, "name": s.name}))
            .collect();
        Ok(json!({"strips": strips}))
    }

    fn snapshot(&self) -> Value {
        let now = self.now();
        let mut out = json!({
            "at_s": now,
            "mode": self.mode,
            "live_context": self.live_context,
            "live_context_applied": self.target.is_some(),
            "console": self.console.as_ref().map(|c| json!({"kind": c.kind, "name": c.name})),
            "target": self.target.as_ref().map(|t| json!({"kind": t.kind, "label": t.label, "info": t.info, "context": context_summary(&t.profile)})),
        });
        let live = self.input.as_ref().and_then(|i| {
            let m = lock(&i.analysis.meters);
            Some((m.live.clone()?, m.momentary.clone()))
        });
        let erb_hz = BandLayout::Erb40.centres_hz();
        if let Some((st, mom)) = &live {
            out["loudness"] = json!({
                "momentary_lufs": mom.as_ref().map(|m| m.loudness.momentary_lufs),
                "short_term_lufs": st.loudness.short_term_lufs,
                "true_peak_dbtp": st.loudness.true_peak_dbtp,
                "crest_db": st.loudness.crest_db,
                "tilt_db_per_oct": st.spectrum.tilt_db_per_oct,
                "width_db": st.stereo.as_ref().map(|s| s.width_db),
                "correlation": st.stereo.as_ref().map(|s| s.correlation),
            });
        }
        let current = live.as_ref().map(|(st, _)| &st.spectrum);
        let target = self.target.as_ref().map(|t| &t.profile.tonal);
        out["spectrum"] = json!({
            "erb_hz": erb_hz,
            "current_erb_rel_db": current.map(|s| s.erb_mid_rel_db.clone()),
            "target_erb_rel_db": target.map(|t| t.erb_mid_rel_db.iter().map(|s| s.p50).collect::<Vec<_>>()),
            "target_erb_lo_db": target.map(|t| t.erb_mid_rel_db.iter().map(|s| s.p10).collect::<Vec<_>>()),
            "target_erb_hi_db": target.map(|t| t.erb_mid_rel_db.iter().map(|s| s.p90).collect::<Vec<_>>()),
            "tolerance_db": target.map(|t| t.tolerance_db.clone()),
            "third_oct_hz": THIRD_OCT_CENTRES_HZ,
            "current_third_oct_db": current.map(|s| s.third_oct_mid_db.clone()),
            "target_third_oct_rel_db": target.map(|t| t.third_oct_rel_db.iter().map(|s| s.p50).collect::<Vec<_>>()),
        });
        let Some(s) = &self.session else {
            out["session"] = Value::Null;
            return out;
        };
        let surface = self.console.as_ref().map(|c| c.surface.clone());
        let touched: Vec<Value> = s
            .baseline
            .iter()
            .map(|(p, b)| {
                let cur = surface.as_ref().and_then(|x| x.read(p).ok());
                json!({"param": p, "baseline": b, "current": cur})
            })
            .collect();
        out["session"] = json!({
            "running": s.running,
            "stopped": s.stopped,
            "feedback_guard": s.feedback_guard,
        });
        out["safety"] = json!({
            "status": s.monitor.status(),
            "trips": s.trips,
            "rta_feed": s.rta.as_ref().map(RtaFeed::status),
        });
        out["touched"] = touched.into();
        out["log"] = self
            .shared
            .hub
            .recent(60, |e| {
                e["source"] == "actuator" && e["type"] != "progress" && e["type"] != "queued"
            })
            .into();
        if let Some(c) = &s.ctl {
            let ctl = &c.ctl;
            out["session"]["holding"] = json!(ctl.holding());
            out["session"]["busy"] = json!(ctl.is_busy());
            out["session"]["budget"] = json!(ctl.budget_used());
            out["score"] = json!(ctl.score());
            out["curve"] = json!(ctl.curve());
            out["problems"] =
                json!(ctl
                    .diagnosis()
                    .map(|d| d.problems.iter().take(20).collect::<Vec<_>>()));
            out["diagnosis_notes"] = json!(ctl.diagnosis().map(|d| d.notes.clone()));
            out["plan"] = json!(ctl.planned());
            out["pending"] = self.pending()["pending"].clone();
            out["moves"] = json!(ctl.records());
        }
        out
    }

    fn status(&self, running: bool) -> DaemonStatus {
        let input = self.input.as_ref().map(|i| {
            let m = lock(&i.analysis.meters);
            json!({
                "kind": i.kind,
                "description": i.source.description,
                "sample_rate": i.source.sample_rate,
                "taps": i.source.layout.labels(),
                "dropped_frames": m.dropped,
                "clock_s": m.clock_s(),
                "stream_errors": i.source.stats.errors(),
            })
        });
        let session = self.session.as_ref().map(|s| {
            let ctl = s.ctl.as_ref().map(|c| &c.ctl);
            json!({
                "running": s.running,
                "stopped": s.stopped,
                "holding": ctl.and_then(|c| c.holding()),
                "objective": ctl.and_then(|c| c.score()).map(|x| x.objective),
                "distance": ctl.and_then(|c| c.score()).map(|x| x.distance),
                "batches": ctl.map(|c| c.records().len()),
                "pending": ctl.map(|c| c.pending().len()),
                "touched": s.baseline.len(),
            })
        });
        DaemonStatus {
            running,
            pid: std::process::id(),
            version: env!("CARGO_PKG_VERSION").into(),
            updated_unix: unix_now(),
            uptime_s: self.started.elapsed().as_secs_f64(),
            state_dir: self.cfg.state_dir.display().to_string(),
            console: self
                .console
                .as_ref()
                .map(|c| json!({"kind": c.kind, "name": c.name, "key": c.key, "scene": c.scene}))
                .unwrap_or(Value::Null),
            input: input.unwrap_or(Value::Null),
            target: self
                .target
                .as_ref()
                .map(|t| json!({"kind": t.kind, "label": t.label}))
                .unwrap_or(Value::Null),
            loading: self
                .loading
                .as_ref()
                .map(|l| json!({"what": l.what, "stage": l.stage, "progress": l.progress}))
                .unwrap_or(Value::Null),
            live_context: self.live_context,
            live_context_applied: self.target.is_some(),
            mode: serde_json::to_value(self.mode)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
            session: session.unwrap_or(Value::Null),
            safety: self
                .session
                .as_ref()
                .map(|s| {
                    json!({
                        "status": s.monitor.status(),
                        "trips": s.trips.len(),
                        "rta_feed": s.rta.as_ref().map(RtaFeed::status),
                    })
                })
                .unwrap_or(Value::Null),
            subscribers: self.shared.hub.subscribers(),
            last_error: self.last_error.clone(),
        }
    }

    fn write_status(&self, running: bool) {
        if let Ok(s) = serde_json::to_string(&self.status(running)) {
            let _ = atomic_write(&self.cfg.state_dir.join(STATUS_FILE), s.as_bytes());
        }
    }
}

fn resolve(s: &str) -> Result<SocketAddr, String> {
    let with_port = if s.contains(':') {
        s.to_string()
    } else {
        format!("{s}:{}", wing_port())
    };
    with_port
        .to_socket_addrs()
        .map_err(|e| format!("{s}: {e}"))?
        .next()
        .ok_or_else(|| format!("{s}: no address"))
}

/// The WING native/discovery port.
fn wing_port() -> u16 {
    mix_wing::ConnectOptions::default()
        .discovery_port
        .unwrap_or(2222)
}

fn connect_wing(ip: &str, port: Option<u16>, scene: String) -> Result<Console, String> {
    let port = port.unwrap_or_else(wing_port);
    let addr = resolve(&format!("{ip}:{port}"))?;
    let opts = mix_wing::ConnectOptions {
        discovery_port: Some(port),
        ..Default::default()
    };
    let s =
        Arc::new(mix_wing::WingSurface::connect(addr, &opts).map_err(|e| format!("{addr}: {e}"))?);
    let info = s.info();
    Ok(Console {
        kind: "wing",
        key: format!("wing:{}:{}", info.model, info.name),
        scene,
        name: format!("{} {} ({addr})", info.model, info.name),
        surface: s,
        sim: None,
    })
}

fn connect_x32(ip: &str, port: Option<u16>, scene: String) -> Result<Console, String> {
    let port = port.unwrap_or(mix_x32::X32_PORT);
    let addr = resolve(&format!("{ip}:{port}"))?;
    let s = Arc::new(
        mix_x32::X32Surface::connect(addr, &mix_x32::ConnectOptions::default())
            .map_err(|e| format!("{addr}: {e}"))?,
    );
    let info = s.info();
    Ok(Console {
        kind: "x32",
        key: format!("x32:{}:{}", info.model, info.name),
        scene,
        name: format!("{} {} ({addr})", info.model, info.name),
        surface: s,
        sim: None,
    })
}

fn connect_sim(o: SimOptions, scene: String) -> Result<Console, String> {
    let (input, default_names): (Multitrack, Vec<String>) = match o.multitrack.as_deref() {
        None | Some("synthetic") => {
            let spec = mix_sim::synth::SongSpec {
                seed: o.seed.unwrap_or(7),
                seconds: o.seconds.unwrap_or(8.0),
                sample_rate: 48_000,
            };
            (
                mix_sim::synth::generate(&spec).0,
                mix_sim::synth::TRACKS
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            )
        }
        Some(path) => {
            let w = mix_capture::wav::read(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
            if w.channels == 0 || w.channels > 40 {
                return Err(format!("{path}: {} channels (1..=40)", w.channels));
            }
            let tracks = (0..w.channels)
                .map(|c| Track {
                    name: format!("CH {}", c + 1),
                    channel: c as u8 + 1,
                    usb: c as u8 + 1,
                    audio: w
                        .samples
                        .iter()
                        .skip(c)
                        .step_by(w.channels)
                        .copied()
                        .collect(),
                })
                .collect();
            (
                Multitrack {
                    sample_rate: w.sample_rate,
                    tracks,
                },
                Vec::new(),
            )
        }
    };
    if input.frames() == 0 {
        return Err("multitrack has no audio".into());
    }
    let channels = input.tracks.len() as u8;
    let snap = match o.scene.as_deref() {
        Some(p) if p.ends_with(".snap") => {
            Some(std::fs::read_to_string(p).map_err(|e| format!("{p}: {e}"))?)
        }
        _ => None,
    };
    let sim = SimSurface::new(SimConfig {
        channels,
        sample_rate: input.sample_rate,
        scene: snap,
        ..SimConfig::default()
    })?;
    let loop_frames = input.frames() as u64;
    sim.load_inputs(input)?;
    let names = o.names.clone().unwrap_or(default_names);
    for (i, n) in names.iter().enumerate().take(channels as usize) {
        sim.set_wing(&format!("ch/{}/name", i + 1), n)?;
    }
    let scene_writes = match o.scene.as_deref() {
        None | Some("factory") => None,
        Some(p) if p.ends_with(".snap") => None,
        Some("reference") => Some(mix_sim::scenes::reference_scene()),
        Some("degraded") => Some(mix_sim::scenes::degraded_scene()),
        Some(other) => {
            return Err(format!(
                "unknown sim scene `{other}` (factory, reference, degraded or a .snap path)"
            ))
        }
    };
    if let Some(sc) = scene_writes {
        mix_sim::scenes::apply(&sim, &sc).map_err(|e| format!("scene {}: {e}", sc.name))?;
    }
    let sim = Arc::new(sim);
    let surface: Arc<dyn Surface> = sim.clone();
    Ok(Console {
        kind: "sim",
        key: "sim".into(),
        scene,
        name: "wing-sim (in-process)".into(),
        surface,
        sim: Some(SimConsole {
            sim,
            inputs: (1..=channels).collect(),
            loop_frames,
            speed: o.speed.unwrap_or(1.0).max(0.0),
        }),
    })
}

/// What the live context did to a target: the context in effect (from `applied_deltas`; `match_record` = none
/// recorded) and the dimensions it moves.
fn context_summary(p: &TargetProfile) -> Value {
    json!({
        "context": mix_describe::context_of(p).unwrap_or(mix_describe::MixContext::MatchRecord),
        "applied": true,
        "plr_db": p.loudness.plr_db,
        "psr_p50_db": p.loudness.psr_db.p50,
        "lra_lu": p.loudness.lra_lu,
        "band_width_p50_db": p.stereo.band_width_db.iter().map(|s| s.p50).collect::<Vec<_>>(),
        "deltas": p.applied_deltas.iter().filter(|d| d.term.starts_with(mix_describe::context::TERM_PREFIX)).count(),
    })
}

fn read_profile(path: &Path) -> Result<TargetProfile, String> {
    let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    mix_reference::profile_from_json(&s).map_err(|e| format!("{}: {e}", path.display()))
}

fn load_reference_job(
    path: &Path,
    r: &LoadReference,
    state: &Path,
    tx: &Sender<LoadMsg>,
) -> Result<Loaded, String> {
    let name = path
        .file_name()
        .map_or_else(|| r.path.clone(), |n| n.to_string_lossy().into_owned());
    if path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
    {
        let _ = tx.send(LoadMsg::Progress("reading profile".into(), 0.5));
        let p = read_profile(path)?;
        return Ok((p, "reference", name, json!({"profile": true})));
    }
    let _ = tx.send(LoadMsg::Progress("analysing".into(), 0.05));
    let cache = mix_reference::ProfileCache::new(state.join("profiles"));
    let (p, stems) = if r.stems {
        let ptx = Mutex::new(tx.clone());
        let sep = mix_stems::StemConfig::from_state_dir(state).map(|c| {
            mix_stems::StemSeparator::new(c).with_progress(move |f| {
                let _ = ptx
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .send(LoadMsg::Progress("stems".into(), 0.1 + 0.8 * f as f64));
            })
        });
        let (p, st) =
            mix_stems::profile_with_stems(path, r.section, Some(&cache), sep).map_err(err)?;
        (p, Some(format!("{st:?}")))
    } else {
        let opts = mix_reference::ProfileOptions {
            section: r.section,
            ..Default::default()
        };
        (
            mix_reference::profile_file(path, &opts, Some(&cache)).map_err(err)?,
            None,
        )
    };
    let info = json!({
        "file": name,
        "sections": p.sections.len(),
        "section": p.section.as_ref().map(|s| s.index),
        "integrated_lufs": p.loudness.integrated_lufs,
        "stems": stems,
        "has_stem_targets": p.stems.is_some(),
    });
    Ok((p, "reference", name, info))
}

//! Test consoles: an in-process `wing_proto` native server (fast, always available) and a headless wing-sim daemon
//! on private ports (needs the sister repo's release binary; tests skip when it is missing).
#![allow(dead_code)]

use mix_wing::{ConnectOptions, WingSurface};
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use wing_model::ParamStore;
use wing_proto::server::{
    Hub, MeterBank, MeterStreamer, MeterSubscriptions, NativeServer, ServerConfig, ServerHandle,
    ServerShared, SyntheticMeters,
};

pub fn opts() -> ConnectOptions {
    ConnectOptions {
        discovery_port: None,
        // generous: other test binaries (and agents) load the machine
        timeout: Duration::from_secs(20),
        sync_timeout: Duration::from_secs(60),
        ..ConnectOptions::default()
    }
}

/// In-process console: wing_proto's native server over a `ParamStore`, synthetic meters.
pub struct InProc {
    pub store: Arc<ParamStore>,
    pub addr: SocketAddr,
    _server: ServerHandle,
    _streamer: MeterStreamer,
    _synth: SyntheticMeters,
}

impl InProc {
    pub fn start() -> InProc {
        let mut store = ParamStore::new();
        let hub = Hub::attach(&mut store);
        let store = Arc::new(store);
        let subs = Arc::new(MeterSubscriptions::new());
        let bank = Arc::new(MeterBank::for_schema());
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let streamer =
            MeterStreamer::spawn(subs.clone(), bank.clone(), sock, Duration::from_millis(50))
                .unwrap();
        let synth =
            SyntheticMeters::spawn(bank, Some(store.clone()), Duration::from_millis(50)).unwrap();
        let cfg = ServerConfig {
            bind: "127.0.0.1:0".parse().unwrap(),
            idle_timeout: None,
            ..ServerConfig::default()
        };
        let server = NativeServer::bind(
            cfg,
            ServerShared {
                store: store.clone(),
                hub,
                meters: Some(subs),
            },
            Arc::new(|_| {}),
        )
        .unwrap()
        .spawn()
        .unwrap();
        let addr = server.local_addr();
        InProc {
            store,
            addr,
            _server: server,
            _streamer: streamer,
            _synth: synth,
        }
    }

    pub fn connect(&self) -> WingSurface {
        WingSurface::connect(self.addr, &opts()).expect("connect in-process console")
    }

    /// Change a value as the console itself would (origin != any client).
    pub fn console_set(&self, path: &str, v: wing_model::Value) {
        let id = self.store.lookup(path).unwrap();
        self.store.set_from(id, v, 0x7777).unwrap();
    }

    pub fn console_set_enum(&self, path: &str, tok: &str) {
        let id = self.store.lookup(path).unwrap();
        self.store.set_enum_name(id, tok, 0x7777).unwrap();
    }
}

// ---- headless wing-sim daemon -----------------------------------------------------------------------------------

/// A headless wing-sim on private ports with its own (bogus) shm name and temp state dir (mix-testsim). Never
/// touches the user's own daemon on 2222/2223.
pub struct Daemon {
    inner: mix_testsim::SimDaemon,
    pub addr: SocketAddr,
    pub discovery: SocketAddr,
}

impl Daemon {
    /// `None` (test should skip) when the binary is missing or does not start (retries on port clashes).
    pub fn start() -> Option<Daemon> {
        Self::start_with(&["--idle-timeout", "0"])
    }

    /// Like [`Daemon::start`] with extra daemon arguments.
    pub fn start_with(extra: &[&str]) -> Option<Daemon> {
        let inner = mix_testsim::SimDaemon::start_with(extra)?;
        Some(Daemon {
            addr: inner.addr(),
            discovery: inner.discovery(),
            inner,
        })
    }

    /// Kill the daemon process (drops every connection).
    pub fn kill(&mut self) {
        self.inner.kill();
    }

    /// Kill and start a fresh daemon on the same ports (factory-default console).
    pub fn restart(&mut self) -> bool {
        self.inner.restart()
    }

    pub fn connect(&self) -> WingSurface {
        let _ = &self.inner;
        // a loaded machine can drop a connection while the daemon is still settling: retry
        let mut last = None;
        for _ in 0..5 {
            match WingSurface::connect(self.addr, &opts()) {
                Ok(s) => return s,
                Err(e) => last = Some(e),
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        panic!("connect wing-sim: {:?}", last.unwrap());
    }
}

/// Start a daemon or print why the test is skipped.
pub fn daemon_or_skip(test: &str) -> Option<Daemon> {
    let d = Daemon::start();
    if d.is_none() {
        eprintln!(
            "{test}: SKIPPED (wing-sim binary not found or did not start; build it with \
             `cargo build --release -p wing-sim` in ../wing-simulator or set WING_SIM_BIN)"
        );
    }
    d
}

// ---- shared scenarios ------------------------------------------------------------------------------------------

use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::model::Role;
use mix_surface::remit::{
    detect_topology, effective_roles, evaluate, Policy, RemitContext, RemitSession,
    ScoringDetector, WriteRequest,
};
use mix_surface::Surface;
use std::collections::BTreeMap;

fn w(s: &WingSurface, p: &str, v: Value) {
    s.write(&ParamId::new(p), v)
        .unwrap_or_else(|e| panic!("write {p}: {e}"));
}

/// Build a small show through `driver` (another client): bus 5 "IEM 1" fed pre-fader by ch 1..3, patched to local
/// out 1/2 and not on the main; bus 1 "DRUMS" fed post-fader and assigned to main 1.
pub fn build_show(driver: &WingSurface) {
    w(driver, "bus.5/meta/name", Value::Text("IEM 1".into()));
    w(driver, "bus.1/meta/name", Value::Text("DRUMS".into()));
    for ch in 1..=3 {
        let c = format!("input.{ch}");
        w(
            driver,
            &format!("{c}/meta/name"),
            Value::Text(format!("CH{ch}")),
        );
        w(driver, &format!("{c}/fader"), Value::Float(0.0));
        w(driver, &format!("{c}/send/main.1/on"), Value::Bool(true));
        w(driver, &format!("{c}/send/bus.5/on"), Value::Bool(true));
        w(
            driver,
            &format!("{c}/send/bus.5/tap"),
            Value::Enum("pre".into()),
        );
        w(driver, &format!("{c}/send/bus.5/level"), Value::Float(0.0));
        w(driver, &format!("{c}/send/bus.1/on"), Value::Bool(true));
        w(
            driver,
            &format!("{c}/send/bus.1/tap"),
            Value::Enum("post".into()),
        );
        w(driver, &format!("{c}/send/bus.1/level"), Value::Float(0.0));
    }
    w(driver, "bus.5/send/main.1/on", Value::Bool(false));
    w(driver, "bus.1/send/main.1/on", Value::Bool(true));
    w(driver, "output.lcl.1/grp", Value::Enum("BUS".into()));
    w(driver, "output.lcl.1/in", Value::Int(9));
    w(driver, "output.lcl.2/grp", Value::Enum("BUS".into()));
    w(driver, "output.lcl.2/in", Value::Int(10));
}

pub fn policy() -> Policy {
    Policy::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/remit/wing-default.toml"))
        .expect("default remit policy")
}

/// Role detection over the surface's topology and the remit decisions that follow from it.
pub fn check_monitor_classification(s: &WingSurface) {
    let policy = policy();
    let topo = s.topology().unwrap();
    let iem = topo.get(&StripId::new("bus", 5)).unwrap();
    assert_eq!(iem.name, "IEM 1");
    assert!(iem
        .outputs
        .contains(&mix_surface::model::OutputKind::Physical));
    let det = ScoringDetector::new(policy.detection.clone());
    let found = detect_topology(&topo, &policy.detection, &det);
    let role = |id: &str| found[&StripId::parse(id).unwrap()].1.effective;
    assert_eq!(
        role("bus.5"),
        Role::Monitor,
        "{:?}",
        found[&StripId::new("bus", 5)]
    );
    assert!(
        matches!(role("bus.1"), Role::FohGroup | Role::Fx),
        "{:?}",
        found[&StripId::new("bus", 1)]
    );
    assert_eq!(role("input.1"), Role::Foh);
    let roles = effective_roles(&found, &BTreeMap::new());
    let solo = s.solo_state().unwrap();
    let ctx = RemitContext::from_topology(
        &topo,
        &roles,
        &policy,
        &s.info().capabilities,
        solo.is_destructive(),
    );
    let session = RemitSession::default();
    let decide = |p: &str, to: Value| {
        let spec = s.param_spec(&ParamId::new(p)).unwrap();
        let from = s.read(&spec.id).unwrap();
        evaluate(
            &policy,
            &ctx,
            &session,
            &WriteRequest {
                param: spec.id,
                class: spec.class,
                from: Some(from),
                to: Some(to),
            },
        )
    };
    let d = decide("input.1/send/bus.5/level", Value::Float(-3.0));
    assert!(!d.is_allowed(), "send into the IEM must be locked: {d:?}");
    let d = decide("bus.5/fader", Value::Float(-3.0));
    assert!(!d.is_allowed(), "IEM master must be locked: {d:?}");
    let d = decide("input.1/fader", Value::Float(-3.0));
    assert!(d.is_allowed(), "FOH channel fader: {d:?}");
    let d = decide("system/cfg/solo/mode", Value::Enum("SIP".into()));
    assert!(!d.is_allowed());
}

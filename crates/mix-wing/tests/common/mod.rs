//! Test consoles: an in-process `wing_proto` native server (fast, always available) and a headless wing-sim daemon
//! on private ports (needs the sister repo's release binary; tests skip when it is missing).
#![allow(dead_code)]

use mix_wing::{ConnectOptions, WingSurface};
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wing_model::ParamStore;
use wing_proto::server::{
    Hub, MeterBank, MeterStreamer, MeterSubscriptions, NativeServer, ServerConfig, ServerHandle,
    ServerShared, SyntheticMeters,
};

pub fn opts() -> ConnectOptions {
    ConnectOptions {
        discovery_port: None,
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

/// The wing-sim binary: `$WING_SIM_BIN`, else `wing-simulator/target/release/wing-sim` next to an ancestor of this
/// crate (the sister repo checkout).
pub fn wing_sim_bin() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("WING_SIM_BIN") {
        let p = PathBuf::from(p);
        return p.is_file().then_some(p);
    }
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    for dir in here.ancestors() {
        let p = dir.join("wing-simulator/target/release/wing-sim");
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn free_port() -> u16 {
    // a port free for both TCP and UDP
    loop {
        let t = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = t.local_addr().unwrap().port();
        if UdpSocket::bind(("127.0.0.1", port)).is_ok()
            && UdpSocket::bind(("127.0.0.1", port + 1)).is_ok()
        {
            if let Ok(t2) = TcpListener::bind(("127.0.0.1", port + 1)) {
                drop(t2);
                return port;
            }
        }
    }
}

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A headless wing-sim on private ports with its own (bogus) shm name and temp state dir. Never touches the
/// user's own daemon on 2222/2223.
pub struct Daemon {
    child: Child,
    pub addr: SocketAddr,
    pub discovery: SocketAddr,
    state_dir: PathBuf,
}

impl Daemon {
    /// `None` (test should skip) when the binary is missing.
    pub fn start() -> Option<Daemon> {
        let bin = wing_sim_bin()?;
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let state_dir = std::env::temp_dir().join(format!("wingmix-test-{pid}-{n}"));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).unwrap();
        let port = free_port();
        assert_ne!(port, 2222);
        let shm = format!("/wingmix-test-{pid}-{n}");
        let child = Command::new(&bin)
            .args(["--name", "WMTEST", "--port", &port.to_string()])
            .args(["--discovery-port", &port.to_string()])
            .args(["--osc-port", &(port + 1).to_string()])
            .args([
                "--no-hostio",
                "--shm-name",
                &shm,
                "--idle-timeout",
                "0",
                "--no-autosave",
            ])
            .arg("--state-dir")
            .arg(&state_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let mut d = Daemon {
            child,
            addr,
            discovery: addr,
            state_dir,
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
                return Some(d);
            }
            if let Ok(Some(_)) = d.child.try_wait() {
                return None;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = d.child.kill();
        None
    }

    pub fn connect(&self) -> WingSurface {
        WingSurface::connect(self.addr, &opts()).expect("connect wing-sim")
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state_dir);
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

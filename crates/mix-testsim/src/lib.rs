//! Test support: a live, headless `wing-sim` daemon on private ports, optionally fed multitrack audio through a
//! private shared-memory ring clocked by [`wing_shm::sim::DriverSim`] (a stand-in for the HAL driver).
//!
//! Safety rules (the user's own wing-sim owns 2222/2223, `/wingsim.audio.v1` and `~/Library/.../wing-sim`):
//! - ports are chosen in 20000..48000 (even pair `p`, `p+1`), never 2222;
//! - the state dir is a fresh temp dir; `--no-autosave`, `--no-hostio`;
//! - the shm name is always private (`/wmx-<pid>-<n>`), never the default;
//! - `Drop` kills the daemon, stops the driver thread, unlinks the shm and removes the state dir.
//!
//! Tests skip (return `None`) when the wing-sim binary is missing: set `WING_SIM_BIN` or build it with
//! `cargo build --release -p wing-sim` in the sister repo. See `docs/testing.md`.
//!
//! Typical use (dev-dependency):
//! ```ignore
//! let console = SimConsole::start(Feed::synth(10.0), &[1, 2]).expect("wing-sim available");
//! // patch channels to USB inputs (see `console.patch_hint()`), connect mix-wing to console.addr(), read meters,
//! let out = console.captured(1); // USB out lane 1 as f32 mono
//! ```

use mix_sim::synth::{self, SongSpec};
use mix_sim::Multitrack;
use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wing_shm::layout::CHANNELS;
use wing_shm::sim::DriverSim;
use wing_shm::{OpenOptions, Segment};

/// Frames per simulated HAL cycle.
pub const BLOCK: usize = 128;
/// Sample rate of the simulated driver.
pub const SAMPLE_RATE: u32 = 48_000;

/// The wing-sim binary: `$WING_SIM_BIN`, else `wing-simulator/target/release/wing-sim` beside an ancestor of this
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

/// A port pair (p, p+1) free for TCP and UDP, outside the OS ephemeral range, never 2222/2223.
fn free_port(salt: u32) -> u16 {
    let mut x = (std::process::id() ^ salt.wrapping_mul(0x9E37_79B9)) as u64;
    loop {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let port = 20_000 + ((x >> 33) % 14_000) as u16 * 2;
        let ok = |p: u16| {
            TcpListener::bind(("0.0.0.0", p)).is_ok() && UdpSocket::bind(("0.0.0.0", p)).is_ok()
        };
        if ok(port) && ok(port + 1) {
            return port;
        }
    }
}

fn spawn_child(
    bin: &Path,
    port: u16,
    shm: &str,
    extra: &[&str],
    state_dir: &Path,
) -> Option<Child> {
    Command::new(bin)
        .args(["--name", "WMTEST", "--port", &port.to_string()])
        .args(["--discovery-port", &port.to_string()])
        .args(["--osc-port", &(port + 1).to_string()])
        .args(["--no-hostio", "--shm-name", shm, "--no-autosave"])
        .args(extra)
        .arg("--state-dir")
        .arg(state_dir)
        .stdout(Stdio::null())
        .stderr(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(state_dir.join("wing-sim.log"))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        )
        .spawn()
        .ok()
}

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A headless wing-sim on private ports with a private shm name and temp state dir. Without a driver attached to
/// the shm name the daemon free-runs on its own clock (no input audio).
pub struct SimDaemon {
    child: Child,
    bin: PathBuf,
    extra: Vec<String>,
    addr: SocketAddr,
    state_dir: PathBuf,
    shm: String,
}

impl SimDaemon {
    /// `None` (test should skip) when the binary is missing or does not start (retries on port clashes).
    pub fn start() -> Option<SimDaemon> {
        Self::start_with(&["--idle-timeout", "0"])
    }

    /// Like [`SimDaemon::start`] with extra daemon arguments.
    pub fn start_with(extra: &[&str]) -> Option<SimDaemon> {
        let bin = wing_sim_bin()?;
        (0..5).find_map(|_| {
            let shm = next_shm_name();
            Self::start_once(&bin, shm, extra)
        })
    }

    /// Start against an existing shm name (a [`ShmDriver`] already created it).
    fn start_on(shm: &str, extra: &[&str]) -> Option<SimDaemon> {
        let bin = wing_sim_bin()?;
        (0..5).find_map(|_| Self::start_once(&bin, shm.to_string(), extra))
    }

    fn start_once(bin: &Path, shm: String, extra: &[&str]) -> Option<SimDaemon> {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let pid = std::process::id();
        let state_dir = std::env::temp_dir().join(format!("wingmix-test-{pid}-{n}"));
        let _ = std::fs::remove_dir_all(&state_dir);
        std::fs::create_dir_all(&state_dir).ok()?;
        let port = free_port(n);
        assert!(port != 2222 && port != 2223);
        assert_ne!(shm, wing_shm::SHM_NAME, "never the default shm name");
        let child = spawn_child(bin, port, &shm, extra, &state_dir)?;
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let mut d = SimDaemon {
            child,
            bin: bin.to_path_buf(),
            extra: extra.iter().map(|s| s.to_string()).collect(),
            addr,
            state_dir,
            shm,
        };
        d.wait_ready().then_some(d)
    }

    fn wait_ready(&mut self) -> bool {
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            if std::net::TcpStream::connect_timeout(&self.addr, Duration::from_millis(200)).is_ok()
            {
                return true;
            }
            if let Ok(Some(st)) = self.child.try_wait() {
                eprintln!("wing-sim exited ({st}):\n{}", self.log());
                return false;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        eprintln!(
            "wing-sim did not accept connections within 20 s:\n{}",
            self.log()
        );
        false
    }

    /// Kill the daemon process (connections drop; the ports are free again). Idempotent.
    pub fn kill(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    /// Kill (if running) and start a fresh daemon on the *same* ports, shm name and state dir: the console comes
    /// back with factory defaults. `false` when it did not come up.
    pub fn restart(&mut self) -> bool {
        self.kill();
        let extra: Vec<&str> = self.extra.iter().map(String::as_str).collect();
        let port = self.addr.port();
        for _ in 0..20 {
            match spawn_child(&self.bin, port, &self.shm, &extra, &self.state_dir) {
                Some(c) => {
                    self.child = c;
                    return self.wait_ready();
                }
                None => std::thread::sleep(Duration::from_millis(100)),
            }
        }
        false
    }

    /// Native-protocol TCP address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// UDP discovery address (same port as the native one on the private pair; OSC is `port + 1`).
    pub fn discovery(&self) -> SocketAddr {
        self.addr
    }

    pub fn shm_name(&self) -> &str {
        &self.shm
    }

    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The daemon's stderr log.
    pub fn log(&self) -> String {
        std::fs::read_to_string(self.state_dir.join("wing-sim.log")).unwrap_or_default()
    }
}

impl Drop for SimDaemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state_dir);
        let _ = Segment::unlink(&self.shm);
    }
}

fn next_shm_name() -> String {
    // macOS limits POSIX shm names to 31 chars.
    format!(
        "/wmx-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    )
}

/// Audio fed into the daemon's USB inputs (looped).
#[derive(Debug, Clone)]
pub struct Feed {
    /// `lanes[usb-1]` = mono samples at [`SAMPLE_RATE`]; empty lanes are silent.
    pub lanes: Vec<Vec<f32>>,
}

impl Feed {
    /// The synthetic 16-track song from `mix-sim` (USB n = track n; patch ch n <- USB n to hear it).
    pub fn synth(seconds: f64) -> Feed {
        let (mt, _) = synth::generate(&SongSpec {
            seconds,
            ..SongSpec::default()
        });
        Feed::from_multitrack(&mt)
    }

    /// Any multitrack (e.g. a real-multitrack excerpt); each track goes to its `usb` lane.
    pub fn from_multitrack(mt: &Multitrack) -> Feed {
        assert_eq!(mt.sample_rate, SAMPLE_RATE, "feed is 48 kHz only");
        let mut lanes = vec![Vec::new(); CHANNELS];
        for t in &mt.tracks {
            lanes[t.usb as usize - 1] = t.audio.clone();
        }
        Feed { lanes }
    }

    /// A sine of `amp` (linear) at `hz` on USB lane `usb` (1-based), `seconds` long; other lanes silent.
    pub fn sine(usb: usize, hz: f32, amp: f32, seconds: f64) -> Feed {
        let n = (seconds * SAMPLE_RATE as f64) as usize;
        let mut lanes = vec![Vec::new(); CHANNELS];
        lanes[usb - 1] = (0..n)
            .map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / SAMPLE_RATE as f32).sin())
            .collect();
        Feed { lanes }
    }
}

type Capture = Arc<Mutex<Vec<Vec<f32>>>>;

/// The simulated driver: a paced thread doing the HAL cycle (`read_input` = daemon's USB outputs, `write_output`
/// = the feed into the daemon's USB inputs) on a private shm segment.
pub struct ShmDriver {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    capture: Capture,
    lanes: Vec<usize>,
    name: String,
}

impl ShmDriver {
    /// Create the private segment `name` and start clocking it. Capture USB-out lanes `capture` (1-based).
    pub fn start(name: &str, feed: Feed, capture: &[usize]) -> Result<ShmDriver, String> {
        assert_ne!(name, wing_shm::SHM_NAME, "never the default shm name");
        let _ = Segment::unlink(name);
        let seg = Segment::open(
            name,
            &OpenOptions {
                create: true,
                sample_rate: SAMPLE_RATE,
                ..Default::default()
            },
        )
        .map_err(|e| format!("shm {name}: {e:?}"))?;
        let drv = DriverSim::new(seg, SAMPLE_RATE);
        drv.set_running(true);
        let capture_lanes: Vec<usize> = capture.to_vec();
        let cap: Capture = Arc::new(Mutex::new(vec![Vec::new(); capture_lanes.len()]));
        let stop = Arc::new(AtomicBool::new(false));
        let (stop2, cap2, lanes2) = (stop.clone(), cap.clone(), capture_lanes.clone());
        let thread = std::thread::Builder::new()
            .name("mix-testsim-driver".into())
            .spawn(move || run_driver(drv, feed, stop2, cap2, lanes2))
            .map_err(|e| e.to_string())?;
        Ok(ShmDriver {
            stop,
            thread: Some(thread),
            capture: cap,
            lanes: capture_lanes,
            name: name.to_string(),
        })
    }

    /// Everything captured so far on USB out lane `usb` (1-based; must have been requested at start).
    pub fn captured(&self, usb: usize) -> Vec<f32> {
        let i = self
            .lanes
            .iter()
            .position(|&l| l == usb)
            .unwrap_or_else(|| panic!("USB out {usb} was not captured"));
        self.capture.lock().unwrap()[i].clone()
    }
}

impl Drop for ShmDriver {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = Segment::unlink(&self.name);
    }
}

fn run_driver(drv: DriverSim, feed: Feed, stop: Arc<AtomicBool>, cap: Capture, lanes: Vec<usize>) {
    let mut io = vec![0.0f32; BLOCK * CHANNELS];
    let start = Instant::now();
    let mut cycle = 0u64;
    while !stop.load(Ordering::Relaxed) {
        // Cycle n happens at wall time n*BLOCK frames; the driver reads one block behind and writes one ahead,
        // like the HAL plugin (see wing-shm's own tests).
        let due = start + Duration::from_secs_f64(cycle as f64 * BLOCK as f64 / SAMPLE_RATE as f64);
        let now = Instant::now();
        if due > now {
            std::thread::sleep(due - now);
        }
        let t_now = (cycle as i64 + 1) * BLOCK as i64;
        // daemon -> DAW
        drv.read_input(t_now - BLOCK as i64, &mut io, BLOCK, CHANNELS);
        // Before the daemon attaches the driver loops its own output back; that is not console audio.
        if !lanes.is_empty() && drv.daemon_alive() {
            let mut c = cap.lock().unwrap();
            for (k, &l) in lanes.iter().enumerate() {
                c[k].extend((0..BLOCK).map(|f| io[f * CHANNELS + l - 1]));
            }
        }
        // DAW -> daemon
        let to = t_now + BLOCK as i64;
        io.fill(0.0);
        for (l, lane) in feed.lanes.iter().enumerate() {
            if lane.is_empty() {
                continue;
            }
            for f in 0..BLOCK {
                io[f * CHANNELS + l] = lane[(to as usize + f) % lane.len().max(1)].min(1.0);
            }
        }
        drv.write_output(to, &io, BLOCK, CHANNELS);
        cycle += 1;
    }
    drv.set_running(false);
}

/// A daemon clocked by a simulated driver that feeds `feed` and captures USB outs.
pub struct SimConsole {
    // field order = drop order: kill the daemon first, then stop the driver thread and unlink the segment.
    daemon: SimDaemon,
    driver: ShmDriver,
}

impl SimConsole {
    /// `None` (skip) when the wing-sim binary is missing / does not start. The segment is created first so the
    /// daemon attaches to a driver that is already cycling.
    pub fn start(feed: Feed, capture_usb_out: &[usize]) -> Option<SimConsole> {
        Self::start_with(feed, capture_usb_out, &["--idle-timeout", "0"])
    }

    pub fn start_with(feed: Feed, capture_usb_out: &[usize], extra: &[&str]) -> Option<SimConsole> {
        wing_sim_bin()?;
        let name = next_shm_name();
        let driver = ShmDriver::start(&name, feed, capture_usb_out).ok()?;
        let daemon = SimDaemon::start_on(&name, extra)?;
        Some(SimConsole { daemon, driver })
    }

    pub fn addr(&self) -> SocketAddr {
        self.daemon.addr()
    }

    pub fn discovery(&self) -> SocketAddr {
        self.daemon.discovery()
    }

    pub fn daemon(&self) -> &SimDaemon {
        &self.daemon
    }

    pub fn captured(&self, usb: usize) -> Vec<f32> {
        self.driver.captured(usb)
    }

    /// To hear USB input `n` in strip `ch`, write `input.<ch>/source/grp = USB` and `input.<ch>/source/in = n`
    /// through the native protocol (mix-wing `WingSurface::write`).
    pub fn patch_hint() -> &'static str {
        "input.<ch>/source/grp = Enum(\"USB\"); input.<ch>/source/in = Int(<usb>)"
    }
}

/// Peak and RMS (linear) of a capture.
pub fn peak_rms(x: &[f32]) -> (f32, f32) {
    let peak = x.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let rms = (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt();
    (peak, rms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_are_private() {
        for salt in 0..20 {
            let p = free_port(salt);
            assert!((20_000..48_000).contains(&p) && p.is_multiple_of(2));
        }
    }

    #[test]
    fn shm_names_fit_macos_and_are_private() {
        let n = next_shm_name();
        assert!(n.len() <= 31 && n != wing_shm::SHM_NAME);
    }

    #[test]
    fn driver_alone_cycles_and_cleans_up() {
        let name = next_shm_name();
        {
            let d = ShmDriver::start(&name, Feed::sine(1, 440.0, 0.5, 1.0), &[1]).unwrap();
            std::thread::sleep(Duration::from_millis(100));
            // no daemon: the segment exists, capture lane exists (loopback mode returns the fed audio)
            let _ = d.captured(1);
            assert!(Segment::open(&name, &OpenOptions::default()).is_ok());
        }
        assert!(Segment::open(&name, &OpenOptions::default()).is_err());
    }

    /// Needs the sister repo's wing-sim binary (skips otherwise): audio fed on USB 1 comes back on USB out 1
    /// once the strip is patched and routed; here we only check the daemon attaches and the shm is released.
    #[test]
    fn console_starts_and_cleans_up() {
        let Some(c) = SimConsole::start(Feed::sine(1, 1000.0, 0.5, 2.0), &[1]) else {
            eprintln!("SKIPPED: no wing-sim binary");
            return;
        };
        let name = c.daemon().shm_name().to_string();
        let dir = c.daemon().state_dir().to_path_buf();
        assert!(c.addr().port() != 2222 && c.addr().port() != 2223);
        drop(c);
        assert!(!dir.exists());
        assert!(Segment::open(&name, &OpenOptions::default()).is_err());
    }
}

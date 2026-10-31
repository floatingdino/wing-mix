//! Test support: a live, headless `wing-sim` daemon on private ports, optionally fed multitrack audio through a
//! private shared-memory ring clocked by [`wing_shm::sim::DriverSim`] (a stand-in for the HAL driver).
//!
//! Safety rules (the user's own wing-sim owns 2222/2223, `/wingsim.audio.v1` and `~/Library/.../wing-sim`):
//! - ports are chosen in 20000..48000 (even pair `p`, `p+1`), never 2222;
//! - the state dir is a fresh temp dir; `--no-autosave`, `--no-hostio`;
//! - the shm name is always private (`/wmx-<pid>-<n>`), never the default;
//! - the daemon runs under a supervisor shell in its own process group and dies when the test process does (stdin
//!   EOF), `Drop` kills the group, and [`sweep_stale`] reaps leftovers of killed test runs (M071);
//! - `Drop` kills the daemon, stops the driver thread, unlinks the shm and removes the state dir.
//!
//! Tests skip (return `None`) when the wing-sim binary is missing: set `WING_SIM_BIN` or build it with
//! `cargo build --release -p wing-sim` in the sister repo. See `docs/testing.md`.
//!
//! Typical use (dev-dependency):
//! ```text
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
    // mix in the clock: concurrent test binaries (different pids, same salts) must not walk the same sequence
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut x = ((std::process::id() as u64) << 32 | nanos as u64)
        ^ (salt as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
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

/// Supervisor script run as `sh -c SCRIPT sh <wing-sim> <args>`: the daemon runs in the background of a shell that
/// is its own process-group leader, and a watcher exits (killing the daemon) when our stdin hits EOF, i.e. when
/// the test process is gone for any reason (SIGKILL, abort, timeout). wing-sim has no stdin-EOF flag (M071).
#[cfg(unix)]
const SUPERVISOR: &str = r#""$@" </dev/null &
c=$!
{ cat >/dev/null; kill -KILL $c 2>/dev/null; } <&0 &
wait $c
st=$?
exit $st"#;

/// A spawned daemon plus the pipe whose closing (on drop, or when this process dies) makes the supervisor kill it.
struct Spawned {
    child: Child,
    #[allow(dead_code)] // held for its Drop: closing the write end is the "parent died" signal
    stdin: Option<std::process::ChildStdin>,
}

fn spawn_child(
    bin: &Path,
    port: u16,
    shm: &str,
    extra: &[&str],
    state_dir: &Path,
) -> Option<Spawned> {
    #[cfg(unix)]
    let mut cmd = {
        use std::os::unix::process::CommandExt;
        let mut c = Command::new("/bin/sh");
        c.args(["-c", SUPERVISOR, "sh"]).arg(bin);
        c.process_group(0);
        c
    };
    #[cfg(not(unix))]
    let mut cmd = Command::new(bin);
    cmd.args(["--name", "WMTEST", "--port", &port.to_string()])
        .args(["--discovery-port", &port.to_string()])
        .args(["--osc-port", &(port + 1).to_string()])
        .args(["--no-hostio", "--shm-name", shm, "--no-autosave"])
        .args(extra)
        .arg("--state-dir")
        .arg(state_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(state_dir.join("wing-sim.log"))
                .map(Stdio::from)
                .unwrap_or_else(|_| Stdio::null()),
        );
    let mut child = cmd.spawn().ok()?;
    // Recorded for the startup sweep: who to wait for, which group to kill, which shm to unlink.
    let _ = std::fs::write(state_dir.join("parent.pid"), std::process::id().to_string());
    let _ = std::fs::write(state_dir.join("group.pid"), child.id().to_string());
    let _ = std::fs::write(state_dir.join("shm.name"), shm);
    let stdin = child.stdin.take();
    Some(Spawned { child, stdin })
}

/// SIGKILL the daemon's whole process group (supervisor shell, watcher, wing-sim) and reap the leader.
fn kill_group(sp: &mut Spawned) {
    #[cfg(unix)]
    // SAFETY: plain kill(2) on the group we created with process_group(0).
    unsafe {
        libc::kill(-(sp.child.id() as i32), libc::SIGKILL);
    }
    let _ = sp.child.kill();
    let _ = sp.child.wait();
    sp.stdin = None;
}

#[cfg(unix)]
fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
    }
}

/// Kill stale `WMTEST` daemons left by test processes that died without running `Drop` (SIGKILL, timeout, abort),
/// unlink their shm and remove their state dirs. Only dirs `wingmix-test-*` in the temp dir whose recorded parent
/// pid is gone are touched, and only a process group whose leader's command line still names `WMTEST` and that
/// state dir (pid-reuse guard). Never the user's own wing-sim. Returns the number of stale dirs cleaned.
/// Runs once per test process from [`SimDaemon::start`]; callable directly.
pub fn sweep_stale() -> usize {
    #[cfg(not(unix))]
    {
        0
    }
    #[cfg(unix)]
    {
        let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) else {
            return 0;
        };
        let mut n = 0;
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with("wingmix-test-") {
                continue;
            }
            let dir = e.path();
            let rd_pid = |f: &str| {
                std::fs::read_to_string(dir.join(f))
                    .ok()
                    .and_then(|t| t.trim().parse::<i32>().ok())
            };
            let parent = match rd_pid("parent.pid") {
                Some(p) => p,
                None => {
                    // not yet written: a daemon is starting right now unless the dir is old
                    let old = e
                        .metadata()
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|d| d > Duration::from_secs(120));
                    if !old {
                        continue;
                    }
                    0
                }
            };
            if parent > 0 && pid_alive(parent) {
                continue;
            }
            if let Some(g) = rd_pid("group.pid").filter(|&g| g > 1) {
                let cmd = Command::new("ps")
                    .args(["-o", "command=", "-p", &g.to_string()])
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_default();
                if cmd.contains("WMTEST") && cmd.contains(name.as_str()) {
                    // SAFETY: kill(2) on a group verified above to be ours.
                    unsafe {
                        libc::kill(-g, libc::SIGKILL);
                    }
                }
            }
            if let Ok(shm) = std::fs::read_to_string(dir.join("shm.name")) {
                let shm = shm.trim();
                if shm.starts_with("/wmx-") && shm != wing_shm::SHM_NAME {
                    let _ = Segment::unlink(shm);
                }
            }
            let _ = std::fs::remove_dir_all(&dir);
            n += 1;
        }
        n
    }
}

static SEQ: AtomicU32 = AtomicU32::new(0);

/// A headless wing-sim on private ports with a private shm name and temp state dir. Without a driver attached to
/// the shm name the daemon free-runs on its own clock (no input audio).
pub struct SimDaemon {
    sp: Spawned,
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
        static SWEEP: std::sync::Once = std::sync::Once::new();
        SWEEP.call_once(|| {
            sweep_stale();
        });
        let sp = spawn_child(bin, port, &shm, extra, &state_dir)?;
        let addr: SocketAddr = ([127, 0, 0, 1], port).into();
        let mut d = SimDaemon {
            sp,
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
                // the port may belong to another test binary's daemon (probe-then-bind race): only ours counts,
                // so make sure our child survived binding it
                std::thread::sleep(Duration::from_millis(150));
                if matches!(self.sp.child.try_wait(), Ok(None)) {
                    return true;
                }
            }
            if let Ok(Some(st)) = self.sp.child.try_wait() {
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
        kill_group(&mut self.sp);
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
                    self.sp = c;
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
        kill_group(&mut self.sp);
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

impl ShmDriver {
    /// Take (drain) everything captured since the last call, one `Vec` per requested lane in request order. For
    /// long runs that stream the console's outputs elsewhere (the M052 end-to-end harness) without the capture
    /// growing; [`ShmDriver::captured`] afterwards only sees what came after.
    pub fn take_captured(&self) -> Vec<Vec<f32>> {
        let mut c = self.capture.lock().unwrap();
        c.iter_mut().map(std::mem::take).collect()
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

    /// See [`ShmDriver::take_captured`].
    pub fn take_captured(&self) -> Vec<Vec<f32>> {
        self.driver.take_captured()
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

    #[cfg(unix)]
    fn group_alive(g: i32) -> bool {
        // SAFETY: signal 0 only probes.
        unsafe { libc::kill(-g, 0) == 0 }
    }

    /// Child half of `daemon_dies_with_killed_parent`: holds a console until killed. Only runs when the parent
    /// test re-executes this binary with `MIX_TESTSIM_HOLD` set.
    #[test]
    #[ignore = "helper: re-executed by daemon_dies_with_killed_parent"]
    fn helper_hold_console() {
        let Ok(out) = std::env::var("MIX_TESTSIM_HOLD") else {
            return;
        };
        let c = SimConsole::start(Feed::sine(1, 440.0, 0.5, 1.0), &[1]).expect("wing-sim");
        let g = std::fs::read_to_string(c.daemon().state_dir().join("group.pid")).unwrap();
        std::fs::write(&out, g).unwrap();
        std::thread::sleep(Duration::from_secs(120));
    }

    /// M071: SIGKILL the test process (no Drop runs) and the daemon must go within 2 s.
    #[cfg(unix)]
    #[test]
    fn daemon_dies_with_killed_parent() {
        if wing_sim_bin().is_none() {
            eprintln!("SKIPPED: no wing-sim binary");
            return;
        }
        let out = std::env::temp_dir().join(format!("wmx-hold-{}", std::process::id()));
        let _ = std::fs::remove_file(&out);
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::helper_hold_console",
                "--ignored",
                "--nocapture",
            ])
            .env("MIX_TESTSIM_HOLD", &out)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let g: i32 = loop {
            if let Some(g) = std::fs::read_to_string(&out)
                .ok()
                .and_then(|t| t.trim().parse().ok())
            {
                break g;
            }
            assert!(Instant::now() < deadline, "helper never started a console");
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(group_alive(g));
        child.kill().unwrap(); // SIGKILL
        let _ = child.wait();
        let t = Instant::now();
        while group_alive(g) && t.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(25));
        }
        let alive = group_alive(g);
        let _ = std::fs::remove_file(&out);
        if alive {
            // SAFETY: cleaning up our own test group.
            unsafe { libc::kill(-g, libc::SIGKILL) };
        }
        assert!(!alive, "daemon outlived its killed parent by > 2 s");
    }

    /// M071: the startup sweep reaps a stale daemon group whose recorded parent is dead, unlinks its shm, and
    /// leaves a live parent's dir alone.
    #[cfg(unix)]
    #[test]
    fn sweep_reaps_stale_and_spares_live() {
        let tmp = std::env::temp_dir();
        let id = std::process::id();
        let dead_parent = {
            let mut c = Command::new("true").spawn().unwrap();
            let p = c.id();
            c.wait().unwrap();
            p
        };
        let mk = |tag: &str, parent: u32| {
            let dir = tmp.join(format!("wingmix-test-sweep{id}-{tag}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // stand-in daemon: own group, command line names WMTEST and the state dir
            use std::os::unix::process::CommandExt;
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 300; true", "sh", "WMTEST"]).arg(&dir);
            c.process_group(0)
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let child = c.spawn().unwrap();
            let shm = format!("/wmx-sw{}-{tag}", id % 100000);
            let seg = Segment::open(
                &shm,
                &OpenOptions {
                    create: true,
                    sample_rate: SAMPLE_RATE,
                    ..Default::default()
                },
            )
            .unwrap();
            std::mem::forget(seg);
            std::fs::write(dir.join("parent.pid"), parent.to_string()).unwrap();
            std::fs::write(dir.join("group.pid"), child.id().to_string()).unwrap();
            std::fs::write(dir.join("shm.name"), &shm).unwrap();
            (dir, child, shm)
        };
        let (stale_dir, mut stale, stale_shm) = mk("stale", dead_parent);
        let (live_dir, mut live, live_shm) = mk("live", id);
        let stale_pid = stale.id() as i32;
        assert!(group_alive(stale_pid));
        sweep_stale(); // (the start-up sweep of a concurrent test may have beaten us to it: assert effects only)
                       // The leader dying of SIGKILL proves the sweep killed its group. (Not `group_alive`: on Linux CI the
                       // killed `sleep` child is reparented to a pid 1 that may not reap, and zombies still answer kill(-g, 0).)
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(stale.wait().unwrap().signal(), Some(libc::SIGKILL));
        assert!(!stale_dir.exists());
        assert!(Segment::open(&stale_shm, &OpenOptions::default()).is_err());
        assert!(live_dir.exists() && matches!(live.try_wait(), Ok(None)));
        assert!(Segment::open(&live_shm, &OpenOptions::default()).is_ok());
        let _ = live.kill();
        let _ = live.wait();
        let _ = Segment::unlink(&live_shm);
        let _ = std::fs::remove_dir_all(&live_dir);
    }
}

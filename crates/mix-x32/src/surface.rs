//! [`X32Surface`]: the [`Surface`] implementation over X32 OSC.
//!
//! Design: one UDP socket "connected" to the console. A background thread receives every datagram, updates the
//! mirror (address -> last OSC argument), emits [`SurfaceEvent`]s for values that *changed* (so own-write echoes,
//! which carry the already-mirrored value, produce no event), and renews `/xremote` before the console's ~10 s
//! subscription timeout. Reads come from the mirror (queried on a miss); writes validate, quantise, send and
//! update the mirror immediately.

use crate::map::{main_assign_addr, send_tap_addr, StripDef, Table, N_BUSES};
use crate::osc::{decode, encode, Arg, Message};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::model::{Block, ParamSpec, SendInfo, SendTap, Strip, StripKind, Topology};
use mix_surface::surface::{
    Capabilities, Origin, SoloState, SoloTarget, Surface, SurfaceError, SurfaceEvent, SurfaceInfo,
    SurfaceResult,
};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Connection settings.
#[derive(Debug, Clone)]
pub struct ConnectOptions {
    /// How long to wait for `/info` and for the initial sync / a missed read.
    pub timeout: Duration,
    /// `/xremote` renewal period (the console drops subscriptions after ~10 s).
    pub xremote_interval: Duration,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        ConnectOptions {
            timeout: Duration::from_secs(3),
            xremote_interval: Duration::from_secs(8),
        }
    }
}

#[derive(Default, Clone)]
struct ConsoleInfo {
    name: String,
    model: String,
    firmware: String,
}

struct Inner {
    sock: UdpSocket,
    table: Table,
    mirror: Mutex<HashMap<String, Arg>>,
    cv: Condvar,
    subs: Mutex<Vec<Sender<SurfaceEvent>>>,
    info: Mutex<Option<ConsoleInfo>>,
    stop: AtomicBool,
    timeout: Duration,
}

impl Inner {
    fn send(&self, m: &Message) -> SurfaceResult<()> {
        self.sock
            .send(&encode(m))
            .map(|_| ())
            .map_err(|e| SurfaceError::Io(e.to_string()))
    }

    fn emit(&self, ev: SurfaceEvent) {
        self.subs
            .lock()
            .unwrap()
            .retain(|tx| tx.send(ev.clone()).is_ok());
    }

    fn handle(&self, m: Message) {
        if m.addr == "/info" {
            let s = |i: usize| match m.args.get(i) {
                Some(Arg::Str(s)) => s.clone(),
                _ => String::new(),
            };
            *self.info.lock().unwrap() = Some(ConsoleInfo {
                name: s(1),
                model: s(2),
                firmware: s(3),
            });
            self.cv.notify_all();
            return;
        }
        let Some(arg) = m.args.first().cloned() else {
            return;
        };
        let old = self
            .mirror
            .lock()
            .unwrap()
            .insert(m.addr.clone(), arg.clone());
        self.cv.notify_all();
        // First sight of a value is initial sync, not a change.
        let Some(old) = old else { return };
        if old == arg {
            return;
        }
        if let Some(id) = self.table.by_addr.get(&m.addr) {
            let entry = &self.table.by_param[id];
            if let Some(value) = entry.codec.decode(&arg) {
                self.emit(SurfaceEvent::ParamChanged {
                    param: id.clone(),
                    value,
                    origin: Origin::External,
                });
            }
            if id.is_send() {
                self.emit(SurfaceEvent::TopologyChanged);
            }
        } else if self.table.topology_addrs.contains(&m.addr) {
            self.emit(SurfaceEvent::TopologyChanged);
        }
    }

    /// Wait until `pred(mirror)` holds or the deadline passes.
    fn wait_mirror(&self, deadline: Instant, pred: impl Fn(&HashMap<String, Arg>) -> bool) -> bool {
        let mut g = self.mirror.lock().unwrap();
        loop {
            if pred(&g) {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            g = self.cv.wait_timeout(g, left).unwrap().0;
        }
    }

    fn mirrored(&self, addr: &str) -> Option<Arg> {
        self.mirror.lock().unwrap().get(addr).cloned()
    }

    /// Mirror value, querying the console on a miss.
    fn fetch(&self, addr: &str) -> SurfaceResult<Arg> {
        if let Some(a) = self.mirrored(addr) {
            return Ok(a);
        }
        self.send(&Message::query(addr))?;
        let deadline = Instant::now() + self.timeout;
        self.wait_mirror(deadline, |m| m.contains_key(addr));
        self.mirrored(addr)
            .ok_or_else(|| SurfaceError::Io(format!("no reply from console for {addr}")))
    }
}

/// Behringer X32/M32 over OSC.
pub struct X32Surface {
    inner: Arc<Inner>,
    thread: Option<JoinHandle<()>>,
}

impl X32Surface {
    /// Connect, subscribe (`/xremote`) and prime the mirror. Fails if the console does not answer `/info`.
    pub fn connect(addr: SocketAddr, opts: &ConnectOptions) -> SurfaceResult<X32Surface> {
        let io = |e: std::io::Error| SurfaceError::Io(e.to_string());
        let sock = UdpSocket::bind(if addr.is_ipv4() {
            "0.0.0.0:0"
        } else {
            "[::]:0"
        })
        .map_err(io)?;
        sock.connect(addr).map_err(io)?;
        sock.set_read_timeout(Some(Duration::from_millis(50)))
            .map_err(io)?;
        let inner = Arc::new(Inner {
            sock,
            table: Table::build(),
            mirror: Mutex::new(HashMap::new()),
            cv: Condvar::new(),
            subs: Mutex::new(Vec::new()),
            info: Mutex::new(None),
            stop: AtomicBool::new(false),
            timeout: opts.timeout,
        });
        let t_inner = inner.clone();
        let interval = opts.xremote_interval;
        let thread = std::thread::Builder::new()
            .name("x32-osc".into())
            .spawn(move || run(t_inner, interval))
            .map_err(io)?;
        let s = X32Surface {
            inner,
            thread: Some(thread),
        };
        s.handshake(opts)?;
        Ok(s)
    }

    fn handshake(&self, opts: &ConnectOptions) -> SurfaceResult<()> {
        let inn = &self.inner;
        // `/info` doubles as the liveness probe; retry a few times (UDP).
        let deadline = Instant::now() + opts.timeout;
        let mut got = false;
        while !got && Instant::now() < deadline {
            inn.send(&Message::query("/info"))?;
            let end = (Instant::now() + Duration::from_millis(250)).min(deadline);
            let mut g = inn.mirror.lock().unwrap();
            while inn.info.lock().unwrap().is_none() {
                let left = end.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break;
                }
                g = inn.cv.wait_timeout(g, left).unwrap().0;
            }
            got = inn.info.lock().unwrap().is_some();
        }
        if !got {
            return Err(SurfaceError::Disconnected);
        }
        inn.send(&Message::query("/xremote"))?;
        self.sync()
    }

    /// Query every mapped address (paced, with retries for dropped datagrams) until the mirror is complete.
    fn sync(&self) -> SurfaceResult<()> {
        let inn = &self.inner;
        let mut required: Vec<String> = inn.table.by_addr.keys().cloned().collect();
        for d in &inn.table.strips {
            required.extend(d.name_addr.iter().cloned());
        }
        let mut optional: Vec<String> = Vec::new();
        for d in inn.table.strips.iter().filter(|d| d.is_input) {
            optional.extend((1..=N_BUSES).map(|m| send_tap_addr(d, m)));
        }
        required.sort();
        for round in 0..4 {
            let missing: Vec<&String> = {
                let m = inn.mirror.lock().unwrap();
                required
                    .iter()
                    .chain(if round == 0 {
                        optional.iter()
                    } else {
                        [].iter()
                    })
                    .filter(|a| !m.contains_key(*a))
                    .collect()
            };
            if missing.is_empty() {
                break;
            }
            for (i, a) in missing.iter().enumerate() {
                inn.send(&Message::query(a.as_str()))?;
                if i % 32 == 31 {
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            let deadline = Instant::now() + inn.timeout / 4;
            inn.wait_mirror(deadline, |m| required.iter().all(|a| m.contains_key(a)));
        }
        let m = inn.mirror.lock().unwrap();
        match required.iter().find(|a| !m.contains_key(*a)) {
            Some(a) => Err(SurfaceError::Io(format!(
                "sync incomplete, no value for {a}"
            ))),
            None => Ok(()),
        }
    }

    /// Native address and OSC argument a generic write would send (no validation): for harnesses that play the
    /// console side of a conformance run.
    pub fn native_of(&self, param: &ParamId, value: &Value) -> Option<(String, Arg)> {
        let e = self.inner.table.by_param.get(param)?;
        Some((e.addr.clone(), e.codec.encode(value)?))
    }

    fn name_of(&self, d: &StripDef) -> String {
        match d.name_addr.as_ref().and_then(|a| self.inner.mirrored(a)) {
            Some(Arg::Str(s)) if !s.trim().is_empty() => s.trim().to_string(),
            _ => d.default_name.clone(),
        }
    }

    fn send_info(
        &self,
        d: &StripDef,
        dest: StripId,
        on_addr: &str,
        level: Option<f64>,
        tap: SendTap,
    ) -> Option<SendInfo> {
        let on = matches!(self.inner.mirrored(on_addr)?, Arg::Int(i) if i != 0);
        let tap_after = match tap {
            SendTap::Pre | SendTap::Unknown => Block::Eq,
            _ if d.chain.contains(&Block::Pan) && dest.kind_name() == "main" => Block::Pan,
            _ => Block::Fader,
        };
        Some(SendInfo {
            dest,
            on,
            level_db: level.unwrap_or(0.0),
            tap,
            tap_after,
        })
    }
}

impl Drop for X32Surface {
    fn drop(&mut self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run(inner: Arc<Inner>, xremote_interval: Duration) {
    let mut buf = [0u8; 2048];
    let mut last_renew = Instant::now();
    while !inner.stop.load(Ordering::Relaxed) {
        if last_renew.elapsed() >= xremote_interval {
            let _ = inner.send(&Message::query("/xremote"));
            last_renew = Instant::now();
        }
        match inner.sock.recv(&mut buf) {
            Ok(n) => {
                if let Some(m) = decode(&buf[..n]) {
                    inner.handle(m);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            // e.g. ECONNREFUSED from an ICMP port-unreachable: keep trying, the console may come back
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

impl Surface for X32Surface {
    fn info(&self) -> SurfaceInfo {
        let i = self.inner.info.lock().unwrap().clone().unwrap_or_default();
        SurfaceInfo {
            surface: "x32".into(),
            name: i.name,
            model: i.model,
            firmware: i.firmware,
            capabilities: Capabilities::default(),
        }
    }

    fn topology(&self) -> SurfaceResult<Topology> {
        let inn = &self.inner;
        let mut strips = Vec::new();
        for d in &inn.table.strips {
            let mut st = Strip::new(d.id.clone(), d.kind, self.name_of(d));
            st.chain = d.chain.clone();
            // X32 has no strip tags; `#WORD` tokens in the 12-char name act as tags (`#AI`, `#NOAI`).
            st.tags = st
                .name
                .split_whitespace()
                .filter_map(|w| w.strip_prefix('#'))
                .map(str::to_string)
                .collect();
            if d.is_input {
                for m in 1..=N_BUSES {
                    let tap = match inn.mirrored(&send_tap_addr(d, m)) {
                        Some(Arg::Int(4)) => SendTap::Post,
                        Some(Arg::Int(5)) => SendTap::Group,
                        Some(Arg::Int(3)) => SendTap::Pre,
                        _ => SendTap::Unknown,
                    };
                    let level = inn
                        .table
                        .by_param
                        .get(&ParamId::new(format!("{}/send/bus.{m}/level", d.id)))
                        .and_then(|e| e.codec.decode(&inn.mirrored(&e.addr)?))
                        .and_then(|v| v.as_f64());
                    st.sends.extend(self.send_info(
                        d,
                        StripId::new("bus", m),
                        &format!("{}/mix/{m:02}/on", d.base),
                        level,
                        tap,
                    ));
                }
            }
            if d.kind != StripKind::Main {
                for m in 1..=2 {
                    st.sends.extend(self.send_info(
                        d,
                        StripId::new("main", m),
                        &main_assign_addr(d, m),
                        None,
                        SendTap::Post,
                    ));
                }
            }
            strips.push(st);
        }
        Ok(Topology::new(strips))
    }

    fn params(&self, strip: &StripId) -> SurfaceResult<Vec<ParamSpec>> {
        let t = &self.inner.table;
        let ids = t
            .strip_params
            .get(strip)
            .ok_or_else(|| SurfaceError::UnknownStrip(strip.clone()))?;
        Ok(ids.iter().map(|i| t.by_param[i].spec.clone()).collect())
    }

    fn param_spec(&self, param: &ParamId) -> SurfaceResult<ParamSpec> {
        self.inner
            .table
            .by_param
            .get(param)
            .map(|e| e.spec.clone())
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))
    }

    fn read(&self, param: &ParamId) -> SurfaceResult<Value> {
        let e = self
            .inner
            .table
            .by_param
            .get(param)
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))?;
        let arg = self.inner.fetch(&e.addr)?;
        e.codec
            .decode(&arg)
            .ok_or_else(|| SurfaceError::Io(format!("unexpected reply {arg:?} for {}", e.addr)))
    }

    fn write(&self, param: &ParamId, value: Value) -> SurfaceResult<Value> {
        let e = self
            .inner
            .table
            .by_param
            .get(param)
            .ok_or_else(|| SurfaceError::UnknownParam(param.clone()))?;
        if e.spec.read_only {
            return Err(SurfaceError::ReadOnly(param.clone()));
        }
        let v = e.spec.coerce(&value)?;
        let arg = e
            .codec
            .encode(&v)
            .ok_or_else(|| SurfaceError::TypeMismatch {
                param: param.clone(),
                got: v.type_name(),
            })?;
        let stored = e.codec.decode(&arg).expect("encode output decodes");
        self.inner
            .send(&Message::new(e.addr.clone(), vec![arg.clone()]))?;
        self.inner
            .mirror
            .lock()
            .unwrap()
            .insert(e.addr.clone(), arg);
        Ok(stored)
    }

    fn subscribe(&self) -> Receiver<SurfaceEvent> {
        let (tx, rx) = channel();
        self.inner.subs.lock().unwrap().push(tx);
        rx
    }

    /// No solo through this adapter (`capabilities.solo = false`); consumers consult the capability first.
    fn solo_state(&self) -> SurfaceResult<SoloState> {
        Err(SurfaceError::Unsupported("solo"))
    }

    fn set_solo(&self, _target: &SoloTarget, _on: bool) -> SurfaceResult<()> {
        Err(SurfaceError::Unsupported("solo"))
    }

    fn clear_solo(&self) -> SurfaceResult<()> {
        Err(SurfaceError::Unsupported("solo"))
    }
}

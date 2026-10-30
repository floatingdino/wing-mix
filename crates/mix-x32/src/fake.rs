//! An in-process fake X32: a UDP OSC server answering the addresses this adapter uses.
//!
//! It is written from the X32 OSC documentation, independently of the adapter's address table: default values come
//! from address *patterns* (`/ch/NN/...`, `/bus/NN/...`, `/main/st|m/...`). It models the console behaviours the
//! adapter depends on: `/info`, `/xremote` subscriptions that expire (renewal needed), queries, sets, and pushing
//! other clients' changes to subscribers. Unknown addresses are ignored, like the real console.

use crate::osc::{decode, encode, Arg, Message};
use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Behaviour switches for tests.
#[derive(Debug, Clone)]
pub struct FakeOptions {
    /// Subscription lifetime after the last `/xremote` (real console: 10 s).
    pub xremote_ttl: Duration,
    /// Also send a client's own sets back to it (to prove the adapter tolerates echoes).
    pub echo_to_sender: bool,
    /// Silently drop this many query datagrams at the start (UDP loss; exercises sync retries).
    pub drop_first_queries: usize,
}

impl Default for FakeOptions {
    fn default() -> Self {
        FakeOptions {
            xremote_ttl: Duration::from_secs(10),
            echo_to_sender: false,
            drop_first_queries: 0,
        }
    }
}

struct State {
    values: HashMap<String, Arg>,
    clients: HashMap<SocketAddr, Instant>,
}

/// The fake console. Dropping it stops the server thread.
pub struct FakeX32 {
    addr: SocketAddr,
    sock: Arc<UdpSocket>,
    state: Arc<Mutex<State>>,
    opts: FakeOptions,
    stop: Arc<AtomicBool>,
    sets: Arc<AtomicUsize>,
    thread: Option<JoinHandle<()>>,
}

fn num(s: &str, max: u32) -> Option<u32> {
    let n: u32 = s.parse().ok()?;
    (s.len() == 2 && (1..=max).contains(&n)).then_some(n)
}

/// Console-side default for an address, `None` if the console has no such node.
pub fn default_value(addr: &str) -> Option<Arg> {
    let seg: Vec<&str> = addr.strip_prefix('/')?.split('/').collect();
    let (kind, rest): (&str, &[&str]) = match seg.as_slice() {
        ["ch", n, rest @ ..] => {
            num(n, 32)?;
            ("ch", rest)
        }
        ["bus", n, rest @ ..] => {
            num(n, 16)?;
            ("bus", rest)
        }
        ["main", "st", rest @ ..] => ("st", rest),
        ["main", "m", rest @ ..] => ("m", rest),
        _ => return None,
    };
    let i = |v| Some(Arg::Int(v));
    let f = |v| Some(Arg::Float(v));
    match rest {
        ["config", "name"] if kind == "ch" || kind == "bus" => {
            let n = seg[1];
            Some(Arg::Str(if kind == "ch" {
                format!("Ch{n}")
            } else {
                String::new()
            }))
        }
        ["mix", "fader"] => f(0.75),
        ["mix", "on"] => i(1),
        ["mix", "pan"] if kind != "m" => f(0.5),
        ["mix", "st"] | ["mix", "mono"] if kind == "ch" || kind == "bus" => {
            i((rest[1] == "st") as i32)
        }
        ["mix", b, leaf] if kind == "ch" => {
            num(b, 16)?;
            match *leaf {
                "on" => i(0),
                "level" => f(0.0),
                "pan" => f(0.5),
                "type" => i(4),
                _ => None,
            }
        }
        ["preamp", leaf] if kind == "ch" => match *leaf {
            "trim" => f(0.5),
            "hpon" => i(0),
            "hpf" => f(0.2),
            _ => None,
        },
        ["eq", "on"] => i(1),
        ["eq", band, leaf] => {
            let b: u32 = band.parse().ok()?;
            if b == 0 || b > if kind == "ch" { 4 } else { 6 } {
                return None;
            }
            match *leaf {
                "type" => i([0, 2, 2, 2, 2, 5][b as usize - 1]),
                "f" => f(0.1 + 0.15 * b as f32),
                "g" => f(0.5),
                "q" => f(0.5),
                _ => None,
            }
        }
        _ => None,
    }
}

impl FakeX32 {
    pub fn start() -> FakeX32 {
        Self::start_with(FakeOptions::default())
    }

    pub fn start_with(opts: FakeOptions) -> FakeX32 {
        let sock = Arc::new(UdpSocket::bind("127.0.0.1:0").unwrap());
        sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let addr = sock.local_addr().unwrap();
        let state = Arc::new(Mutex::new(State {
            values: HashMap::new(),
            clients: HashMap::new(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let sets = Arc::new(AtomicUsize::new(0));
        let thread = {
            let (sock, state, stop, sets, opts) =
                (sock.clone(), state.clone(), stop.clone(), sets.clone(), opts.clone());
            std::thread::spawn(move || serve(sock, state, stop, sets, opts))
        };
        FakeX32 {
            addr,
            sock,
            state,
            opts,
            stop,
            sets,
            thread: Some(thread),
        }
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Current value of a node (default if never set).
    pub fn get(&self, addr: &str) -> Option<Arg> {
        let st = self.state.lock().unwrap();
        st.values.get(addr).cloned().or_else(|| default_value(addr))
    }

    /// Number of client set messages received.
    pub fn client_sets(&self) -> usize {
        self.sets.load(Ordering::Relaxed)
    }

    /// Change a node as the console surface / another client would, and push it to live subscribers.
    /// Returns false if the console has no such node.
    pub fn console_set(&self, addr: &str, v: Arg) -> bool {
        if default_value(addr).is_none() {
            return false;
        }
        let mut st = self.state.lock().unwrap();
        st.values.insert(addr.to_string(), v.clone());
        let now = Instant::now();
        let ttl = self.opts.xremote_ttl;
        let bytes = encode(&Message::new(addr, vec![v]));
        for (c, t) in &st.clients {
            if now.duration_since(*t) < ttl {
                let _ = self.sock.send_to(&bytes, c);
            }
        }
        true
    }

    /// Live (non-expired) `/xremote` subscribers.
    pub fn subscribers(&self) -> usize {
        let st = self.state.lock().unwrap();
        st.clients
            .values()
            .filter(|t| t.elapsed() < self.opts.xremote_ttl)
            .count()
    }
}

impl Drop for FakeX32 {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(
    sock: Arc<UdpSocket>,
    state: Arc<Mutex<State>>,
    stop: Arc<AtomicBool>,
    sets: Arc<AtomicUsize>,
    opts: FakeOptions,
) {
    let mut buf = [0u8; 2048];
    let mut dropped = 0usize;
    while !stop.load(Ordering::Relaxed) {
        let Ok((n, from)) = sock.recv_from(&mut buf) else {
            continue;
        };
        let Some(m) = decode(&buf[..n]) else { continue };
        let reply = |m: Message| {
            let _ = sock.send_to(&encode(&m), from);
        };
        match (m.addr.as_str(), m.args.is_empty()) {
            ("/info", _) => reply(Message::new(
                "/info",
                ["V2.07", "osc-server", "X32", "4.06"]
                    .map(|s| Arg::Str(s.into()))
                    .to_vec(),
            )),
            ("/xremote", _) => {
                state.lock().unwrap().clients.insert(from, Instant::now());
            }
            (addr, true) => {
                if dropped < opts.drop_first_queries {
                    dropped += 1;
                    continue;
                }
                let v = {
                    let st = state.lock().unwrap();
                    st.values.get(addr).cloned().or_else(|| default_value(addr))
                };
                if let Some(v) = v {
                    reply(Message::new(addr, vec![v]));
                }
            }
            (addr, false) => {
                let Some(def) = default_value(addr) else {
                    continue;
                };
                // type must match the node's type
                let arg = m.args[0].clone();
                if std::mem::discriminant(&arg) != std::mem::discriminant(&def) {
                    continue;
                }
                sets.fetch_add(1, Ordering::Relaxed);
                let mut st = state.lock().unwrap();
                st.values.insert(addr.to_string(), arg.clone());
                let bytes = encode(&Message::new(addr, vec![arg]));
                let now = Instant::now();
                for (c, t) in &st.clients {
                    let own = *c == from;
                    if now.duration_since(*t) < opts.xremote_ttl && (!own || opts.echo_to_sender) {
                        let _ = sock.send_to(&bytes, c);
                    }
                }
            }
        }
    }
}

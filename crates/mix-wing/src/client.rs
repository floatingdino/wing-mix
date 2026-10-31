//! Native protocol client (TCP, `wing_proto::nrp` codec) with a local mirror of the console's parameter tree.
//!
//! The mirror is a `wing_model::ParamStore`: the same schema, quantisation and model-switch logic the console uses,
//! so path/hash lookup, active descriptors and value conversions come for free. It is filled by
//! [`NativeClient::sync`] (`DA DC` dump of every active parameter, then explicit gets of the `$` live leaves the dump
//! skips) and kept current by the console's unsolicited change events (`D7 <hash> <value>`; every native client
//! gets them, unlike OSC's single subscriber).
//!
//! Wire facts (docs/research/wing-control.md section 1): get = `D7 h DC` -> `D7 h <value> DE`; set = `D7 h <value>`
//! (not echoed to the setter unless the console adjusted it); events carry no `DE`, so counting `DE`s orders
//! request completion; keepalive `DF D1` within 10 s. Store changes are reported through the mirror's hook as
//! [`ClientMsg::Change`] with origin [`ORIGIN_REMOTE`] (from the console) or [`ORIGIN_LOCAL`] (own writes).

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use wing_model::{Change, NodeId, Origin, ParamStore, Schema};
use wing_proto::nrp::framing::Escaped;
use wing_proto::nrp::meter::{encode_meter_define, MeterSelection};
use wing_proto::nrp::{MeterToken, NrpDecoder, NrpEncoder, Token, CH_CONTROL, CH_METER};
use wing_proto::server::values::{apply_value, with_value_token};

/// Origin of values applied from the console.
pub const ORIGIN_REMOTE: Origin = 1;
/// Origin of own writes applied to the mirror.
pub const ORIGIN_LOCAL: Origin = wing_model::ORIGIN_LOCAL;

/// Keepalive interval (console drops clients after 10 s of silence; libwing uses ~7 s).
pub const KEEPALIVE: Duration = Duration::from_secs(5);

/// Messages from the client's threads.
#[derive(Debug, Clone)]
pub enum ClientMsg {
    /// A mirror value changed (origin [`ORIGIN_REMOTE`] or [`ORIGIN_LOCAL`]).
    Change(Change),
    /// The connection closed.
    Disconnected,
    /// M068: a console value for a node with an own write in flight that is not one of our own in-flight values:
    /// another client (the engineer) set it before the console processed our write, which then overwrote it. The
    /// mirror keeps our value (the console's final state); `raw` is the overwritten value (interpret it with the
    /// node's active descriptor). Reported so the engineer's move is never mistaken for our own echo.
    Overridden(Change),
}

struct Writer {
    stream: TcpStream,
    enc: NrpEncoder,
    buf: Vec<u8>,
    /// `DE`s the console owes us for requests sent so far.
    de_sent: u64,
    last_send: Instant,
}

impl Writer {
    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.stream.write_all(&self.buf)?;
            self.buf.clear();
        }
        self.last_send = Instant::now();
        Ok(())
    }
}

struct Shared {
    alive: AtomicBool,
    stop: AtomicBool,
    /// `DE`s received.
    des: Mutex<u64>,
    des_cv: Condvar,
    /// Held while an own write is applied to the mirror and sent, and while the reader applies console values:
    /// the value `send_value` reads back is the one just written, never a console event that slipped in between.
    apply: Mutex<()>,
    /// Nodes with own writes in flight -> `(DE count at which the console has processed the write, the raw value
    /// we sent)`, oldest first. Console values for such a node that arrive before the last of them were produced
    /// before our last write and would roll the mirror back: they are not applied. If such a value is none of our
    /// in-flight values it is someone else's write that ours overwrote: reported as [`ClientMsg::Overridden`].
    pending: Mutex<HashMap<NodeId, Vec<(u64, Own)>>>,
    /// Nodes with a get in flight -> `DE` count of its answer (a get's answer predates a write sent after it).
    gets: Mutex<HashMap<NodeId, u64>>,
}

/// An own value in flight, as the console will store it.
#[derive(Debug, Clone)]
enum Own {
    Raw(u32),
    /// Float params (raw = the f32's bits): compared with a tiny tolerance (a console that rounds a value we sent
    /// slightly differently than our mirror does must not look like someone else's write).
    Float(f32),
    Text(String),
}

impl Own {
    fn same(&self, o: &Own) -> bool {
        match (self, o) {
            (Own::Raw(a), Own::Raw(b)) => a == b,
            (Own::Float(a), Own::Float(b)) => {
                let (a, b) = (*a as f64, *b as f64);
                (a - b).abs() <= 1e-3 + 1e-4 * a.abs().max(b.abs())
            }
            (Own::Text(a), Own::Text(b)) => a == b,
            _ => false,
        }
    }

    /// Raw encoding (`None` for text).
    fn raw(&self) -> Option<u32> {
        match self {
            Own::Raw(r) => Some(*r),
            Own::Float(f) => Some(f.to_bits()),
            Own::Text(_) => None,
        }
    }
}

fn is_float(d: &wing_model::Desc) -> bool {
    use wing_model::ParamType as T;
    matches!(d.ty, T::LinF | T::LogF | T::Fader)
}

fn own_value(store: &ParamStore, id: NodeId) -> Own {
    if let Some(t) = store.with_str(id, |t| t.to_string()) {
        return Own::Text(t);
    }
    let raw = store.get_raw(id);
    match store.desc(id) {
        Some(d) if is_float(d) => Own::Float(f32::from_bits(raw)),
        _ => Own::Raw(raw),
    }
}

/// The value a console value token stands for, in the node's active encoding (`None`: not decodable).
fn token_value(store: &ParamStore, id: NodeId, t: &Token<'_>) -> Option<Own> {
    use wing_model::{ParamType, Value as W};
    let d = store.desc(id)?;
    let raw = match *t {
        Token::Str(b) => {
            let text = String::from_utf8_lossy(b);
            match d.ty {
                ParamType::Str => return Some(Own::Text(text.into_owned())),
                ParamType::Enum | ParamType::FEnum => d
                    .enum_index(&text)
                    .or_else(|| d.parse_text(&text).ok())?,
                _ => d.parse_text(&text).ok()?,
            }
        }
        _ if d.ty == ParamType::Str => return None,
        Token::Float(v) => d.encode(W::Float(v)).ok()?,
        Token::RawFloat(p) => d.from_norm(p).ok()?,
        _ => d.encode(W::Int(t.as_int()?)).ok()?,
    };
    Some(if is_float(d) {
        Own::Float(f32::from_bits(raw))
    } else {
        Own::Raw(raw)
    })
}

/// What the reader does with a console value for a node.
#[derive(Debug)]
enum Fate {
    /// No own write in flight: apply it. `answer`: it answers a get of ours (else it is a change event: someone
    /// else set the node, even if the value is what the mirror holds).
    Apply { answer: bool },
    /// Produced before our in-flight write, and it is ours (an adjusted echo) or a get's answer: drop it.
    Drop,
    /// Produced before our in-flight write and not ours: someone else's write that ours overwrote.
    Overridden(Own),
}

/// Decide the [`Fate`] of console value `v` for `id`, with `des` answers received so far.
fn fate(
    pending: &mut HashMap<NodeId, Vec<(u64, Own)>>,
    gets: &mut HashMap<NodeId, u64>,
    des: u64,
    id: NodeId,
    v: Option<Own>,
) -> Fate {
    let answer = match gets.get(&id) {
        Some(&t) if des < t => true,
        Some(_) => {
            gets.remove(&id);
            false
        }
        None => false,
    };
    let Some(list) = pending.get_mut(&id) else {
        return Fate::Apply { answer };
    };
    list.retain(|(t, _)| des < *t);
    if list.is_empty() {
        pending.remove(&id);
        return Fate::Apply { answer };
    }
    match v {
        Some(v) if !answer && !list.iter().any(|(_, o)| o.same(&v)) => Fate::Overridden(v),
        _ => Fate::Drop,
    }
}

/// One native-protocol connection plus the mirror store.
pub struct NativeClient {
    store: Arc<ParamStore>,
    writer: Mutex<Writer>,
    shared: Arc<Shared>,
    syncing: Arc<AtomicBool>,
    peer: SocketAddr,
    reader: Mutex<Option<JoinHandle<()>>>,
    /// Sender cloned into each (re)started reader thread.
    tx: Mutex<Sender<ClientMsg>>,
    /// Mirror as it was when the link dropped; kept across failed reconnect attempts (a failed resync may have
    /// half-updated the mirror) until one succeeds and the differences have been announced.
    pre_disconnect: Mutex<Option<Snapshot>>,
}

/// Values of every announced parameter (all but the `$` live leaves, which change constantly; solo leaves excepted).
struct Snapshot(Vec<(NodeId, Slot)>);

#[derive(PartialEq)]
enum Slot {
    Num { raw: u32, desc: usize },
    Text(String),
}

fn announced(id: NodeId) -> bool {
    let s = Schema::get();
    let p = s.path(id);
    let live = p.split('/').skip(1).any(|x| x.starts_with('$'));
    s.is_param(id) && (!live || crate::map::is_solo_leaf(s.name(id)))
}

fn slot(store: &ParamStore, id: NodeId) -> Option<Slot> {
    if let Some(t) = store.with_str(id, |t| t.to_string()) {
        return Some(Slot::Text(t));
    }
    let d = store.desc(id)?;
    Some(Slot::Num {
        raw: store.get_raw(id),
        desc: d as *const _ as usize,
    })
}

impl Snapshot {
    fn take(store: &ParamStore) -> Snapshot {
        Snapshot(
            Schema::get()
                .ids()
                .filter(|&id| announced(id))
                .filter_map(|id| Some((id, slot(store, id)?)))
                .collect(),
        )
    }

    /// Nodes whose value (or active descriptor) differs in `store` now.
    fn diff(&self, store: &ParamStore) -> Vec<NodeId> {
        self.0
            .iter()
            .filter(|(id, old)| slot(store, *id).is_some_and(|now| now != *old))
            .map(|(id, _)| *id)
            .collect()
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn dead() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "WING connection closed")
}

impl NativeClient {
    /// Connect and start the reader/keepalive thread. The mirror holds factory defaults until [`Self::sync`].
    pub fn connect(
        addr: SocketAddr,
        timeout: Duration,
    ) -> io::Result<(Arc<NativeClient>, Receiver<ClientMsg>)> {
        let stream = TcpStream::connect_timeout(&addr, timeout)?;
        stream.set_nodelay(true)?;
        let rd = stream.try_clone()?;
        rd.set_read_timeout(Some(Duration::from_millis(100)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;

        let (tx, rx) = channel::<ClientMsg>();
        let syncing = Arc::new(AtomicBool::new(false));
        let mut store = ParamStore::new();
        {
            let tx = Mutex::new(tx.clone());
            let syncing = syncing.clone();
            store.add_hook(Box::new(move |c: &Change| {
                if !syncing.load(Relaxed) {
                    let _ = lock(&tx).send(ClientMsg::Change(*c));
                }
            }));
        }
        let store = Arc::new(store);
        let shared = Arc::new(Shared {
            alive: AtomicBool::new(true),
            stop: AtomicBool::new(false),
            des: Mutex::new(0),
            des_cv: Condvar::new(),
            apply: Mutex::new(()),
            pending: Mutex::new(Default::default()),
            gets: Mutex::new(Default::default()),
        });
        let mut w = Writer {
            stream,
            enc: NrpEncoder::new(),
            buf: Vec::new(),
            de_sent: 0,
            last_send: Instant::now(),
        };
        w.enc.select(CH_CONTROL, &mut w.buf);
        w.flush()?;
        let client = Arc::new(NativeClient {
            store,
            writer: Mutex::new(w),
            shared,
            syncing,
            peer: addr,
            reader: Mutex::new(None),
            tx: Mutex::new(tx),
            pre_disconnect: Mutex::new(None),
        });
        client.spawn_reader(rd)?;
        Ok((client, rx))
    }

    fn spawn_reader(self: &Arc<Self>, rd: TcpStream) -> io::Result<()> {
        let weak = Arc::downgrade(self);
        let shared = self.shared.clone();
        let store = self.store.clone();
        let tx = lock(&self.tx).clone();
        let syncing = self.syncing.clone();
        let handle = std::thread::Builder::new()
            .name("wing-native-rx".into())
            .spawn(move || reader_loop(rd, store, shared, weak, tx, syncing))?;
        *lock(&self.reader) = Some(handle);
        Ok(())
    }

    /// Re-establish a lost connection in place: a fresh TCP connection, a full resync of the mirror (silent: no
    /// change messages during the resync, the caller announces the reconnect; afterwards one [`ORIGIN_REMOTE`] change
    /// message per value that differs from before the outage is queued) and a restarted reader/keepalive. The mirror, the
    /// message channel and every `Arc<NativeClient>` stay valid. No-op when the connection is alive; fails when the
    /// client was [`closed`](Self::close) or the console is unreachable (call again to retry).
    pub fn reconnect(
        self: &Arc<Self>,
        timeout: Duration,
        sync_timeout: Duration,
    ) -> io::Result<()> {
        if self.is_stopped() {
            return Err(dead());
        }
        if self.is_alive() {
            return Ok(());
        }
        // the previous reader has finished (it is what marked the link dead)
        if let Some(h) = lock(&self.reader).take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
        let stream = TcpStream::connect_timeout(&self.peer, timeout)?;
        stream.set_nodelay(true)?;
        let rd = stream.try_clone()?;
        rd.set_read_timeout(Some(Duration::from_millis(100)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        {
            let mut g = lock(&self.writer);
            let w = &mut *g;
            let _ = w.stream.shutdown(Shutdown::Both);
            w.stream = stream;
            w.enc = NrpEncoder::new();
            w.buf.clear();
            w.de_sent = 0;
            w.enc.select(CH_CONTROL, &mut w.buf);
            w.flush()?;
            *lock(&self.shared.des) = 0;
            lock(&self.shared.pending).clear();
            lock(&self.shared.gets).clear();
            self.shared.alive.store(true, SeqCst);
        }
        self.spawn_reader(rd)?;
        {
            let mut pre = lock(&self.pre_disconnect);
            if pre.is_none() {
                *pre = Some(Snapshot::take(&self.store));
            }
        }
        if let Err(e) = self.sync(sync_timeout) {
            let _ = lock(&self.writer).stream.shutdown(Shutdown::Both);
            return Err(e);
        }
        // announce what changed while we were away as console changes (queued behind whatever the caller emits)
        if let Some(pre) = lock(&self.pre_disconnect).take() {
            let tx = lock(&self.tx);
            for id in pre.diff(&self.store) {
                let _ = tx.send(ClientMsg::Change(Change {
                    id: id.0,
                    desc: 0,
                    raw: self.store.get_raw(id),
                    origin: ORIGIN_REMOTE,
                }));
            }
        }
        Ok(())
    }

    /// Whether [`Self::close`] (or drop) ended this client for good.
    pub fn is_stopped(&self) -> bool {
        self.shared.stop.load(Acquire)
    }

    /// The mirror store (read-only use; writes go through [`Self::set_local`]).
    pub fn store(&self) -> &ParamStore {
        &self.store
    }

    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    pub fn is_alive(&self) -> bool {
        self.shared.alive.load(Acquire)
    }

    fn write_with(&self, f: impl FnOnce(&mut NrpEncoder, &mut Vec<u8>) -> u64) -> io::Result<u64> {
        let mut w = lock(&self.writer);
        self.write_locked(&mut w, f)
    }

    fn write_locked(
        &self,
        w: &mut Writer,
        f: impl FnOnce(&mut NrpEncoder, &mut Vec<u8>) -> u64,
    ) -> io::Result<u64> {
        if !self.is_alive() {
            return Err(dead());
        }
        let n = f(&mut w.enc, &mut w.buf);
        w.de_sent += n;
        let target = w.de_sent;
        w.flush()?;
        Ok(target)
    }

    fn wait_des(&self, target: u64, timeout: Duration) -> io::Result<()> {
        let deadline = Instant::now() + timeout;
        let mut got = lock(&self.shared.des);
        while *got < target {
            if !self.is_alive() {
                return Err(dead());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "WING did not answer in time",
                ));
            }
            got = self
                .shared
                .des_cv
                .wait_timeout(got, left.min(Duration::from_millis(100)))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        Ok(())
    }

    /// Fetch the current values of `ids` from the console into the mirror (pipelined gets) and wait for them.
    pub fn refresh(&self, ids: &[NodeId], timeout: Duration) -> io::Result<()> {
        let s = Schema::get();
        let mut target = 0;
        for chunk in ids.chunks(256) {
            let mut w = lock(&self.writer);
            let base = w.de_sent;
            target = self.write_locked(&mut w, |enc, buf| {
                for &id in chunk {
                    let _ = enc.control(&Token::NavHash(s.hash(id)), buf);
                    let _ = enc.control(&Token::DataRequest, buf);
                }
                chunk.len() as u64
            })?;
            let mut gets = lock(&self.shared.gets);
            for (i, &id) in chunk.iter().enumerate() {
                gets.insert(id, base + i as u64 + 1);
            }
        }
        if ids.is_empty() {
            return Ok(());
        }
        self.wait_des(target, timeout)
    }

    /// Round trip with no data: everything the console sent before answering is in the mirror afterwards.
    pub fn barrier(&self, timeout: Duration) -> io::Result<()> {
        let target = self.write_with(|enc, buf| {
            let _ = enc.control(&Token::NavHash(0), buf);
            let _ = enc.control(&Token::DataRequest, buf);
            1
        })?;
        self.wait_des(target, timeout)
    }

    /// Full synchronisation: `DA DC` dump, then gets of every `$` live leaf below the top level. No change
    /// messages are produced while it runs.
    pub fn sync(&self, timeout: Duration) -> io::Result<()> {
        self.syncing.store(true, SeqCst);
        let r = (|| {
            let target = self.write_with(|enc, buf| {
                let _ = enc.control(&Token::Root, buf);
                let _ = enc.control(&Token::DataRequest, buf);
                1
            })?;
            self.wait_des(target, timeout)?;
            self.refresh(live_leaves(), timeout)
        })();
        self.syncing.store(false, SeqCst);
        r
    }

    /// Guard to hold across "apply an own write to the mirror, then [`Self::send_value`]" so that no console
    /// event is applied to the mirror in between (it would be sent instead of the written value).
    pub fn write_guard(&self) -> MutexGuard<'_, ()> {
        lock(&self.shared.apply)
    }

    /// Send the mirror's current value of `id` to the console (after a local write to the mirror; hold
    /// [`Self::write_guard`] around both). The node counts as pending until the console has answered a barrier
    /// sent right behind the write: console events for it that arrive earlier predate the write and are ignored.
    pub fn send_value(&self, id: NodeId) -> io::Result<()> {
        let store = self.store.clone();
        let target = self.write_with(|enc, buf| {
            let _ = enc.control(&Token::NavHash(Schema::get().hash(id)), buf);
            with_value_token(&store, id, |t| {
                let _ = enc.control(&t, buf);
            });
            let _ = enc.control(&Token::NavHash(0), buf);
            let _ = enc.control(&Token::DataRequest, buf);
            1
        })?;
        lock(&self.shared.pending)
            .entry(id)
            .or_default()
            .push((target, own_value(&self.store, id)));
        Ok(())
    }

    /// [`Self::send_value`] for several nodes in **one** wire write with one barrier behind the last value (M064
    /// slot recall: the console receives the whole batch as one TCP segment run). Hold [`Self::write_guard`]
    /// around the mirror updates and this call.
    pub fn send_values(&self, ids: &[NodeId]) -> io::Result<()> {
        if ids.is_empty() {
            return Ok(());
        }
        let store = self.store.clone();
        let target = self.write_with(|enc, buf| {
            for &id in ids {
                let _ = enc.control(&Token::NavHash(Schema::get().hash(id)), buf);
                with_value_token(&store, id, |t| {
                    let _ = enc.control(&t, buf);
                });
            }
            let _ = enc.control(&Token::NavHash(0), buf);
            let _ = enc.control(&Token::DataRequest, buf);
            1
        })?;
        let mut pending = lock(&self.shared.pending);
        for &id in ids {
            pending
                .entry(id)
                .or_default()
                .push((target, own_value(&self.store, id)));
        }
        Ok(())
    }

    /// Send a raw value token for `id` (used by test drivers).
    pub fn send_token(&self, id: NodeId, t: &Token<'_>) -> io::Result<()> {
        self.write_with(|enc, buf| {
            let _ = enc.control(&Token::NavHash(Schema::get().hash(id)), buf);
            let _ = enc.control(t, buf);
            0
        })
        .map(|_| ())
    }

    /// Define (or redefine) meter report `id` streamed to UDP `port` (channel 3), then reselect channel 1.
    pub fn meter_define(&self, port: u16, id: u32, sels: &[MeterSelection]) -> io::Result<()> {
        self.write_with(|enc, buf| {
            enc.select(CH_METER, buf);
            {
                let mut payload = Vec::new();
                let _ = MeterToken::Port(port).encode(&mut Escaped(&mut payload));
                let _ = encode_meter_define(id, sels, &mut Escaped(&mut payload));
                buf.extend_from_slice(&payload);
            }
            enc.select(CH_CONTROL, buf);
            0
        })
        .map(|_| ())
    }

    /// Renew meter report `id` (`D4 id` on channel 3; the console streams for 5 s after the last renew).
    pub fn meter_renew(&self, id: u32) -> io::Result<()> {
        self.write_with(|enc, buf| {
            enc.select(CH_METER, buf);
            let mut payload = Vec::new();
            let _ = MeterToken::ReportId(id).encode(&mut Escaped(&mut payload));
            buf.extend_from_slice(&payload);
            enc.select(CH_CONTROL, buf);
            0
        })
        .map(|_| ())
    }

    /// Close the connection and join the reader.
    pub fn close(&self) {
        self.shared.stop.store(true, SeqCst);
        let _ = lock(&self.writer).stream.shutdown(Shutdown::Both);
        if let Some(h) = lock(&self.reader).take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }

    fn keepalive(&self) {
        if let Ok(mut w) = self.writer.try_lock() {
            if w.last_send.elapsed() >= KEEPALIVE {
                let w = &mut *w;
                w.enc.select(CH_CONTROL, &mut w.buf);
                let _ = w.flush();
            }
        }
    }
}

impl Drop for NativeClient {
    fn drop(&mut self) {
        self.shared.stop.store(true, SeqCst);
        let _ = lock(&self.writer).stream.shutdown(Shutdown::Both);
    }
}

/// Params with a `$` segment below the top level: the `DA DC` dump skips them.
pub fn live_leaves() -> &'static [NodeId] {
    use std::sync::OnceLock;
    static L: OnceLock<Vec<NodeId>> = OnceLock::new();
    L.get_or_init(|| {
        let s = Schema::get();
        s.ids()
            .filter(|&id| s.is_param(id))
            .filter(|&id| {
                let p = s.path(id);
                p.split('/').skip(1).any(|x| x.starts_with('$'))
            })
            .collect()
    })
}

fn reader_loop(
    mut rd: TcpStream,
    store: Arc<ParamStore>,
    shared: Arc<Shared>,
    client: std::sync::Weak<NativeClient>,
    tx: Sender<ClientMsg>,
    syncing: Arc<AtomicBool>,
) {
    let mut dec = NrpDecoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut cursor: Option<NodeId> = None;
    loop {
        if shared.stop.load(Relaxed) {
            break;
        }
        match rd.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                dec.feed(&buf[..n]);
                while let Some(r) = dec.next_control() {
                    let Ok(t) = r else { continue };
                    match t {
                        Token::NavHash(h) => cursor = store.by_hash(h),
                        Token::End => {
                            *lock(&shared.des) += 1;
                            shared.des_cv.notify_all();
                        }
                        t if t.is_value() => {
                            if let Some(id) = cursor {
                                let _g = lock(&shared.apply);
                                let des = *lock(&shared.des);
                                let f = fate(
                                    &mut lock(&shared.pending),
                                    &mut lock(&shared.gets),
                                    des,
                                    id,
                                    token_value(&store, id, &t),
                                );
                                match f {
                                    Fate::Apply { answer } => {
                                        let changed = apply_value(&store, id, &t, ORIGIN_REMOTE)
                                            .unwrap_or(true);
                                        // M068: a change event whose value the mirror already holds is still
                                        // someone else's write (the console fans out the current value: an
                                        // engineer move our write overwrote before it was sent shows our value)
                                        if !changed && !answer && !syncing.load(Relaxed) {
                                            let _ = tx.send(ClientMsg::Change(Change {
                                                id: id.0,
                                                desc: 0,
                                                raw: store.get_raw(id),
                                                origin: ORIGIN_REMOTE,
                                            }));
                                        }
                                    }
                                    Fate::Drop => {}
                                    Fate::Overridden(v) => {
                                        if let Some(raw) = v.raw() {
                                            let _ = tx.send(ClientMsg::Overridden(Change {
                                                id: id.0,
                                                desc: 0,
                                                raw,
                                                origin: ORIGIN_REMOTE,
                                            }));
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
                while dec.next_meter().is_some() {}
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => break,
        }
        match client.upgrade() {
            Some(c) => c.keepalive(),
            None => break,
        }
    }
    shared.alive.store(false, SeqCst);
    shared.des_cv.notify_all();
    let _ = tx.send(ClientMsg::Disconnected);
}

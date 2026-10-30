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
        });
        client.spawn_reader(rd)?;
        Ok((client, rx))
    }

    fn spawn_reader(self: &Arc<Self>, rd: TcpStream) -> io::Result<()> {
        let weak = Arc::downgrade(self);
        let shared = self.shared.clone();
        let store = self.store.clone();
        let tx = lock(&self.tx).clone();
        let handle = std::thread::Builder::new()
            .name("wing-native-rx".into())
            .spawn(move || reader_loop(rd, store, shared, weak, tx))?;
        *lock(&self.reader) = Some(handle);
        Ok(())
    }

    /// Re-establish a lost connection in place: a fresh TCP connection, a full resync of the mirror (silent: no
    /// change messages, the caller announces the reconnect) and a restarted reader/keepalive. The mirror, the
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
            self.shared.alive.store(true, SeqCst);
        }
        self.spawn_reader(rd)?;
        if let Err(e) = self.sync(sync_timeout) {
            let _ = lock(&self.writer).stream.shutdown(Shutdown::Both);
            return Err(e);
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
        if !self.is_alive() {
            return Err(dead());
        }
        let mut w = lock(&self.writer);
        let w = &mut *w;
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
            target = self.write_with(|enc, buf| {
                for &id in chunk {
                    let _ = enc.control(&Token::NavHash(s.hash(id)), buf);
                    let _ = enc.control(&Token::DataRequest, buf);
                }
                chunk.len() as u64
            })?;
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

    /// Send the mirror's current value of `id` to the console (after a local [`Self::set_local`]).
    pub fn send_value(&self, id: NodeId) -> io::Result<()> {
        let store = self.store.clone();
        self.write_with(|enc, buf| {
            let _ = enc.control(&Token::NavHash(Schema::get().hash(id)), buf);
            with_value_token(&store, id, |t| {
                let _ = enc.control(&t, buf);
            });
            0
        })
        .map(|_| ())
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
                let mut des = 0u64;
                while let Some(r) = dec.next_control() {
                    let Ok(t) = r else { continue };
                    match t {
                        Token::NavHash(h) => cursor = store.by_hash(h),
                        Token::End => des += 1,
                        t if t.is_value() => {
                            if let Some(id) = cursor {
                                let _ = apply_value(&store, id, &t, ORIGIN_REMOTE);
                            }
                        }
                        _ => {}
                    }
                }
                while dec.next_meter().is_some() {}
                if des > 0 {
                    *lock(&shared.des) += des;
                    shared.des_cv.notify_all();
                }
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

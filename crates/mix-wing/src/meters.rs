//! Meters over the native protocol: one report definition on TCP channel 3, data as UDP packets (~20/s), renewed
//! every 3 s (the console streams for 5 s after the last renew; an expired report must be *redefined*, so the
//! engine redefines whenever packets stop). wing-control.md section 3.
//!
//! The report covers every strip (V2 blocks: in/out L/R, gate key/gain/LED, dyn key/gain/state, automix), DCAs
//! (pre/post L/R), the monitor block (solo L/R) and the RTA (120 semitone bands). Gain-reduction words are meter
//! positions under the strip's current gate/dyn model law (`wing_blocks::meter::DynMeterLaw`); they are decoded
//! back to dB of reduction (>= 0) with the model read from the mirror. Levels are dBFS (word / 256), RTA bands dB
//! (word / 128).

use crate::client::NativeClient;
use mix_core::ids::StripId;
use mix_surface::surface::{MeterFrame, StripMeters};
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering::*};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use wing_blocks::meter::DynMeterLaw;
use wing_model::ParamStore;
use wing_proto::nrp::meter::{meter_to_db, rta_to_db, MeterSelection, RTA_BINS};
use wing_proto::nrp::{MeterKind, MeterPacket};

/// Report id this adapter uses ("MX" + 1).
pub const REPORT_ID: u32 = 0x4D58_0001;
/// Renew interval (libwing: 3 s; lifetime 5 s).
pub const RENEW: Duration = Duration::from_secs(3);
/// Redefine when no packet arrived for this long.
pub const STALE: Duration = Duration::from_millis(1500);

/// `(WING domain, generic kind, count, V2 kind, has gate)` of the metered strip kinds.
const STRIPS: &[(&str, &str, u8, MeterKind, bool)] = &[
    ("ch", "input", 40, MeterKind::ChannelV2, true),
    ("aux", "aux", 8, MeterKind::AuxV2, false),
    ("bus", "bus", 16, MeterKind::BusV2, false),
    ("main", "main", 4, MeterKind::MainV2, false),
    ("mtx", "matrix", 8, MeterKind::MatrixV2, false),
];
const DCAS: u8 = 16;
const V2_WORDS: usize = 11;

/// The report definition (order = word order in the packets).
pub fn selections() -> Vec<MeterSelection> {
    let mut v: Vec<MeterSelection> = STRIPS
        .iter()
        .map(|&(_, _, n, kind, _)| MeterSelection {
            kind,
            indices: (0..n).collect(),
        })
        .collect();
    v.push(MeterSelection {
        kind: MeterKind::Dca,
        indices: (0..DCAS).collect(),
    });
    v.push(MeterSelection {
        kind: MeterKind::Monitor,
        indices: Vec::new(),
    });
    v.push(MeterSelection {
        kind: MeterKind::Rta,
        indices: Vec::new(),
    });
    v
}

/// Words per packet of [`selections`].
pub fn total_words() -> usize {
    STRIPS
        .iter()
        .map(|s| s.2 as usize * V2_WORDS)
        .sum::<usize>()
        + DCAS as usize * 4
        + 6
        + RTA_BINS
}

/// RTA band centres: semitones from E0 (20.6 Hz), A4 = 440 Hz at index 53.
pub fn rta_bands_hz() -> Vec<f64> {
    (0..RTA_BINS)
        .map(|i| 440.0 * 2f64.powf((i as f64 - 53.0) / 12.0))
        .collect()
}

/// dB of gain reduction (>= 0) from a gate/dyn gain word under `model`'s meter law; `None` when the law does not
/// encode a reduction (constant needles, dynamic EQs).
pub fn gr_from_word(model: &str, word: i16) -> Option<f32> {
    let pos = word as f32 / 256.0;
    let gr = match DynMeterLaw::for_model(model) {
        DynMeterLaw::Linear {
            full_scale, offset, ..
        } => {
            if pos <= 0.0 {
                0.0
            } else {
                pos * full_scale + offset
            }
        }
        DynMeterLaw::Negative { full_scale } => (-pos).max(0.0) * full_scale,
        DynMeterLaw::Vu => {
            let g = -pos / 0.3;
            if g > 0.0 {
                -20.0 * g.log10()
            } else {
                return None;
            }
        }
        DynMeterLaw::OneKnob => {
            let g = pos * std::f32::consts::LN_10;
            if g > 0.0 {
                -20.0 * g.log10()
            } else {
                return None;
            }
        }
        DynMeterLaw::Const(_) | DynMeterLaw::DynEq | DynMeterLaw::DynEq2 => return None,
    };
    Some(gr.max(0.0))
}

fn token(store: &ParamStore, path: &str) -> String {
    let mut s = String::new();
    if let Some(id) = store.lookup(path) {
        let _ = store.format_token(id, &mut s);
    }
    s
}

/// Decode one packet's words (order of [`selections`]) into a frame.
pub fn decode(words: &[i16], store: &ParamStore, seq: u64) -> Option<MeterFrame> {
    if words.len() < total_words() {
        return None;
    }
    let lvl = |w: i16| meter_to_db(w).max(-128.0);
    let mut strips = Vec::new();
    let mut off = 0;
    for &(dom, kind, n, _, has_gate) in STRIPS {
        for i in 0..n as usize {
            let w = &words[off + i * V2_WORDS..off + (i + 1) * V2_WORDS];
            let p = format!("{dom}/{}", i + 1);
            let dyn_model = token(store, &format!("{p}/dyn/mdl"));
            let (gate_gr_db, gate_open) = if has_gate {
                let gm = token(store, &format!("{p}/gate/mdl"));
                (gr_from_word(&gm, w[5]), Some(w[6] != 0))
            } else {
                (None, None)
            };
            strips.push(StripMeters {
                strip: StripId::new(kind, i as u32 + 1),
                input_db: [lvl(w[0]), lvl(w[1])],
                output_db: [lvl(w[2]), lvl(w[3])],
                pre_fader_db: None,
                gate_gr_db,
                dyn_gr_db: gr_from_word(&dyn_model, w[8]),
                gate_open,
                dyn_active: Some(w[9] != 0),
            });
        }
        off += n as usize * V2_WORDS;
    }
    for i in 0..DCAS as usize {
        let w = &words[off + i * 4..off + i * 4 + 4];
        let mut m = StripMeters::silent(StripId::new("dca", i as u32 + 1));
        m.input_db = [lvl(w[0]), lvl(w[1])];
        m.output_db = [lvl(w[2]), lvl(w[3])];
        strips.push(m);
    }
    off += DCAS as usize * 4;
    let solo_db = Some([lvl(words[off]), lvl(words[off + 1])]);
    off += 6;
    let rta_db = Some(
        words[off..off + RTA_BINS]
            .iter()
            .map(|&w| rta_to_db(w))
            .collect(),
    );
    Some(MeterFrame {
        seq,
        strips,
        solo_db,
        rta_db,
    })
}

struct Sub {
    tx: Sender<MeterFrame>,
    strips: Vec<StripId>,
}

struct State {
    latest: Option<MeterFrame>,
    subs: Vec<Sub>,
}

/// The meter receiver: defines the report, renews it and decodes packets on a background thread.
pub struct MeterEngine {
    state: Mutex<State>,
    cv: Condvar,
    stop: AtomicBool,
    thread: Mutex<Option<JoinHandle<()>>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl MeterEngine {
    /// Bind a UDP socket, define the report and start receiving.
    pub fn start(client: Arc<NativeClient>) -> std::io::Result<Arc<MeterEngine>> {
        let local_ip = match client.peer() {
            SocketAddr::V4(a) if a.ip().is_loopback() => std::net::IpAddr::from([127, 0, 0, 1]),
            _ => std::net::IpAddr::from([0, 0, 0, 0]),
        };
        let sock = UdpSocket::bind((local_ip, 0))?;
        sock.set_read_timeout(Some(Duration::from_millis(100)))?;
        let port = sock.local_addr()?.port();
        let eng = Arc::new(MeterEngine {
            state: Mutex::new(State {
                latest: None,
                subs: Vec::new(),
            }),
            cv: Condvar::new(),
            stop: AtomicBool::new(false),
            thread: Mutex::new(None),
        });
        client.meter_define(port, REPORT_ID, &selections())?;
        let weak = Arc::downgrade(&eng);
        let h = std::thread::Builder::new()
            .name("wing-meters-rx".into())
            .spawn(move || run(sock, port, client, weak))?;
        *lock(&eng.thread) = Some(h);
        Ok(eng)
    }

    /// Latest frame, waiting up to `wait` for the first one.
    pub fn latest(&self, wait: Duration) -> Option<MeterFrame> {
        let deadline = Instant::now() + wait;
        let mut g = lock(&self.state);
        while g.latest.is_none() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            g = self
                .cv
                .wait_timeout(g, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        g.latest.clone()
    }

    /// Stream of frames limited to `strips` (empty = all).
    pub fn subscribe(&self, strips: &[StripId]) -> Receiver<MeterFrame> {
        let (tx, rx) = std::sync::mpsc::channel();
        lock(&self.state).subs.push(Sub {
            tx,
            strips: strips.to_vec(),
        });
        rx
    }

    pub fn stop(&self) {
        self.stop.store(true, SeqCst);
        if let Some(h) = lock(&self.thread).take() {
            if h.thread().id() != std::thread::current().id() {
                let _ = h.join();
            }
        }
    }

    fn publish(&self, f: MeterFrame) {
        let mut g = lock(&self.state);
        g.subs.retain(|s| {
            let frame = if s.strips.is_empty() {
                f.clone()
            } else {
                MeterFrame {
                    strips: f
                        .strips
                        .iter()
                        .filter(|m| s.strips.contains(&m.strip))
                        .cloned()
                        .collect(),
                    ..f.clone()
                }
            };
            s.tx.send(frame).is_ok()
        });
        g.latest = Some(f);
        self.cv.notify_all();
    }
}

fn run(sock: UdpSocket, port: u16, client: Arc<NativeClient>, eng: std::sync::Weak<MeterEngine>) {
    let mut buf = vec![0u8; 8192];
    let mut words = Vec::with_capacity(total_words());
    let mut seq = 0u64;
    let mut last_packet = Instant::now();
    let mut last_define = Instant::now();
    let mut last_renew = Instant::now();
    let mut was_down = false;
    loop {
        let Some(e) = eng.upgrade() else { break };
        if e.stop.load(Relaxed) || client.is_stopped() {
            break;
        }
        drop(e);
        if !client.is_alive() {
            // link down: wait for the reconnect, then redefine the report (packets are stale by then)
            was_down = true;
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        if std::mem::take(&mut was_down) {
            let _ = client.meter_define(port, REPORT_ID, &selections());
            last_define = Instant::now();
            last_renew = last_define;
        }
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            if let Ok(p) = MeterPacket::parse(&buf[..n]) {
                if p.report_id == REPORT_ID {
                    last_packet = Instant::now();
                    words.clear();
                    words.extend(p.values());
                    seq += 1;
                    if let Some(f) = decode(&words, client.store(), seq) {
                        if let Some(e) = eng.upgrade() {
                            e.publish(f);
                        }
                    }
                }
            }
        }
        let now = Instant::now();
        if now.duration_since(last_packet) > STALE && now.duration_since(last_define) > STALE {
            let _ = client.meter_define(port, REPORT_ID, &selections());
            last_define = now;
            last_renew = now;
        } else if now.duration_since(last_renew) >= RENEW {
            let _ = client.meter_renew(REPORT_ID);
            last_renew = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_and_bands() {
        assert_eq!(total_words(), 76 * 11 + 64 + 6 + 120);
        let b = rta_bands_hz();
        assert!((b[0] - 20.6).abs() < 0.05);
        assert!((b[53] - 440.0).abs() < 1e-9);
    }

    #[test]
    fn gr_laws_invert() {
        // GATE: 60 dB full scale
        assert!((gr_from_word("GATE", 128).unwrap() - 30.0).abs() < 1e-4);
        assert!((gr_from_word("COMP", 64).unwrap() - 5.0).abs() < 1e-4);
        assert!((gr_from_word("76LA", -128).unwrap() - 10.0).abs() < 1e-4);
        assert_eq!(gr_from_word("COMP", 0), Some(0.0));
        assert_eq!(gr_from_word("WAVE", 256), None);
        // round trip against the sim's encoder
        for (m, gr) in [
            ("COMP", 6.0f32),
            ("GATE", 24.0),
            ("RIDE", 3.0),
            ("B160", 4.0),
        ] {
            let (w, _) = DynMeterLaw::for_model(m).words(gr, &[], true);
            let back = gr_from_word(m, w).unwrap();
            assert!((back - gr).abs() < 0.3, "{m}: {gr} -> {w} -> {back}");
        }
    }

    #[test]
    fn decode_frame() {
        let st = ParamStore::new();
        let mut w = vec![0i16; total_words()];
        w[0] = -20 * 256; // ch1 in L
        w[3] = -6 * 256; // ch1 out R
        w[5] = 128; // ch1 gate gain (GATE: 30 dB)
        w[6] = 1;
        let mon = 76 * 11 + 64;
        w[mon] = -10 * 256;
        w[mon + 6 + 53] = 128 * -3;
        let f = decode(&w, &st, 7).unwrap();
        let ch1 = &f.strips[0];
        assert_eq!(ch1.strip, StripId::new("input", 1));
        assert_eq!((ch1.input_db[0], ch1.output_db[1]), (-20.0, -6.0));
        assert_eq!(ch1.gate_open, Some(true));
        assert!((ch1.gate_gr_db.unwrap() - 30.0).abs() < 1e-4);
        assert_eq!(f.solo_db.unwrap()[0], -10.0);
        assert_eq!(f.rta_db.as_ref().unwrap()[53], -3.0);
        assert_eq!(f.strips.len(), 76 + 16);
        assert!(decode(&w[..10], &st, 1).is_none());
    }
}

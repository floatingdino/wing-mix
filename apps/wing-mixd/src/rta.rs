//! Console RTA -> [`SafetyMonitor::feed_spectrum`] (M053).
//!
//! With a console that has an RTA (`capabilities.rta`) connected and the feedback guard on, the daemon points the
//! console RTA at the main bus ([`Surface::claim_rta`]; the previous source comes back when the feed stops) and feeds
//! every new meter frame's 120 RTA bands to the safety monitor's feedback detector, labelled `rta:<strip>`. That
//! catches ringing the capture tap cannot see (e.g. a monitor wedge mix with no audio return). The actuator may
//! re-point the RTA for an inspection and restores it to ours afterwards; frames then arrive under the inspected
//! strip's label, so the detectors never mix sources.
//!
//! The claim lives on the feed thread (it borrows the surface), so the thread starts, claims, reports back, and
//! releases when told to stop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use mix_actuator::SafetyMonitor;
use mix_core::ids::StripId;
use mix_surface::surface::RtaSource;
use mix_surface::Surface;
use serde_json::{json, Value};

/// Poll period for new meter frames.
const POLL: Duration = Duration::from_millis(25);

/// What the console RTA listens to while the daemon feeds the safety monitor.
pub fn feed_source() -> RtaSource {
    RtaSource {
        strip: StripId::new("main", 1),
        tap: "post".into(),
    }
}

#[derive(Default)]
struct Stats {
    frames: AtomicU64,
    source: Mutex<String>,
}

/// A running RTA feed; dropping it stops the thread and restores the console's RTA source.
pub struct RtaFeed {
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    stats: Arc<Stats>,
}

impl RtaFeed {
    /// Claim the RTA and start feeding `monitor`. `Ok(None)`: the console has no RTA. Errors: the claim failed.
    pub fn start(
        surface: Arc<dyn Surface>,
        monitor: SafetyMonitor,
    ) -> Result<Option<RtaFeed>, String> {
        let Some(caps) = surface.info().capabilities.rta else {
            return Ok(None);
        };
        let centres = caps.bands_hz;
        let stop = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(Stats::default());
        let (tx, rx) = channel();
        let (st, sa) = (stop.clone(), stats.clone());
        let join = std::thread::Builder::new()
            .name("mixd-rta".into())
            .spawn(move || {
                let claim = match surface.claim_rta(&feed_source()) {
                    Ok(c) => {
                        let _ = tx.send(Ok(()));
                        c
                    }
                    Err(e) => {
                        let _ = tx.send(Err(format!("rta claim: {e}")));
                        return;
                    }
                };
                let mut last_seq = 0;
                while !st.load(Ordering::Relaxed) {
                    if let Ok(f) = surface.meters() {
                        if f.seq != last_seq {
                            last_seq = f.seq;
                            if let Some(db) = &f.rta_db {
                                let src = match surface.rta_source() {
                                    Ok(Some(s)) => format!("rta:{}", s.strip.as_str()),
                                    _ => "rta:off".into(),
                                };
                                monitor.feed_spectrum(&src, db, &centres);
                                sa.frames.fetch_add(1, Ordering::Relaxed);
                                let mut g = sa.source.lock().unwrap_or_else(|e| e.into_inner());
                                if *g != src {
                                    *g = src;
                                }
                            }
                        }
                    }
                    std::thread::sleep(POLL);
                }
                drop(claim); // restores the previous RTA source
            })
            .map_err(|e| e.to_string())?;
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(())) => Ok(Some(RtaFeed {
                stop,
                join: Some(join),
                stats,
            })),
            Ok(Err(e)) => {
                let _ = join.join();
                Err(e)
            }
            Err(_) => {
                stop.store(true, Ordering::Relaxed);
                Err("rta claim timed out".into())
            }
        }
    }

    /// Frames fed so far.
    pub fn frames(&self) -> u64 {
        self.stats.frames.load(Ordering::Relaxed)
    }

    /// `{"frames":n,"source":"rta:main.1"}` for status/snapshot.
    pub fn status(&self) -> Value {
        json!({
            "frames": self.frames(),
            "source": *self.stats.source.lock().unwrap_or_else(|e| e.into_inner()),
        })
    }
}

impl Drop for RtaFeed {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

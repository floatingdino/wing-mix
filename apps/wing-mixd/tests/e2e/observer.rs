//! A second native-protocol client on the console: the WING-Edit / engineer stand-in. It records every parameter
//! change the console announces (with the time it arrived), snapshots the solo state on every solo change, and
//! makes the "engineer" moves (its own writes are remembered so the audit can tell them apart).

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_surface::surface::{Origin, SoloState, SurfaceEvent};
use mix_surface::Surface;
use mix_wing::{ConnectOptions, WingSurface};

#[derive(Debug, Clone)]
pub struct Change {
    pub at: Instant,
    pub param: ParamId,
    pub value: Value,
}

#[derive(Default)]
struct Log {
    changes: Vec<Change>,
    solo: Vec<(Instant, SoloState)>,
    own: BTreeSet<ParamId>,
}

#[derive(Clone)]
pub struct Observer {
    pub s: Arc<WingSurface>,
    log: Arc<Mutex<Log>>,
}

impl Observer {
    pub fn connect(addr: SocketAddr) -> Observer {
        let opts = ConnectOptions {
            discovery_port: Some(addr.port()),
            ..Default::default()
        };
        let s = Arc::new(WingSurface::connect(addr, &opts).expect("observer connect"));
        let rx = s.subscribe();
        let log = Arc::new(Mutex::new(Log::default()));
        let (lg, s2) = (log.clone(), s.clone());
        std::thread::spawn(move || {
            for ev in rx {
                match ev {
                    SurfaceEvent::ParamChanged {
                        param,
                        value,
                        origin: Origin::External,
                    } => lg.lock().unwrap().changes.push(Change {
                        at: Instant::now(),
                        param,
                        value,
                    }),
                    SurfaceEvent::SoloChanged => {
                        if let Ok(st) = s2.solo_state() {
                            lg.lock().unwrap().solo.push((Instant::now(), st));
                        }
                    }
                    _ => {}
                }
            }
        });
        Observer { s, log }
    }

    /// An engineer move (remembered as the harness's own write).
    pub fn engineer_write(&self, param: &str, v: Value) -> Value {
        let p = ParamId::new(param);
        self.log.lock().unwrap().own.insert(p.clone());
        self.s
            .write(&p, v)
            .unwrap_or_else(|e| panic!("{param}: {e}"))
    }

    /// Setup write (soundcheck: patching, names, rough levels); not recorded as an engineer move.
    pub fn setup_write(&self, param: &str, v: Value) {
        self.s
            .write(&ParamId::new(param), v)
            .unwrap_or_else(|e| panic!("setup {param}: {e}"));
    }

    pub fn read(&self, param: &str) -> Value {
        self.s
            .read(&ParamId::new(param))
            .unwrap_or_else(|e| panic!("{param}: {e}"))
    }

    pub fn changes(&self) -> Vec<Change> {
        self.log.lock().unwrap().changes.clone()
    }

    pub fn solo_log(&self) -> Vec<(Instant, SoloState)> {
        self.log.lock().unwrap().solo.clone()
    }

    pub fn own(&self) -> BTreeSet<ParamId> {
        self.log.lock().unwrap().own.clone()
    }

    /// Changes of `param` within (from, to].
    pub fn changes_of(&self, param: &ParamId, from: Instant, to: Instant) -> Vec<Change> {
        self.changes()
            .into_iter()
            .filter(|c| &c.param == param && c.at > from && c.at <= to)
            .collect()
    }

    /// Let notifications in flight arrive.
    pub fn settle(&self) {
        std::thread::sleep(Duration::from_millis(300));
    }
}

//! Console USB outputs -> daemon: the harness's stand-in for the WING's USB audio interface. A headless wing-sim
//! (mix-testsim `SimConsole`, private ports and shm) is clocked by DriverSim, which captures the USB-out lanes; this
//! bridge drains them every few ms and serves them to `wing-mixd` as a raw f32 TCP stream (`select_input
//! {"source":"stream"}`). It can also record a stretch of the main tap for offline scoring.

use std::io::Write;
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mix_testsim::SimConsole;

#[derive(Default)]
struct Rec {
    want: usize,
    left: Vec<f32>,
    right: Vec<f32>,
}

pub struct Bridge {
    pub addr: String,
    stop: Arc<AtomicBool>,
    rec: Arc<Mutex<Rec>>,
    join: Option<JoinHandle<()>>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

impl Bridge {
    /// Serve the console's captured lanes (`channels` of them, as requested at `SimConsole::start`). Lanes 0/1 are
    /// recorded by [`Bridge::record`].
    pub fn start(console: Arc<SimConsole>, channels: usize) -> Bridge {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        l.set_nonblocking(true).unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let stop = Arc::new(AtomicBool::new(false));
        let rec = Arc::new(Mutex::new(Rec::default()));
        let (st, rc) = (stop.clone(), rec.clone());
        let join = std::thread::spawn(move || {
            let mut client = None;
            let mut bytes = Vec::new();
            while !st.load(Ordering::Relaxed) {
                if client.is_none() {
                    if let Ok((c, _)) = l.accept() {
                        c.set_nonblocking(false).ok();
                        c.set_nodelay(true).ok();
                        client = Some(c);
                    }
                }
                let lanes = console.take_captured();
                let n = lanes.iter().map(Vec::len).min().unwrap_or(0);
                if n > 0 {
                    {
                        let mut r = rc.lock().unwrap();
                        if r.left.len() < r.want {
                            let take = n.min(r.want - r.left.len());
                            r.left.extend_from_slice(&lanes[0][..take]);
                            r.right.extend_from_slice(&lanes[1][..take]);
                        }
                    }
                    if let Some(c) = client.as_mut() {
                        bytes.clear();
                        for i in 0..n {
                            for lane in lanes.iter().take(channels) {
                                bytes.extend_from_slice(&lane[i].to_le_bytes());
                            }
                        }
                        if c.write_all(&bytes).is_err() {
                            client = None;
                        }
                    }
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        });
        Bridge {
            addr,
            stop,
            rec,
            join: Some(join),
        }
    }

    /// Record `secs` of lanes 0/1 (the main tap) from now; blocks until done.
    pub fn record(&self, secs: f64, sr: u32) -> (Vec<f32>, Vec<f32>) {
        let want = (secs * sr as f64) as usize;
        {
            let mut r = self.rec.lock().unwrap();
            *r = Rec {
                want,
                left: Vec::with_capacity(want),
                right: Vec::with_capacity(want),
            };
        }
        let t0 = Instant::now();
        loop {
            {
                let r = self.rec.lock().unwrap();
                if r.left.len() >= want {
                    return (r.left.clone(), r.right.clone());
                }
            }
            assert!(
                t0.elapsed().as_secs_f64() < secs * 2.0 + 10.0,
                "bridge recording stalled"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

//! Analysis thread and the controller's meter.
//!
//! The analysis thread drains the capture ring (SPSC; the audio side never waits) into one
//! [`StreamingAnalyser`] per tap, kept in [`Meters`] behind a mutex shared with the engine thread. It feeds the
//! [`SafetyMonitor`] (mains every 100 ms of audio, monitor taps' spectra), refreshes the live main features for
//! snapshots every 0.5 s and advances the audio clock the controller runs on. [`SharedMeter`] is the
//! [`MixMeter`] the controller ticks against: restart/measure lock the meters only for the snapshot.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use mix_actuator::SafetyMonitor;
use mix_analysis::{AnalyserConfig, StreamingAnalyser};
use mix_capture::ring::Consumer;
use mix_core::features::{Features, Window};
use mix_core::ids::StripId;
use mix_engine::control::{Measurement, MixMeter, SoloProbe};
use mix_engine::diagnosis::StripTap;
use mix_knowledge::KnowledgeBase;
use mix_surface::surface::SoloTarget;
use mix_surface::Surface;

use crate::taps::{Tap, TapLayout};

/// Frames popped per analysis step.
const CHUNK: usize = 1024;
/// Safety feed period (s of audio).
const FEED_S: f64 = 0.1;
/// Live snapshot period (s of audio).
const LIVE_S: f64 = 0.5;
/// Solo audio needed before the probe answers (s).
const SOLO_MIN_S: f64 = 0.3;

struct TapAn {
    offsets: Vec<usize>,
    an: StreamingAnalyser,
    scratch: Vec<f32>,
}

impl TapAn {
    fn new(sr: u32, offsets: Vec<usize>) -> Result<Self, String> {
        let an = StreamingAnalyser::new(AnalyserConfig::new(sr, offsets.len()))
            .map_err(|e| e.to_string())?;
        Ok(TapAn {
            scratch: Vec::with_capacity(CHUNK * offsets.len()),
            offsets,
            an,
        })
    }

    fn push(&mut self, frames: &[f32], ch: usize) {
        self.scratch.clear();
        for f in frames.chunks_exact(ch) {
            self.scratch.extend(self.offsets.iter().map(|&o| f[o]));
        }
        self.an.push(&self.scratch);
    }

    fn snap(&mut self, tap: &str, w: Window) -> Features {
        let mut f = self.an.new_features(tap, w);
        self.an.snapshot_into(&mut f);
        f
    }
}

struct SoloAn {
    tap: TapAn,
    active: Vec<SoloTarget>,
    secs: f64,
}

/// Analysers of every tap plus the clocks.
pub struct Meters {
    pub sample_rate: u32,
    ring_channels: usize,
    main: TapAn,
    strips: Vec<(StripId, TapAn)>,
    solo: Option<SoloAn>,
    monitors: Vec<(String, TapAn)>,
    window_frames: u64,
    clock_frames: u64,
    next_feed: u64,
    next_live: u64,
    /// Latest short-term main features (snapshots / GUI).
    pub live: Option<Features>,
    /// Latest momentary main features.
    pub momentary: Option<Features>,
    /// Fed by the analysis thread when set (the session's monitor).
    pub safety: Option<SafetyMonitor>,
    /// Capture ring overruns seen (frames).
    pub dropped: u64,
}

impl Meters {
    pub fn new(layout: &TapLayout, sample_rate: u32) -> Result<Meters, String> {
        let mut main = None;
        let mut strips = Vec::new();
        let mut solo = None;
        let mut monitors = Vec::new();
        for (tap, offs) in &layout.taps {
            let a = TapAn::new(sample_rate, offs.clone())?;
            match tap {
                Tap::Main => main = Some(a),
                Tap::Solo => {
                    solo = Some(SoloAn {
                        tap: a,
                        active: Vec::new(),
                        secs: 0.0,
                    })
                }
                Tap::Strip { strip } => strips.push((strip.clone(), a)),
                Tap::Monitor { name } => monitors.push((name.clone(), a)),
            }
        }
        Ok(Meters {
            sample_rate,
            ring_channels: layout.ring_channels,
            main: main.ok_or("no main tap")?,
            strips,
            solo,
            monitors,
            window_frames: 0,
            clock_frames: 0,
            next_feed: 0,
            next_live: 0,
            live: None,
            momentary: None,
            safety: None,
            dropped: 0,
        })
    }

    /// Audio clock (s of main audio analysed).
    pub fn clock_s(&self) -> f64 {
        self.clock_frames as f64 / self.sample_rate as f64
    }

    pub fn has_solo(&self) -> bool {
        self.solo.is_some()
    }

    pub fn strip_ids(&self) -> Vec<StripId> {
        self.strips.iter().map(|(s, _)| s.clone()).collect()
    }

    fn push(&mut self, frames: &[f32], solo_now: Option<&[SoloTarget]>) {
        let ch = self.ring_channels;
        let n = (frames.len() / ch) as u64;
        self.main.push(frames, ch);
        for (_, a) in &mut self.strips {
            a.push(frames, ch);
        }
        for (_, a) in &mut self.monitors {
            a.push(frames, ch);
        }
        if let (Some(s), Some(now)) = (&mut self.solo, solo_now) {
            if s.active != now {
                s.active = now.to_vec();
                s.tap.an.reset_session();
                s.secs = 0.0;
            }
            if !now.is_empty() {
                s.tap.push(frames, ch);
                s.secs += n as f64 / self.sample_rate as f64;
            }
        }
        self.window_frames += n;
        self.clock_frames += n;
        let sr = self.sample_rate as f64;
        if self.clock_frames >= self.next_feed {
            self.next_feed = self.clock_frames + (FEED_S * sr) as u64;
            let m = self.main.snap("main", Window::Momentary);
            if let Some(mon) = &self.safety {
                mon.feed_mains(&m);
                for (name, a) in &mut self.monitors {
                    let f = a.snap(&format!("monitor:{name}"), Window::Momentary);
                    mon.feed_features(&f);
                }
            }
            self.momentary = Some(m);
        }
        if self.clock_frames >= self.next_live {
            self.next_live = self.clock_frames + (LIVE_S * sr) as u64;
            self.live = Some(self.main.snap("main", Window::ShortTerm));
        }
    }
}

/// Lock helper (a poisoned lock still holds valid analysers).
pub fn lock(m: &Mutex<Meters>) -> MutexGuard<'_, Meters> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The analysis thread.
pub struct Analysis {
    pub meters: Arc<Mutex<Meters>>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Analysis {
    /// Start draining `consumer`. `surface` (when given) is asked for the solo state so the solo analyser restarts
    /// whenever what is soloed changes.
    pub fn spawn(
        mut consumer: Consumer,
        meters: Meters,
        surface: Option<Arc<dyn Surface>>,
    ) -> Result<Analysis, String> {
        let ch = consumer.channels();
        let has_solo = meters.has_solo();
        let meters = Arc::new(Mutex::new(meters));
        let stop = Arc::new(AtomicBool::new(false));
        let (m, st) = (meters.clone(), stop.clone());
        let stats = consumer.stats();
        let join = std::thread::Builder::new()
            .name("mixd-analysis".into())
            .spawn(move || {
                let mut buf = vec![0f32; CHUNK * ch];
                while !st.load(Ordering::Relaxed) {
                    let n = consumer.pop(&mut buf);
                    if n == 0 {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    // Ask before locking the meters (the sim holds its own lock while rendering).
                    let solo = match (&surface, has_solo) {
                        (Some(s), true) => {
                            Some(s.solo_state().map(|s| s.active).unwrap_or_default())
                        }
                        _ => None,
                    };
                    let mut g = lock(&m);
                    g.dropped = stats.dropped();
                    g.push(&buf[..n * ch], solo.as_deref());
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Analysis {
            meters,
            stop,
            join: Some(join),
        })
    }
}

impl Drop for Analysis {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// The controller's view of the analysis.
pub struct SharedMeter {
    meters: Arc<Mutex<Meters>>,
    kb: &'static KnowledgeBase,
    /// Strip names (for source hints and explanations).
    names: BTreeMap<StripId, String>,
}

impl SharedMeter {
    pub fn new(
        meters: Arc<Mutex<Meters>>,
        kb: &'static KnowledgeBase,
        names: BTreeMap<StripId, String>,
    ) -> Self {
        SharedMeter { meters, kb, names }
    }
}

impl MixMeter for SharedMeter {
    fn restart(&mut self) {
        let mut m = lock(&self.meters);
        m.main.an.reset_session();
        for (_, a) in &mut m.strips {
            a.an.reset_session();
        }
        m.window_frames = 0;
    }

    fn window_s(&self) -> f64 {
        let m = lock(&self.meters);
        m.window_frames as f64 / m.sample_rate as f64
    }

    fn measure(&mut self) -> Option<Measurement> {
        let mut m = lock(&self.meters);
        if m.window_frames == 0 {
            return None;
        }
        let main = m.main.snap("main", Window::Session);
        let kb = self.kb;
        let strips = m
            .strips
            .iter_mut()
            .map(|(id, a)| {
                let f = a.snap(id.as_str(), Window::Session);
                let name = self.names.get(id).map_or(id.as_str(), String::as_str);
                StripTap::new(id.clone(), name, f, kb)
            })
            .collect();
        Some(Measurement { main, strips })
    }

    fn short_term(&mut self) -> Option<Features> {
        let mut m = lock(&self.meters);
        (m.clock_frames > 0).then(|| m.main.snap("main", Window::ShortTerm))
    }

    fn solo_probe(&self) -> Option<SoloProbe> {
        if !lock(&self.meters).has_solo() {
            return None;
        }
        let meters = self.meters.clone();
        Some(Arc::new(move |strip: &StripId| {
            let mut m = lock(&meters);
            let s = m.solo.as_mut()?;
            let soloed = s
                .active
                .iter()
                .any(|t| matches!(t, SoloTarget::Strip { strip: x } if x == strip));
            if !soloed || s.secs < SOLO_MIN_S {
                return None;
            }
            Some(s.tap.snap("solo", Window::Session))
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mix_capture::ring;

    #[test]
    fn analysis_thread_measures_main_and_strips() {
        let layout = TapLayout::stereo(vec![
            Tap::Main,
            Tap::Strip {
                strip: StripId::new("input", 1),
            },
        ]);
        let sr = 48_000;
        let (mut p, c) = ring::channel(sr as usize, layout.ring_channels);
        let a = Analysis::spawn(c, Meters::new(&layout, sr).unwrap(), None).unwrap();
        let mut meter = SharedMeter::new(
            a.meters.clone(),
            KnowledgeBase::embedded(),
            BTreeMap::from([(StripId::new("input", 1), "KICK".to_string())]),
        );
        assert!(meter.measure().is_none());
        // 2 s of a 1 kHz tone at -20 dBFS on every channel
        let frames = 2 * sr as usize;
        let mut pushed = 0;
        while pushed < frames {
            let n = 4800.min(frames - pushed);
            let mut buf = Vec::with_capacity(n * 4);
            for i in pushed..pushed + n {
                let x = 0.1 * (std::f32::consts::TAU * 1000.0 * i as f32 / sr as f32).sin();
                buf.extend([x; 4]);
            }
            while p.free_frames() < n {
                std::thread::sleep(Duration::from_millis(1));
            }
            p.push(&buf);
            pushed += n;
        }
        let t0 = std::time::Instant::now();
        while meter.window_s() < 1.99 {
            assert!(t0.elapsed() < Duration::from_secs(10), "analysis stalled");
            std::thread::sleep(Duration::from_millis(5));
        }
        let m = meter.measure().unwrap();
        assert_eq!(m.strips.len(), 1);
        assert_eq!(m.strips[0].name, "KICK");
        assert_eq!(m.strips[0].source.as_deref(), Some("kick"));
        assert!(m.main.loudness.integrated_lufs.is_some());
        assert!(lock(&a.meters).live.is_some());
        assert!((lock(&a.meters).clock_s() - 2.0).abs() < 1e-6);
        meter.restart();
        assert_eq!(meter.window_s(), 0.0);
        assert!(meter.solo_probe().is_none());
    }
}

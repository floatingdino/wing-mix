//! Deterministic synthetic multitrack songs (seeded): drums, bass, keys, guitar, pad, lead and vocal-like tracks.
//!
//! The song is a 4-chord loop (I-V-vi-IV, key chosen from the seed) at a seeded tempo; vocals sing 3-bar phrases
//! with a bar of rest in which the lead synth answers; backing vocals join every other phrase. Each track is a mono
//! "recorded" signal peaking roughly between -12 and -3 dBFS with a -80 dBFS noise floor, so the console scene (not
//! the generator) sets the balance. Same seed + same length = bit-identical audio on every platform that rounds
//! `f32` the IEEE way (only `+ - * /`, `sin`, `exp` and `tanh` are used).
//!
//! Track layout ([`TRACKS`], WING channel = USB input = position):
//! 1 KICK, 2 SNARE, 3 HAT, 4 TOM, 5 OH L, 6 OH R, 7 BASS, 8 KEYS L, 9 KEYS R, 10 GTR, 11 PAD, 12 LEAD, 13 VOX,
//! 14 BV 1, 15 BV 2, 16 SHAKER.

use crate::surface::{Multitrack, Track};
use std::f32::consts::TAU;

/// Names of the 16 synthetic tracks, in channel order (channel n = index + 1).
pub const TRACKS: [&str; 16] = [
    "KICK", "SNARE", "HAT", "TOM", "OH L", "OH R", "BASS", "KEYS L", "KEYS R", "GTR", "PAD", "LEAD",
    "VOX", "BV 1", "BV 2", "SHAKER",
];

/// Channel of a named track (1-based).
pub fn channel_of(name: &str) -> Option<u8> {
    TRACKS.iter().position(|t| *t == name).map(|i| i as u8 + 1)
}

/// Song parameters.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SongSpec {
    pub seed: u64,
    pub seconds: f64,
    pub sample_rate: u32,
}

impl Default for SongSpec {
    fn default() -> Self {
        SongSpec {
            seed: 1,
            seconds: 10.0,
            sample_rate: 48_000,
        }
    }
}

/// Musical facts derived from the seed (exposed for tests and analysis ground truth).
#[derive(Debug, Clone, PartialEq)]
pub struct SongFacts {
    pub bpm: f32,
    /// MIDI note of the key's tonic (major key).
    pub tonic: i32,
}

/// SplitMix64: tiny deterministic PRNG.
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    pub fn unit(&mut self) -> f32 {
        (self.next_u64() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Uniform in [-1, 1).
    pub fn bipolar(&mut self) -> f32 {
        self.unit() * 2.0 - 1.0
    }

    pub fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.unit()
    }

    pub fn pick(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
}

/// RBJ biquad (transposed direct form II).
#[derive(Debug, Clone, Copy, Default)]
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    z1: f32,
    z2: f32,
}

#[derive(Clone, Copy)]
enum Kind {
    Lp,
    Hp,
    Bp,
}

impl Biquad {
    fn new(kind: Kind, f: f32, q: f32, sr: f32) -> Self {
        let mut b = Biquad::default();
        b.set(kind, f, q, sr);
        b
    }

    fn set(&mut self, kind: Kind, f: f32, q: f32, sr: f32) {
        let w = TAU * (f / sr).min(0.49);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        let a0 = 1.0 + alpha;
        let (b0, b1, b2) = match kind {
            Kind::Lp => ((1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0),
            Kind::Hp => ((1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0),
            // constant 0 dB peak gain
            Kind::Bp => (alpha, 0.0, -alpha),
        };
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = -2.0 * c / a0;
        self.a2 = (1.0 - alpha) / a0;
    }

    #[inline]
    fn run(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

fn midi_hz(m: f32) -> f32 {
    440.0 * (2.0f32).powf((m - 69.0) / 12.0)
}

/// Band-limited sawtooth (polyBLEP), phase in [0, 1).
#[inline]
fn saw(phase: f32, dt: f32) -> f32 {
    let mut v = 2.0 * phase - 1.0;
    if phase < dt {
        let t = phase / dt;
        v -= t + t - t * t - 1.0;
    } else if phase > 1.0 - dt {
        let t = (phase - 1.0) / dt;
        v -= t * t + t + t + 1.0;
    }
    v
}

#[inline]
fn square(phase: f32, dt: f32) -> f32 {
    saw(phase, dt) - saw((phase + 0.5).fract(), dt)
}

/// Shared timing of the song.
struct Clock {
    sr: f32,
    beat: f32,
    frames: usize,
}

impl Clock {
    fn beat_frame(&self, beat: f32) -> usize {
        (beat * self.beat * self.sr) as usize
    }
    fn beats(&self) -> usize {
        (self.frames as f32 / (self.beat * self.sr)).ceil() as usize + 1
    }
    fn bar_of(&self, beat: usize) -> usize {
        beat / 4
    }
}

/// Add `f(t, i)` for `len` frames starting at `start` (clipped to the buffer).
fn add(buf: &mut [f32], start: usize, len: usize, mut f: impl FnMut(f32, usize) -> f32, sr: f32) {
    let end = (start + len).min(buf.len());
    for (i, x) in buf[start.min(end)..end].iter_mut().enumerate() {
        *x += f(i as f32 / sr, i);
    }
}

/// Chord (scale degree offsets from the tonic, root position triad) of bar `bar`: I V vi IV.
fn chord(bar: usize) -> [i32; 3] {
    match bar % 4 {
        0 => [0, 4, 7],
        1 => [7, 11, 14],
        2 => [9, 12, 16],
        _ => [5, 9, 12],
    }
}

/// Major pentatonic degrees (for melodies).
const PENTA: [i32; 5] = [0, 2, 4, 7, 9];

fn normalise(buf: &mut [f32], peak_db: f32, rng: &mut Rng) {
    let peak = buf.iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    let g = if peak > 0.0 {
        10f32.powf(peak_db / 20.0) / peak
    } else {
        0.0
    };
    let floor = 10f32.powf(-80.0 / 20.0);
    for x in buf.iter_mut() {
        *x = *x * g + floor * rng.bipolar();
    }
}

/// Generate the 16-track song. Channel n = USB input n = `TRACKS[n-1]`.
pub fn generate(spec: &SongSpec) -> (Multitrack, SongFacts) {
    let sr = spec.sample_rate as f32;
    let frames = (spec.seconds * spec.sample_rate as f64).round() as usize;
    let mut rng = Rng::new(spec.seed);
    let bpm = 100.0 + 8.0 * rng.pick(5) as f32; // 100..132
    let tonic = 45 + rng.pick(7) as i32; // A2..D#3 region for the bass
    let clk = Clock {
        sr,
        beat: 60.0 / bpm,
        frames,
    };
    let mut t: Vec<Vec<f32>> = (0..TRACKS.len()).map(|_| vec![0.0; frames]).collect();
    let mut trng: Vec<Rng> = (0..TRACKS.len())
        .map(|i| Rng::new(spec.seed.wrapping_mul(31).wrapping_add(i as u64 + 1)))
        .collect();
    let beats = clk.beats();

    // --- drums -------------------------------------------------------------------------------------------------
    let mut kick = vec![0.0f32; frames];
    let mut snare = vec![0.0f32; frames];
    let mut hat = vec![0.0f32; frames];
    let mut crash = vec![0.0f32; frames];
    for b in 0..beats {
        let bar = clk.bar_of(b);
        let in_bar = b % 4;
        // kick on 1 and 3, sometimes on the "and" of 3
        let mut kicks = vec![];
        if in_bar == 0 || in_bar == 2 {
            kicks.push(b as f32);
        }
        if in_bar == 2 && trng[0].unit() < 0.4 {
            kicks.push(b as f32 + 0.5);
        }
        for k in kicks {
            let vel = trng[0].range(0.85, 1.0);
            let mut ph = 0.0f32;
            let mut click = Rng::new(spec.seed ^ (k.to_bits() as u64));
            add(
                &mut kick,
                clk.beat_frame(k),
                (0.5 * sr) as usize,
                |tt, _| {
                    let f = 48.0 + 90.0 * (-tt / 0.03).exp();
                    ph = (ph + f / sr).fract();
                    let body = (TAU * ph).sin() * (-tt / 0.28).exp();
                    let c = if tt < 0.003 { click.bipolar() * 0.5 } else { 0.0 };
                    vel * (body + c)
                },
                sr,
            );
        }
        if in_bar == 1 || in_bar == 3 {
            let vel = trng[1].range(0.8, 1.0);
            let mut bp = Biquad::new(Kind::Bp, 2200.0, 0.6, sr);
            let mut ph = 0.0f32;
            let r = &mut trng[1];
            add(
                &mut snare,
                clk.beat_frame(b as f32),
                (0.4 * sr) as usize,
                |tt, _| {
                    ph = (ph + 185.0 / sr).fract();
                    let tone = (TAU * ph).sin() * (-tt / 0.07).exp() * 0.5;
                    let noise = bp.run(r.bipolar()) * (-tt / 0.13).exp() * 2.2;
                    vel * (tone + noise)
                },
                sr,
            );
        }
        // hats on 8ths
        for half in 0..2 {
            let vel = if half == 0 { 1.0 } else { 0.65 } * trng[2].range(0.8, 1.0);
            let mut hp = Biquad::new(Kind::Hp, 7000.0, 0.7, sr);
            let r = &mut trng[2];
            add(
                &mut hat,
                clk.beat_frame(b as f32 + half as f32 * 0.5),
                (0.12 * sr) as usize,
                |tt, _| vel * hp.run(r.bipolar()) * (-tt / 0.025).exp(),
                sr,
            );
        }
        // tom fill on the last beat of every 2nd bar
        if bar % 2 == 1 && in_bar == 3 {
            for k in 0..4 {
                let f0 = 150.0 - 18.0 * k as f32;
                let mut ph = 0.0f32;
                add(
                    &mut t[3],
                    clk.beat_frame(b as f32 + k as f32 * 0.25),
                    (0.35 * sr) as usize,
                    |tt, _| {
                        let f = f0 * (1.0 + 0.5 * (-tt / 0.02).exp());
                        ph = (ph + f / sr).fract();
                        (TAU * ph).sin() * (-tt / 0.2).exp()
                    },
                    sr,
                );
            }
        }
        // crash at the start of every 4-bar section
        if in_bar == 0 && bar % 4 == 0 {
            let mut hp = Biquad::new(Kind::Hp, 4500.0, 0.7, sr);
            let r = &mut trng[4];
            add(
                &mut crash,
                clk.beat_frame(b as f32),
                (2.0 * sr) as usize,
                |tt, _| hp.run(r.bipolar()) * (-tt / 0.7).exp() * 0.7,
                sr,
            );
        }
    }
    let delay = (0.0006 * sr) as usize;
    for i in 0..frames {
        t[0][i] = kick[i] + 0.02 * snare[i];
        t[1][i] = snare[i] + 0.08 * hat[i] + 0.05 * kick[i];
        t[2][i] = hat[i] + 0.1 * snare[i];
        t[4][i] = 0.12 * kick[i] + 0.35 * snare[i] + 0.45 * hat[i] + crash[i];
        let j = i.saturating_sub(delay);
        t[5][i] = 0.12 * kick[j] + 0.4 * snare[j] + 0.3 * hat[j] + 0.85 * crash[j];
    }

    // --- bass: root on 8ths -----------------------------------------------------------------------------------
    {
        let mut lp = Biquad::new(Kind::Lp, 700.0, 0.8, sr);
        let mut ph = 0.0f32;
        let eighth = clk.beat * 0.5;
        let notes = (frames as f32 / (eighth * sr)).ceil() as usize;
        let buf = &mut t[6];
        for n in 0..notes {
            let bar = n / 8;
            let root = tonic + chord(bar)[0] % 12 - if chord(bar)[0] >= 7 { 12 } else { 0 };
            let octave = if n % 8 == 7 { 12 } else { 0 };
            let f = midi_hz((root + octave) as f32);
            let start = (n as f32 * eighth * sr) as usize;
            let len = (eighth * sr) as usize;
            let end = (start + len).min(frames);
            for (i, x) in buf[start.min(end)..end].iter_mut().enumerate() {
                let tt = i as f32 / sr;
                let dt = f / sr;
                ph = (ph + dt).fract();
                let env = (tt / 0.004).min(1.0) * (0.75 + 0.25 * (-tt / 0.08).exp())
                    * ((len - i) as f32 / (0.01 * sr)).min(1.0);
                *x = lp.run(saw(ph, dt) + 0.6 * (TAU * ph).sin()) * env;
            }
        }
    }

    // --- keys (half-note chords), stereo pair -----------------------------------------------------------------
    for (ti, detune, dly) in [(7usize, 0.0f32, 0usize), (8, 0.03, (0.004 * sr) as usize)] {
        let buf = &mut t[ti];
        for half in 0..(beats / 2) {
            let bar = half / 2;
            let start = clk.beat_frame(half as f32 * 2.0) + dly;
            for &deg in &chord(bar) {
                let f = midi_hz((tonic + 24 + deg) as f32 + detune);
                let mut ph = 0.0f32;
                add(
                    buf,
                    start,
                    (2.0 * clk.beat * sr) as usize,
                    |tt, _| {
                        ph = (ph + f / sr).fract();
                        let s = (TAU * ph).sin() + 0.3 * (2.0 * TAU * ph).sin() + 0.1 * (3.0 * TAU * ph).sin();
                        s * (-tt / 0.9).exp() * (tt / 0.003).min(1.0) * 0.33
                    },
                    sr,
                );
            }
        }
    }

    // --- guitar: Karplus-Strong strums on 2 and 4 -------------------------------------------------------------
    {
        let buf = &mut t[9];
        let r = &mut trng[9];
        for b in (0..beats).filter(|b| b % 2 == 1) {
            let bar = clk.bar_of(b);
            let c = chord(bar);
            let notes = [c[0] + 12, c[1] + 12, c[2] + 12, c[0] + 24, c[1] + 24, c[2] + 24];
            for (k, &deg) in notes.iter().enumerate() {
                let f = midi_hz((tonic + 12 + deg) as f32);
                let n = ((sr / f) as usize).max(2);
                let mut line: Vec<f32> = (0..n).map(|_| r.bipolar() * 0.5).collect();
                let mut idx = 0usize;
                let start = clk.beat_frame(b as f32) + k * (0.012 * sr) as usize;
                add(
                    buf,
                    start,
                    (0.9 * sr) as usize,
                    |_, _| {
                        let next = (idx + 1) % n;
                        let y = line[idx];
                        line[idx] = 0.996 * 0.5 * (line[idx] + line[next]);
                        idx = next;
                        y
                    },
                    sr,
                );
            }
        }
    }

    // --- pad: sustained detuned saws ----------------------------------------------------------------------------
    {
        let mut lp = Biquad::new(Kind::Lp, 1400.0, 0.7, sr);
        let mut phases = [0.0f32; 9];
        let bar_len = 4.0 * clk.beat * sr;
        let buf = &mut t[10];
        for (i, x) in buf.iter_mut().enumerate() {
            let bar = (i as f32 / bar_len) as usize;
            let tt = i as f32 / sr;
            let c = chord(bar);
            let mut s = 0.0;
            for (k, ph) in phases.iter_mut().enumerate() {
                let f = midi_hz((tonic + 24 + c[k / 3]) as f32 + [-0.1, 0.0, 0.1][k % 3]);
                let dt = f / sr;
                *ph = (*ph + dt).fract();
                s += saw(*ph, dt);
            }
            *x = lp.run(s / 9.0) * (tt / 0.8).min(1.0);
        }
    }

    // --- melody (vocal phrases + lead answers) -----------------------------------------------------------------
    // Melody notes: per beat a pentatonic degree near the chord; phrases = bars 0..2 of each 4-bar block sung,
    // bar 3 answered by the lead.
    let mut mel_rng = Rng::new(spec.seed ^ 0xABCD);
    let melody: Vec<i32> = (0..beats)
        .map(|b| {
            let c = chord(clk.bar_of(b));
            if mel_rng.unit() < 0.5 {
                c[mel_rng.pick(3)]
            } else {
                PENTA[mel_rng.pick(5)]
            }
        })
        .collect();
    // vowels (F1, F2, F3)
    const VOWELS: [[f32; 3]; 5] = [
        [730.0, 1090.0, 2440.0],
        [530.0, 1840.0, 2480.0],
        [270.0, 2290.0, 3010.0],
        [570.0, 840.0, 2410.0],
        [300.0, 870.0, 2240.0],
    ];
    let voice = |buf: &mut [f32], interval: i32, gain: f32, only_even_phrases: bool, seed: u64| {
        let mut r = Rng::new(seed);
        let mut ph = 0.0f32;
        let mut f1 = Biquad::new(Kind::Bp, 700.0, 6.0, sr);
        let mut f2 = Biquad::new(Kind::Bp, 1200.0, 8.0, sr);
        let mut f3 = Biquad::new(Kind::Bp, 2500.0, 10.0, sr);
        for b in 0..beats {
            let bar = clk.bar_of(b);
            if bar % 4 == 3 || (only_even_phrases && (bar / 4) % 2 == 1) {
                continue;
            }
            let v = VOWELS[r.pick(5)];
            f1.set(Kind::Bp, v[0], 6.0, sr);
            f2.set(Kind::Bp, v[1], 8.0, sr);
            f3.set(Kind::Bp, v[2], 10.0, sr);
            let base = midi_hz((tonic + 24 + melody[b] + interval) as f32);
            let len = (clk.beat * sr * 0.95) as usize;
            add(
                buf,
                clk.beat_frame(b as f32),
                len,
                |tt, i| {
                    let vib = if tt > 0.15 { 0.006 * (TAU * 5.5 * tt).sin() } else { 0.0 };
                    let f = base * (1.0 + vib);
                    let dt = f / sr;
                    ph = (ph + dt).fract();
                    let src = saw(ph, dt) + 0.02 * r.bipolar();
                    let s = f1.run(src) + 0.6 * f2.run(src) + 0.3 * f3.run(src);
                    let env = (tt / 0.03).min(1.0) * ((len - i) as f32 / (0.03 * sr)).min(1.0);
                    gain * s * env
                },
                sr,
            );
        }
    };
    voice(&mut t[12], 0, 1.0, false, spec.seed ^ 13);
    voice(&mut t[13], 4, 0.8, true, spec.seed ^ 14);
    voice(&mut t[14], -5, 0.8, true, spec.seed ^ 15);
    {
        let mut lp = Biquad::new(Kind::Lp, 3000.0, 0.7, sr);
        let mut ph = 0.0f32;
        let buf = &mut t[11];
        for b in 0..beats {
            if clk.bar_of(b) % 4 != 3 {
                continue;
            }
            for half in 0..2 {
                let f = midi_hz((tonic + 36 + melody[b] + if half == 1 { 2 } else { 0 }) as f32);
                let len = (clk.beat * 0.5 * sr * 0.9) as usize;
                add(
                    buf,
                    clk.beat_frame(b as f32 + half as f32 * 0.5),
                    len,
                    |tt, i| {
                        let dt = f / sr;
                        ph = (ph + dt).fract();
                        let env = (tt / 0.01).min(1.0) * ((len - i) as f32 / (0.01 * sr)).min(1.0);
                        lp.run(square(ph, dt)) * env
                    },
                    sr,
                );
            }
        }
    }

    // --- shaker: 16ths ----------------------------------------------------------------------------------------
    {
        let buf = &mut t[15];
        let r = &mut trng[15];
        for s16 in 0..beats * 4 {
            let accent = if s16 % 2 == 1 { 1.0 } else { 0.55 };
            let mut bp = Biquad::new(Kind::Bp, 6000.0, 1.2, sr);
            add(
                buf,
                clk.beat_frame(s16 as f32 * 0.25),
                (0.06 * sr) as usize,
                |tt, _| accent * bp.run(r.bipolar()) * (tt / 0.004).min(1.0) * (-tt / 0.02).exp(),
                sr,
            );
        }
    }

    // --- level the "recordings" --------------------------------------------------------------------------------
    let peaks = [
        -3.0, -4.0, -12.0, -6.0, -8.0, -8.0, -5.0, -9.0, -9.0, -8.0, -10.0, -9.0, -4.0, -8.0, -8.0, -14.0,
    ];
    let mut floor_rng = Rng::new(spec.seed ^ 0xF100);
    for (buf, p) in t.iter_mut().zip(peaks) {
        normalise(buf, p, &mut floor_rng);
    }

    let tracks = t
        .into_iter()
        .enumerate()
        .map(|(i, audio)| Track {
            name: TRACKS[i].into(),
            channel: i as u8 + 1,
            usb: i as u8 + 1,
            audio,
        })
        .collect();
    (
        Multitrack {
            sample_rate: spec.sample_rate,
            tracks,
        },
        SongFacts { bpm, tonic },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rms_db(x: &[f32]) -> f32 {
        let e: f64 = x.iter().map(|&v| (v as f64) * (v as f64)).sum::<f64>() / x.len() as f64;
        (10.0 * e.max(1e-20).log10()) as f32
    }

    #[test]
    fn deterministic_and_seeded() {
        let spec = SongSpec {
            seed: 7,
            seconds: 2.0,
            sample_rate: 48_000,
        };
        let (a, fa) = generate(&spec);
        let (b, fb) = generate(&spec);
        assert_eq!(fa, fb);
        for (x, y) in a.tracks.iter().zip(&b.tracks) {
            assert_eq!(x.audio, y.audio, "{}", x.name);
        }
        let (c, _) = generate(&SongSpec { seed: 8, ..spec });
        assert_ne!(a.tracks[12].audio, c.tracks[12].audio);
    }

    #[test]
    fn every_track_has_signal_in_range() {
        let (m, facts) = generate(&SongSpec {
            seed: 3,
            seconds: 8.0,
            sample_rate: 48_000,
        });
        assert!((100.0..=132.0).contains(&facts.bpm));
        assert_eq!(m.tracks.len(), 16);
        assert_eq!(m.frames(), 8 * 48_000);
        for t in &m.tracks {
            let peak = t.audio.iter().fold(0.0f32, |p, &x| p.max(x.abs()));
            assert!(peak <= 0.75 && peak > 0.1, "{}: peak {peak}", t.name);
            assert!(t.audio.iter().all(|x| x.is_finite()), "{}", t.name);
            assert!(rms_db(&t.audio) > -50.0, "{}: {} dB", t.name, rms_db(&t.audio));
        }
        assert_eq!(channel_of("VOX"), Some(13));
    }
}

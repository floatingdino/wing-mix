//! Song alignment: find where a reference recording is played in the live multitrack (M016).
//!
//! Method (deliberately simple, see fixtures/README.md for the results):
//! 1. A coarse mono mixdown of the multitrack at [`ALIGN_RATE`] (project gains applied, muted tracks skipped;
//!    [`mixdown`] streams the 95 min in 30 s blocks) and the reference folded to mono at the same rate.
//! 2. Chroma features ([`chroma`]): 0.5 s hop, Hann STFT, 80 Hz - 2 kHz bins folded to 12 pitch classes,
//!    L1-normalised, square-root compressed, smoothed over ~2.5 s and L2-normalised (CENS-like; robust to mix,
//!    timbre and loudness differences between a studio master and a live band).
//! 3. Subsequence DTW ([`align_chroma`]) of the reference chroma against the whole multitrack, cost
//!    `1 - cosine`, steps (1,1), (1,0), (0,1), free start/end in the multitrack, tried for all 12 circular chroma
//!    shifts (live keys often differ from the record).
//! 4. Confidence ([`Alignment::confidence`]): the margin of the best path's mean cost against the best
//!    non-overlapping alternative path and against the best other transposition.
//!
//! The warp path is kept as `(reference_s, multitrack_s)` anchors so reference section times map to multitrack
//! times ([`map_time`]) without re-running the alignment.

use crate::{AudioTrack, Fixture, FixtureError};
use realfft::RealFftPlanner;

/// Rate of the mixdown / reference used for chroma (Hz).
pub const ALIGN_RATE: u32 = 8000;
/// Chroma hop (seconds).
pub const HOP_S: f64 = 0.5;
/// Chroma frames smoothed together (moving average), ~2.5 s.
const SMOOTH: usize = 5;
const F_LO: f32 = 80.0;
const F_HI: f32 = 2000.0;
/// Extra cost of a non-diagonal DTW step, so ties (a held chord) resolve to the diagonal.
const STEP_PENALTY: f32 = 0.1;
const CENTRE: bool = true;

/// One 12-bin chroma vector (pitch class C = 0), L2-normalised (or all zero for silence).
pub type ChromaFrame = [f32; 12];

/// Integer-factor decimation with a windowed-sinc low-pass (cutoff 0.45 of the output rate).
pub fn decimate(x: &[f32], factor: usize) -> Vec<f32> {
    if factor <= 1 {
        return x.to_vec();
    }
    let taps = 16 * factor + 1;
    let half = (taps / 2) as isize;
    let fc = 0.45 / factor as f64; // cycles/sample at the input rate
    let h: Vec<f32> = (0..taps as isize)
        .map(|i| {
            let n = (i - half) as f64;
            let sinc = if n == 0.0 {
                2.0 * fc
            } else {
                (2.0 * std::f64::consts::PI * fc * n).sin() / (std::f64::consts::PI * n)
            };
            let w = 0.5
                - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / (taps as f64 - 1.0)).cos();
            (sinc * w) as f32
        })
        .collect();
    let n_out = x.len() / factor;
    let mut out = Vec::with_capacity(n_out);
    for o in 0..n_out {
        let c = (o * factor) as isize;
        let mut acc = 0.0f32;
        for (k, &hk) in h.iter().enumerate() {
            let i = c + k as isize - half;
            if i >= 0 && (i as usize) < x.len() {
                acc += hk * x[i as usize];
            }
        }
        out.push(acc);
    }
    out
}

/// Mono mixdown of the whole multitrack at [`ALIGN_RATE`]: project gains applied, muted tracks skipped, read in
/// `block_s` blocks (each block decimated on its own; edge effects are irrelevant for chroma).
pub fn mixdown(fx: &Fixture, block_s: f64) -> Result<Vec<f32>, FixtureError> {
    mixdown_where(fx, block_s, |t| !t.mute)
}

/// [`mixdown`] of the tracks selected by `pick` (project gains applied).
pub fn mixdown_where(
    fx: &Fixture,
    block_s: f64,
    pick: impl Fn(&AudioTrack) -> bool,
) -> Result<Vec<f32>, FixtureError> {
    let sr = fx.sample_rate();
    let factor = (sr / ALIGN_RATE) as usize;
    if factor == 0 || sr % ALIGN_RATE != 0 {
        return Err(FixtureError::Range(format!(
            "sample rate {sr} is not a multiple of {ALIGN_RATE}"
        )));
    }
    let total = fx.duration();
    let mut out = Vec::with_capacity((total * ALIGN_RATE as f64) as usize + 1);
    let mut t = 0.0;
    while t < total {
        let dur = block_s.min(total - t);
        let ex = fx.excerpt(t, dur)?;
        let mut mix = vec![0.0f32; ex.frames];
        for (tr, a) in fx.tracks.iter().zip(&ex.audio) {
            if !pick(tr) {
                continue;
            }
            let g = tr.project_gain as f32;
            for (m, s) in mix.iter_mut().zip(a) {
                *m += s * g;
            }
        }
        out.extend(decimate(&mix, factor));
        t += block_s;
    }
    Ok(out)
}

/// CENS-like chroma of a mono signal (see the module doc). One frame per [`HOP_S`]; frame `i` is centred at
/// `i * HOP_S` seconds.
pub fn chroma(mono: &[f32], rate: u32) -> Vec<ChromaFrame> {
    let hop = (rate as f64 * HOP_S).round() as usize;
    chroma_with(mono, rate, hop_s_frames(rate, HOP_S), hop.next_power_of_two(), SMOOTH)
}

fn hop_s_frames(rate: u32, hop_s: f64) -> usize {
    (rate as f64 * hop_s).round() as usize
}

/// [`chroma`] with an explicit hop (samples), FFT size and smoothing length (frames).
pub fn chroma_with(mono: &[f32], rate: u32, hop: usize, n: usize, smooth: usize) -> Vec<ChromaFrame> {
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let mut inbuf = fft.make_input_vec();
    let mut spec = fft.make_output_vec();
    let win: Vec<f32> = (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / n as f32).cos())
        .collect();
    // Bin -> pitch class.
    let pc: Vec<Option<usize>> = (0..n / 2 + 1)
        .map(|k| {
            let f = k as f32 * rate as f32 / n as f32;
            (F_LO..=F_HI).contains(&f).then(|| {
                let midi = (12.0 * (f / 440.0).log2() + 69.0).round() as i32;
                midi.rem_euclid(12) as usize
            })
        })
        .collect();
    let frames = mono.len() / hop + 1;
    let mut raw: Vec<ChromaFrame> = Vec::with_capacity(frames);
    let mut energy: Vec<f32> = Vec::with_capacity(frames);
    for i in 0..frames {
        let c = (i * hop) as isize - (n / 2) as isize;
        for (j, b) in inbuf.iter_mut().enumerate() {
            let s = c + j as isize;
            *b = if s >= 0 && (s as usize) < mono.len() {
                mono[s as usize] * win[j]
            } else {
                0.0
            };
        }
        fft.process(&mut inbuf, &mut spec).expect("fft sizes");
        let mut v = [0.0f32; 12];
        for (k, z) in spec.iter().enumerate() {
            if let Some(p) = pc[k] {
                v[p] += z.norm_sqr();
            }
        }
        let e: f32 = v.iter().sum();
        energy.push(e);
        raw.push(v);
    }
    // Silence gate relative to the loudest frames (95th percentile).
    let mut sorted = energy.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let gate = sorted.get(sorted.len() * 95 / 100).copied().unwrap_or(0.0) * 1e-4;
    let comp: Vec<ChromaFrame> = raw
        .iter()
        .zip(&energy)
        .map(|(v, &e)| {
            let mut o = [0.0f32; 12];
            if e > gate && e > 0.0 {
                for (o, x) in o.iter_mut().zip(v) {
                    *o = (x / e).sqrt();
                }
            }
            o
        })
        .collect();
    (0..comp.len())
        .map(|i| {
            let lo = i.saturating_sub(smooth / 2);
            let hi = (i + smooth / 2 + 1).min(comp.len());
            let mut o = [0.0f32; 12];
            for f in &comp[lo..hi] {
                for (o, x) in o.iter_mut().zip(f) {
                    *o += x;
                }
            }
            normalise(o)
        })
        .collect()
}

fn normalise(mut v: ChromaFrame) -> ChromaFrame {
    if CENTRE {
        let mean = v.iter().sum::<f32>() / 12.0;
        v.iter_mut().for_each(|x| *x -= mean);
    }
    let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if n > 1e-9 {
        v.iter_mut().for_each(|x| *x /= n);
    }
    v
}

/// Circularly shift a chroma frame up by `k` semitones.
pub fn shift(v: &ChromaFrame, k: usize) -> ChromaFrame {
    let mut o = [0.0f32; 12];
    for (i, x) in v.iter().enumerate() {
        o[(i + k) % 12] = *x;
    }
    o
}

#[inline]
fn cost(a: &ChromaFrame, b: &ChromaFrame) -> f32 {
    1.0 - a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>()
}

/// Result of [`align_chroma`]. Times are seconds (reference time / multitrack time relative to the start of the
/// chroma sequences passed in).
#[derive(Debug, Clone)]
pub struct Alignment {
    /// Semitones the reference was shifted up to match the multitrack (0..12; 10 means "2 down").
    pub transpose: usize,
    pub start_s: f64,
    pub end_s: f64,
    /// Mean `1 - cosine` along the best path.
    pub mean_cost: f32,
    /// Best non-overlapping alternative path (same transposition), mean cost.
    pub alt_cost: f32,
    /// Best path with any other transposition, mean cost.
    pub other_key_cost: f32,
    /// Warp path `(reference_s, multitrack_s)`, one point per reference frame (monotonic).
    pub path: Vec<(f64, f64)>,
    /// Per reference frame cost along the path (same length as `path`).
    pub path_cost: Vec<f32>,
}

impl Alignment {
    /// 0..1: relative margin of the best path over the best alternative (other place or other key). Above ~0.3 the
    /// match is unambiguous; below ~0.1 treat the song as not found.
    pub fn confidence(&self) -> f32 {
        let alt = self.alt_cost.min(self.other_key_cost);
        if alt <= 0.0 {
            return 0.0;
        }
        ((alt - self.mean_cost) / alt).clamp(0.0, 1.0)
    }

    /// Mean path cost over reference times `[from, to)` (NaN when empty).
    pub fn segment_cost(&self, from: f64, to: f64) -> f32 {
        let v: Vec<f32> = self
            .path
            .iter()
            .zip(&self.path_cost)
            .filter(|((r, _), _)| *r >= from && *r < to)
            .map(|(_, c)| *c)
            .collect();
        v.iter().sum::<f32>() / v.len() as f32
    }
}

/// Subsequence DTW over the last row only: (total cost, path length, start column) per end column.
fn sdtw_last_row(q: &[ChromaFrame], d: &[ChromaFrame]) -> Vec<(f32, u32, u32)> {
    let m = d.len();
    let mut prev: Vec<(f32, u32, u32)> = (0..m)
        .map(|j| (cost(&q[0], &d[j]), 1, j as u32))
        .collect();
    let mut cur = prev.clone();
    for qi in &q[1..] {
        for j in 0..m {
            let c = cost(qi, &d[j]);
            let up = prev[j];
            let mut best = (up.0 + STEP_PENALTY, up.1, up.2);
            if j > 0 {
                let diag = prev[j - 1];
                let left = cur[j - 1];
                let left = (left.0 + STEP_PENALTY, left.1, left.2);
                if diag.0 <= best.0 {
                    best = diag;
                }
                if left.0 < best.0 {
                    best = left;
                }
            }
            cur[j] = (best.0 + c, best.1 + 1, best.2);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev
}

/// Best end column by mean cost, optionally excluding columns `[lo, hi)`.
fn best_end(row: &[(f32, u32, u32)], exclude: Option<(usize, usize)>) -> Option<(usize, f32)> {
    row.iter()
        .enumerate()
        .filter(|(j, _)| exclude.is_none_or(|(lo, hi)| *j < lo || *j >= hi))
        .map(|(j, r)| (j, r.0 / r.1 as f32))
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// Full DTW over a window of the database with backtracking; returns `(query_idx, db_idx, cost)` per query frame
/// (the db index is the last one the path visits for that query frame).
fn sdtw_path(q: &[ChromaFrame], d: &[ChromaFrame], end: usize) -> Vec<(usize, usize, f32)> {
    let (n, m) = (q.len(), end + 1);
    let mut acc = vec![f32::INFINITY; n * m];
    let mut step = vec![0u8; n * m]; // 0 diag, 1 up (query advances), 2 left (db advances), 3 start
    for j in 0..m {
        acc[j] = cost(&q[0], &d[j]);
        step[j] = 3;
    }
    for i in 1..n {
        for j in 0..m {
            let c = cost(&q[i], &d[j]);
            let mut best = (acc[(i - 1) * m + j] + STEP_PENALTY, 1u8);
            if j > 0 {
                let diag = acc[(i - 1) * m + j - 1];
                if diag <= best.0 {
                    best = (diag, 0);
                }
                let left = acc[i * m + j - 1] + STEP_PENALTY;
                if left < best.0 {
                    best = (left, 2);
                }
            }
            acc[i * m + j] = best.0 + c;
            step[i * m + j] = best.1;
        }
    }
    let (mut i, mut j) = (n - 1, end);
    let mut out = vec![(i, j, cost(&q[i], &d[j]))];
    loop {
        match step[i * m + j] {
            3 => break,
            0 => {
                i -= 1;
                j -= 1;
            }
            1 => i -= 1,
            _ => j -= 1,
        }
        if out.last().map(|o| o.0) != Some(i) {
            out.push((i, j, cost(&q[i], &d[j])));
        }
    }
    out.reverse();
    out
}

/// Align reference chroma `reference` inside `multitrack` chroma, all transpositions, whole range (see the
/// module doc). Forces the whole reference onto one monotonic path; for songs whose live version omits or
/// reorders parts use [`locate`].
pub fn align_chroma(reference: &[ChromaFrame], multitrack: &[ChromaFrame]) -> Option<Alignment> {
    align_window(reference, multitrack, &(0..12).collect::<Vec<_>>(), 0, multitrack.len())
}

/// [`align_chroma`] restricted to the given transpositions and multitrack frames `lo..hi` (times in the result
/// are still relative to the start of `multitrack`).
pub fn align_window(
    reference: &[ChromaFrame],
    multitrack: &[ChromaFrame],
    shifts: &[usize],
    lo: usize,
    hi: usize,
) -> Option<Alignment> {
    let hi = hi.min(multitrack.len());
    if reference.len() < 2 || hi < lo + 2 || shifts.is_empty() {
        return None;
    }
    let db = &multitrack[lo..hi];
    let per_key: Vec<(usize, Vec<(f32, u32, u32)>)> = std::thread::scope(|s| {
        let hs: Vec<_> = shifts
            .iter()
            .map(|&k| {
                s.spawn(move || {
                    let q: Vec<ChromaFrame> = reference.iter().map(|v| shift(v, k)).collect();
                    (k, sdtw_last_row(&q, db))
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("dtw")).collect()
    });
    let mut keyed: Vec<(usize, usize, f32)> = per_key
        .iter()
        .filter_map(|(k, row)| best_end(row, None).map(|(j, c)| (*k, j, c)))
        .collect();
    keyed.sort_by(|a, b| a.2.total_cmp(&b.2));
    let (k, end, mean_cost) = keyed[0];
    let other_key_cost = keyed.get(1).map_or(1.0, |x| x.2);
    let row = &per_key.iter().find(|(kk, _)| *kk == k)?.1;
    let start = row[end].2 as usize;
    // Exclude ends whose path would overlap the found one.
    let span = end - start + 1;
    let alt_cost = best_end(row, Some((start, end + span))).map_or(1.0, |(_, c)| c);
    let q: Vec<ChromaFrame> = reference.iter().map(|v| shift(v, k)).collect();
    // Backtrack in a window around the found span (the full matrix over 95 min would be ~75 MB).
    let w0 = start.saturating_sub(q.len());
    let path: Vec<(usize, usize, f32)> = sdtw_path(&q, &db[w0..=end], end - w0)
        .into_iter()
        .map(|(i, j, c)| (i, j + w0 + lo, c))
        .collect();
    Some(Alignment {
        transpose: k,
        start_s: path.first().map_or(0, |p| p.1) as f64 * HOP_S,
        end_s: (end + lo) as f64 * HOP_S,
        mean_cost,
        alt_cost,
        other_key_cost,
        path: path
            .iter()
            .map(|&(i, j, _)| (i as f64 * HOP_S, j as f64 * HOP_S))
            .collect(),
        path_cost: path.iter().map(|p| p.2).collect(),
    })
}

/// Hop of the fine chroma used by [`locate`] (seconds; no smoothing).
pub const FINE_HOP_S: f64 = 0.1;
/// Reference block length and hop for [`locate`] (seconds).
pub const BLOCK_S: f64 = 30.0;
pub const BLOCK_HOP_S: f64 = 15.0;
/// Width of the region one live performance is assumed to fit in (seconds).
pub const VOTE_WINDOW_S: f64 = 480.0;
/// Peak prominence a block match needs to count; chance peaks reach ~0.15-0.2, true ones 0.2-0.35.
pub const PROMINENCE_FLOOR: f32 = 0.12;
/// Block offsets closer than this (seconds) belong to one stretch played along the record's timeline (allows a
/// few % of tempo drift over a 30 s block hop).
pub const OFFSET_TOL_S: f64 = 1.5;

/// Fine chroma for [`locate`]: [`FINE_HOP_S`] hop, 4096-point frames at [`ALIGN_RATE`], unsmoothed.
pub fn fine_chroma(mono: &[f32], rate: u32) -> Vec<ChromaFrame> {
    let hop = hop_s_frames(rate, FINE_HOP_S);
    chroma_with(mono, rate, hop, (rate as usize / 2).next_power_of_two(), 1)
}

/// One reference block found in the multitrack at a fixed offset (tempo 1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockMatch {
    pub ref_start_s: f64,
    pub ref_end_s: f64,
    pub transpose: usize,
    /// `multitrack_s = reference_s + offset_s`.
    pub offset_s: f64,
    /// Mean cosine similarity along the diagonal.
    pub height: f32,
    /// Height minus the mean 1-4 s either side: how much better this exact offset is than its neighbours.
    pub prominence: f32,
}

impl BlockMatch {
    pub fn start_s(&self) -> f64 {
        self.ref_start_s + self.offset_s
    }
    pub fn end_s(&self) -> f64 {
        self.ref_end_s + self.offset_s
    }
}

/// Result of [`locate`].
#[derive(Debug, Clone)]
pub struct SongMatch {
    pub transpose: usize,
    /// Multitrack range covered by the anchored matches (seconds).
    pub start_s: f64,
    pub end_s: f64,
    /// Best match of every reference block (any key, anywhere).
    pub blocks: Vec<BlockMatch>,
    /// Indices into `blocks` that voted for the chosen key and lie in `start_s..end_s`.
    pub members: Vec<usize>,
    /// Members that share their offset with another member (stretches played along the record).
    pub anchored: Vec<usize>,
    /// Vote score (sum of prominence above [`PROMINENCE_FLOOR`]) of the winner and of the best other key/region.
    pub score: f32,
    pub runner_up: f32,
}

impl SongMatch {
    /// 0..1: `(score - runner_up) / score`, scaled down when fewer than 4 blocks are anchored (share an offset).
    /// A song the band played along the record scores > 0.6; a song that is absent scatters its votes over keys
    /// and offsets and scores ~0.
    pub fn confidence(&self) -> f32 {
        if self.score <= 0.0 {
            return 0.0;
        }
        let support = (self.anchored.len() as f32 / 4.0).min(1.0);
        ((self.score - self.runner_up) / self.score * support).clamp(0.0, 1.0)
    }

    /// Anchored matches merged into segments `(ref_start_s, ref_end_s, offset_s)` (offset within
    /// [`OFFSET_TOL_S`] and overlapping or adjacent reference ranges), sorted by reference time.
    pub fn segments(&self) -> Vec<(f64, f64, f64)> {
        let mut m: Vec<BlockMatch> = self.anchored.iter().map(|&i| self.blocks[i]).collect();
        m.sort_by(|a, b| a.ref_start_s.total_cmp(&b.ref_start_s));
        let mut out: Vec<(f64, f64, f64)> = Vec::new();
        for b in m {
            match out
                .iter_mut()
                .find(|s| (s.2 - b.offset_s).abs() < OFFSET_TOL_S && b.ref_start_s <= s.1 + 1e-9)
            {
                Some(s) => s.1 = s.1.max(b.ref_end_s),
                None => out.push((b.ref_start_s, b.ref_end_s, b.offset_s)),
            }
        }
        out
    }
}

/// Per reference block, the most prominent (transposition, offset) over the whole multitrack, at tempo 1.
fn block_matches(reference: &[ChromaFrame], multitrack: &[ChromaFrame]) -> Vec<BlockMatch> {
    let (n, m) = (reference.len(), multitrack.len());
    let sub = (BLOCK_HOP_S / FINE_HOP_S).round() as usize;
    let per_block = (BLOCK_S / BLOCK_HOP_S).round() as usize;
    let nsub = n.div_ceil(sub);
    if nsub < per_block || m < 100 {
        return Vec::new();
    }
    // acc[k][sub-block][o + n] = sum over the sub-block's frames i of sim(shift(ref[i], k), mt[i + o]).
    let acc: Vec<Vec<Vec<f32>>> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..12)
            .map(|k| {
                s.spawn(move || {
                    let mut acc = vec![vec![0f32; n + m]; nsub];
                    for (i, r) in reference.iter().enumerate() {
                        let q = shift(r, k);
                        let row = &mut acc[i / sub][n - i..n - i + m];
                        for (dst, d) in row.iter_mut().zip(multitrack) {
                            let mut s = 0.0;
                            for c in 0..12 {
                                s += q[c] * d[c];
                            }
                            *dst += s;
                        }
                    }
                    acc
                })
            })
            .collect();
        hs.into_iter().map(|h| h.join().expect("block scan")).collect()
    });
    let (near, far) = (
        (1.0 / FINE_HOP_S) as usize,
        (4.0 / FINE_HOP_S) as usize,
    );
    let mut out = Vec::new();
    for b in 0..=nsub - per_block {
        let len = (b * sub + per_block * sub).min(n) - b * sub;
        let mut best: Option<BlockMatch> = None;
        let mut sc = vec![0f32; n + m];
        let mut pre = vec![0f64; n + m + 1];
        for (k, acc_k) in acc.iter().enumerate() {
            sc.iter_mut().for_each(|v| *v = 0.0);
            for row in &acc_k[b..b + per_block] {
                for (v, x) in sc.iter_mut().zip(row) {
                    *v += x;
                }
            }
            sc.iter_mut().for_each(|v| *v /= len as f32);
            for (i, v) in sc.iter().enumerate() {
                pre[i + 1] = pre[i] + *v as f64;
            }
            for o in far..n + m - far - 1 {
                let side = (pre[o - near] - pre[o - far] + pre[o + far + 1] - pre[o + near + 1])
                    / (2 * (far - near)) as f64;
                let prom = sc[o] - side as f32;
                if best.is_none_or(|bm| prom > bm.prominence) {
                    let ref_start_s = (b * sub) as f64 * FINE_HOP_S;
                    best = Some(BlockMatch {
                        ref_start_s,
                        ref_end_s: ref_start_s + len as f64 * FINE_HOP_S,
                        transpose: k,
                        offset_s: (o as f64 - n as f64) * FINE_HOP_S,
                        height: sc[o],
                        prominence: prom,
                    });
                }
            }
        }
        out.extend(best);
    }
    out
}

/// Best (transposition, member indices, score) over [`VOTE_WINDOW_S`] regions, skipping `exclude`d indices.
fn vote(blocks: &[BlockMatch], exclude: &[usize]) -> Option<(usize, Vec<usize>, f32)> {
    let w = |b: &BlockMatch| (b.prominence - PROMINENCE_FLOOR).max(0.0);
    let mut best: Option<(usize, Vec<usize>, f32)> = None;
    for (ai, a) in blocks.iter().enumerate() {
        if exclude.contains(&ai) || w(a) <= 0.0 {
            continue;
        }
        let members: Vec<usize> = (0..blocks.len())
            .filter(|i| !exclude.contains(i))
            .filter(|&i| {
                let b = &blocks[i];
                w(b) > 0.0
                    && b.transpose == a.transpose
                    && b.start_s() >= a.start_s()
                    && b.end_s() <= a.start_s() + VOTE_WINDOW_S
            })
            .collect();
        let score: f32 = members.iter().map(|&i| w(&blocks[i])).sum();
        if best.as_ref().is_none_or(|b| score > b.2) {
            best = Some((a.transpose, members, score));
        }
    }
    best
}

/// Locate a reference song in the multitrack, assuming the band plays at the record's tempo (to a click or
/// backing tracks, as in this show) but possibly in another key, cut short, repeated or rearranged: every
/// [`BLOCK_S`] reference block (hop [`BLOCK_HOP_S`]) is matched on its own at every offset and key using
/// [`fine_chroma`]; blocks whose best match is sharply better than the neighbouring offsets vote, and the key +
/// [`VOTE_WINDOW_S`] region with the most prominence wins. See [`SongMatch::confidence`]. For performances at a
/// different tempo use [`align_chroma`] (DTW) instead.
pub fn locate(reference: &[ChromaFrame], multitrack: &[ChromaFrame]) -> Option<SongMatch> {
    let blocks = block_matches(reference, multitrack);
    let (transpose, voters, _) = vote(&blocks, &[])?;
    // Keep the part of the region backed by repeated offsets: >= 2 blocks within OFFSET_TOL_S of each other are a
    // stretch the band played along the record's timeline; lone blocks only count inside that span.
    let mut by_off = voters.clone();
    by_off.sort_by(|&a, &b| blocks[a].offset_s.total_cmp(&blocks[b].offset_s));
    let mut anchored: Vec<usize> = Vec::new();
    let mut group: Vec<usize> = Vec::new();
    for &i in &by_off {
        if group
            .last()
            .is_some_and(|&g| blocks[i].offset_s - blocks[g].offset_s > OFFSET_TOL_S)
        {
            if group.len() >= 2 {
                anchored.extend(&group);
            }
            group.clear();
        }
        group.push(i);
    }
    if group.len() >= 2 {
        anchored.extend(&group);
    }
    let span_of = |set: &[usize]| {
        (
            set.iter().map(|&i| blocks[i].start_s()).fold(f64::MAX, f64::min),
            set.iter().map(|&i| blocks[i].end_s()).fold(f64::MIN, f64::max),
        )
    };
    let (start_s, end_s) = span_of(if anchored.is_empty() { &voters } else { &anchored });
    let members: Vec<usize> = voters
        .iter()
        .copied()
        .filter(|&i| {
            let b = &blocks[i];
            b.start_s() >= start_s - 1e-9 && b.end_s() <= end_s + 1e-9
        })
        .collect();
    let w = |b: &BlockMatch| (b.prominence - PROMINENCE_FLOOR).max(0.0);
    let score: f32 = members.iter().map(|&i| w(&blocks[i])).sum();
    // Runner-up: the best region made of blocks that did not vote for the winner, in another key or elsewhere.
    let excl: Vec<usize> = (0..blocks.len())
        .filter(|&i| {
            let b = &blocks[i];
            members.contains(&i)
                || (b.transpose == transpose && b.end_s() > start_s && b.start_s() < end_s)
        })
        .collect();
    let runner_up = vote(&blocks, &excl).map_or(0.0, |v| v.2);
    Some(SongMatch {
        transpose,
        start_s,
        end_s,
        blocks,
        members,
        anchored,
        score,
        runner_up,
    })
}

/// Multitrack range of reference range `[from, to)` through `segments` (`(ref_start_s, ref_end_s, offset_s)`, see
/// [`SongMatch::segments`]): the part of the range covered by the segment overlapping it most, shifted by that
/// segment's offset. `None` when no segment overlaps (that part of the record was not found live).
pub fn map_range(segments: &[(f64, f64, f64)], from: f64, to: f64) -> Option<(f64, f64)> {
    segments
        .iter()
        .map(|&(a, b, off)| (from.max(a), to.min(b), off))
        .filter(|(a, b, _)| b > a)
        .max_by(|x, y| (x.1 - x.0).total_cmp(&(y.1 - y.0)))
        .map(|(a, b, off)| (a + off, b + off))
}

/// Map a reference time to multitrack time by linear interpolation between `anchors` (`(reference_s,
/// multitrack_s)`, increasing); clamps/extrapolates with slope 1 outside the anchored range.
pub fn map_time(anchors: &[(f64, f64)], t: f64) -> f64 {
    match anchors {
        [] => t,
        [(r, m)] => m + (t - r),
        _ => {
            let first = anchors[0];
            let last = anchors[anchors.len() - 1];
            if t <= first.0 {
                return first.1 + (t - first.0);
            }
            if t >= last.0 {
                return last.1 + (t - last.0);
            }
            let i = anchors.partition_point(|a| a.0 <= t).max(1);
            let (a, b) = (anchors[i - 1], anchors[i]);
            if b.0 <= a.0 {
                return a.1;
            }
            a.1 + (t - a.0) / (b.0 - a.0) * (b.1 - a.1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A toy "song": a sequence of chords (pitch-class sets), each `dur` seconds of summed sines.
    fn song(chords: &[&[usize]], dur: f64, transpose: usize, tempo: f64) -> Vec<f32> {
        let sr = ALIGN_RATE as f64;
        let mut out = Vec::new();
        for ch in chords {
            let n = (dur / tempo * sr) as usize;
            for i in 0..n {
                let t = i as f64 / sr;
                let mut s = 0.0;
                for &pc in ch.iter() {
                    let midi = 48 + ((pc + transpose) % 12) as i32;
                    let f = 440.0 * 2f64.powf((midi as f64 - 69.0) / 12.0);
                    for h in 1..=3 {
                        s += (2.0 * std::f64::consts::PI * f * h as f64 * t).sin() / h as f64;
                    }
                }
                out.push((0.1 * s) as f32);
            }
        }
        out
    }

    const PROG: &[&[usize]] = &[
        &[0, 4, 7],
        &[9, 0, 4],
        &[5, 9, 0],
        &[7, 11, 2],
        &[2, 5, 9],
        &[4, 7, 11],
        &[0, 4, 7],
        &[5, 9, 0],
    ];
    const OTHER: &[&[usize]] = &[&[1, 5, 8], &[3, 6, 10], &[6, 10, 1], &[8, 0, 3]];

    #[test]
    fn decimate_keeps_low_and_kills_high() {
        let sr = 48000.0;
        let lo: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f64::consts::PI * 500.0 * i as f64 / sr).sin() as f32)
            .collect();
        let hi: Vec<f32> = (0..48000)
            .map(|i| (2.0 * std::f64::consts::PI * 10000.0 * i as f64 / sr).sin() as f32)
            .collect();
        let rms = |v: &[f32]| (v.iter().map(|x| x * x).sum::<f32>() / v.len() as f32).sqrt();
        let dl = decimate(&lo, 6);
        let dh = decimate(&hi, 6);
        assert_eq!(dl.len(), 8000);
        assert!((rms(&dl[100..7900]) - rms(&lo)).abs() < 0.02);
        assert!(rms(&dh[100..7900]) < 0.01, "{}", rms(&dh));
    }

    #[test]
    fn chroma_finds_pitch_class() {
        let s = song(&[&[9]], 3.0, 0, 1.0); // A
        let c = chroma(&s, ALIGN_RATE);
        let mid = c[c.len() / 2];
        let argmax = (0..12).max_by(|&a, &b| mid[a].total_cmp(&mid[b])).unwrap();
        assert_eq!(argmax, 9);
        assert!(shift(&mid, 3)[0] == mid[9]);
    }

    #[test]
    fn finds_transposed_slower_song_in_a_set() {
        // "Live set": other material, then the song 2 semitones up and 10 % slower, then other material.
        let reference = song(PROG, 4.0, 0, 1.0);
        let mut live = song(OTHER, 5.0, 0, 1.0);
        let at = live.len() as f64 / ALIGN_RATE as f64;
        live.extend(song(PROG, 4.0, 2, 0.9));
        let end = live.len() as f64 / ALIGN_RATE as f64;
        live.extend(song(OTHER, 5.0, 0, 1.0));
        let a = align_chroma(&chroma(&reference, ALIGN_RATE), &chroma(&live, ALIGN_RATE)).unwrap();
        assert_eq!(a.transpose, 2);
        assert!((a.start_s - at).abs() < 2.0, "start {} vs {at}", a.start_s);
        assert!((a.end_s - end).abs() < 2.0, "end {} vs {end}", a.end_s);
        assert!(a.confidence() > 0.3, "{a:?}");
        // Mid-song: reference 16 s (chord 5 starts) maps to at + 16/0.9.
        let m = map_time(&a.path, 16.0);
        assert!((m - (at + 16.0 / 0.9)).abs() < 1.5, "{m}");
        assert!(a.segment_cost(4.0, 28.0) < 0.2);
    }

    /// Chord sequence with uneven durations (3/4/5 s), so offsets a chord apart do not match as well.
    fn long_song(transpose: usize, from: usize, to: usize) -> Vec<f32> {
        long_song_with(transpose, from, to, [3.0, 4.0, 5.0], 3)
    }

    fn long_song_with(
        transpose: usize,
        from: usize,
        to: usize,
        durs: [f64; 3],
        stride: usize,
    ) -> Vec<f32> {
        const CHORDS: &[&[usize]] = &[
            &[0, 4, 7],
            &[9, 0, 4],
            &[5, 9, 0],
            &[7, 11, 2],
            &[2, 5, 9],
            &[4, 7, 11],
            &[0, 4, 7],
            &[5, 9, 0],
            &[7, 11, 2],
            &[9, 0, 4],
        ];
        let mut out = Vec::new();
        for i in from..to {
            let dur = durs[(i * 7 + i / 3) % 3];
            out.extend(song(
                &[CHORDS[(i * stride + i / 5) % CHORDS.len()]],
                dur,
                transpose,
                1.0,
            ));
        }
        out
    }

    #[test]
    fn locate_finds_cut_and_repeated_song() {
        // Record: 30 chords (~120 s). Live: other material, chords 0..22 up 3 semitones, chords 8..16 again, other.
        let reference = long_song(0, 0, 30);
        let mut live = song(OTHER, 5.0, 0, 1.0);
        let at = live.len() as f64 / ALIGN_RATE as f64;
        live.extend(long_song(3, 0, 22));
        live.extend(long_song(3, 8, 16));
        live.extend(song(OTHER, 5.0, 0, 1.0));
        let r = fine_chroma(&reference, ALIGN_RATE);
        let mt = fine_chroma(&live, ALIGN_RATE);
        let m = locate(&r, &mt).unwrap();
        assert_eq!(m.transpose, 3);
        assert!(m.confidence() > 0.6, "{m:?}");
        let segs = m.segments();
        let main = segs
            .iter()
            .find(|s| (s.2 - at).abs() < 0.3)
            .unwrap_or_else(|| panic!("no segment at offset {at}: {segs:?}"));
        assert!(main.0 < 1.0 && main.1 > 60.0, "{main:?}");
        assert!((m.start_s - at).abs() < 0.3, "start {} vs {at}", m.start_s);
        // A reference range inside the played part maps by the offset.
        let (a, b) = map_range(&segs, 20.0, 40.0).unwrap();
        assert!((a - (at + 20.0)).abs() < 0.3 && (b - (at + 40.0)).abs() < 0.3);
    }

    #[test]
    fn locate_has_low_confidence_for_an_absent_song() {
        // Another song from the same chord vocabulary (different order and rhythm), in two keys.
        let reference = long_song(0, 0, 30);
        let mut live = long_song_with(0, 0, 30, [2.5, 6.0, 3.5], 7);
        live.extend(long_song_with(5, 0, 30, [2.5, 6.0, 3.5], 7));
        let m = locate(&fine_chroma(&reference, ALIGN_RATE), &fine_chroma(&live, ALIGN_RATE));
        assert!(m.as_ref().is_none_or(|m| m.confidence() < 0.3), "{m:?}");
    }

    #[test]
    fn map_time_interpolates_and_extrapolates() {
        let a = [(0.0, 100.0), (10.0, 120.0), (20.0, 130.0)];
        assert_eq!(map_time(&a, 5.0), 110.0);
        assert_eq!(map_time(&a, 15.0), 125.0);
        assert_eq!(map_time(&a, 25.0), 135.0);
        assert_eq!(map_time(&a, -1.0), 99.0);
        assert_eq!(map_time(&[], 3.0), 3.0);
    }
}

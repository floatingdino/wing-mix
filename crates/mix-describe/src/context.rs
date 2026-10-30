//! Mix context: de-mastering a [`TargetProfile`] for the place the console mix is heard (M037).
//!
//! Reference profiles (and the measured `worship` genre) come from *mastered records*. A console mix is
//! pre-mastering and, at FOH, plays through a PA in a room, so chasing a record's limiting, stereo width and sub
//! balance is wrong. M034 measured exactly that on a competent human FOH mix vs the matched records: image width
//! −6..−14 dB, PLR +9..+11 dB (no mastering limiter), sub +6..+11 dB, and those were the top-ranked "problems".
//!
//! [`apply_context`] moves the *mastering-chain* dimensions of a profile towards evidence-based live norms for a
//! [`MixContext`] and widens their spread; balance, tonal shape (every band between the sub and air shelves,
//! tilt), masking and stem targets stay strict. Every edit is recorded in `TargetProfile::applied_deltas` under
//! the term `context:<name>` (diagnosis lists it in a problem's `terms`, so explanations can say "target relaxed
//! for live"). Applying a context replaces a previously applied one ([`context_of`] reads it back; the revert is
//! exact up to float rounding). Apply it after any description terms (a description is relative to the record).
//!
//! | dimension | profile path | edit ([`ContextTargets`]) |
//! |---|---|---|
//! | PLR | `loudness.plr_db` | `max(record, plr_floor_db)` |
//! | PSR | `loudness.psr_db` | p50 `max(record, psr_floor_db)`, spread `± psr_spread_db` |
//! | LRA | `loudness.lra_lu` | `+ lra_lu` (floored at 0) |
//! | width | `stereo.band_width_db[i]` (Broad6 bands >= 250 Hz) | p50 `min(record, width_ceiling_db[i])`, spread `± width_spread_db` |
//! | sub / air shelf | `tonal.tolerance_db` in [`SUB_HZ`] / [`AIR_HZ`] | `+ sub_tol_db` / `+ air_tol_db` |
//!
//! [`LIVE`] and [`BROADCAST_VIDEO`] are statistics of human mixes vs the matched records ([`derive`] over
//! [`sample`]s, one per played song section that passes the alignment sanity check; see their doc comments and
//! `crates/mix-engine/tests/live_context.rs`, which re-derives them from the local fixtures and validates them on
//! the held-out song).

use std::fmt;
use std::str::FromStr;

use mix_core::bands::{BROAD6_EDGES_HZ, N_BROAD};
use mix_core::features::Features;
use mix_core::profile::{DescriptorDelta, TargetProfile};
use mix_core::BandLayout;
use serde::{Deserialize, Serialize};

/// workflow.toml `[bands] sub` (Hz).
pub const SUB_HZ: (f32, f32) = (20.0, 60.0);
/// workflow.toml `[bands] air` (Hz).
pub const AIR_HZ: (f32, f32) = (10_000.0, 20_000.0);
/// Width deviations are taken over Broad6 bands from this edge up (diagnosis' width check, mix.rs).
pub const WIDTH_FROM_HZ: f32 = 250.0;
/// Side/mid floor below which a band counts as mono (diagnosis `width_floor_db`).
pub const WIDTH_FLOOR_DB: f32 = -40.0;
/// `applied_deltas` term prefix of context edits.
pub const TERM_PREFIX: &str = "context:";
/// `applied_deltas` target path of the marker entry every applied context records (delta 0).
pub const MARKER_PATH: &str = "context";

/// Where the console mix is heard, i.e. how far to de-master a record-derived target.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MixContext {
    /// FOH through a PA in a room (default for console mixing): no mastering limiter, near-mono image, PA sub.
    #[default]
    Live,
    /// A broadcast/stream/video mix off the console: pre-master, but heard on near-field/consumer playback.
    BroadcastVideo,
    /// Chase the record as mastered (no transform).
    MatchRecord,
}

impl MixContext {
    /// Every context, default first.
    pub const ALL: [MixContext; 3] = [
        MixContext::Live,
        MixContext::BroadcastVideo,
        MixContext::MatchRecord,
    ];

    /// Stable name (`live`, `broadcast_video`, `match_record`).
    pub fn as_str(self) -> &'static str {
        match self {
            MixContext::Live => "live",
            MixContext::BroadcastVideo => "broadcast_video",
            MixContext::MatchRecord => "match_record",
        }
    }

    /// `applied_deltas` term (`context:live`).
    pub fn term(self) -> String {
        format!("{TERM_PREFIX}{}", self.as_str())
    }

    /// What this context applies ([`ContextTargets::NONE`] for [`MixContext::MatchRecord`]).
    pub fn targets(self) -> ContextTargets {
        match self {
            MixContext::Live => LIVE,
            MixContext::BroadcastVideo => BROADCAST_VIDEO,
            MixContext::MatchRecord => ContextTargets::NONE,
        }
    }
}

impl fmt::Display for MixContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for MixContext {
    type Err = String;

    /// Accepts the stable names plus `foh`, `broadcast`, `video`, `stream`, `record`, `match`, `mastered`
    /// (case-insensitive, `-`/space for `_`).
    fn from_str(s: &str) -> Result<Self, String> {
        let k = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        Ok(match k.as_str() {
            "live" | "foh" => MixContext::Live,
            "broadcast_video" | "broadcast" | "video" | "stream" => MixContext::BroadcastVideo,
            "match_record" | "record" | "match" | "mastered" => MixContext::MatchRecord,
            _ => {
                return Err(format!(
                    "unknown mix context {s:?} (live, broadcast_video, match_record)"
                ))
            }
        })
    }
}

/// What a context does to a record-derived target. dB unless noted; `None`/0 leaves the dimension as is.
///
/// PLR, PSR and width are *live norms* (absolute values a human console mix has in that context), not offsets
/// from the record: across the fixture songs the human mixes' PLR and side/mid sit at nearly the same absolute
/// value whatever the record's mastering did (leave-one-song-out RMS error of the norm 2.5-4.8 dB vs 4.3-9.6 dB for
/// a constant offset from the record; width per band: norms −9..−12 dB on all songs, offsets −5..−13 dB). A
/// norm only ever moves a target *away from the master*: PLR/PSR targets are raised to at least the norm, width
/// targets narrowed to at most the norm, so a record that is already more dynamic or narrower is kept.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContextTargets {
    /// Live PLR norm: target `loudness.plr_db = max(record, norm)`.
    pub plr_floor_db: Option<f32>,
    /// Live PSR norm (median of the live 3 s windows): target `loudness.psr_db.p50 = max(record, norm)`.
    pub psr_floor_db: Option<f32>,
    /// Added to both sides of the PSR target spread (diagnosis tolerance is `max(2, half_spread)`).
    pub psr_spread_db: f32,
    /// Offset added to `loudness.lra_lu` (section-scope LRA, see `mix_reference::build_profile`).
    pub lra_lu: f32,
    /// Live side/mid norm per Broad6 band (only bands >= [`WIDTH_FROM_HZ`]): target p50 = `min(record, norm)`.
    pub width_ceiling_db: [Option<f32>; N_BROAD],
    /// Added to both sides of every width target spread it touches.
    pub width_spread_db: f32,
    /// Extra tonal tolerance on the sub ([`SUB_HZ`]) and air ([`AIR_HZ`]) shelves. Not re-centred: the human
    /// mixes are above the record on some songs and below on others (sub +6.7 / +5.5 / −11.2 dB, air +2.6 /
    /// −2.8 / −14.5 dB per song), so the evidence supports only a wider window.
    pub sub_tol_db: f32,
    pub air_tol_db: f32,
}

impl ContextTargets {
    pub const NONE: ContextTargets = ContextTargets {
        plr_floor_db: None,
        psr_floor_db: None,
        psr_spread_db: 0.0,
        lra_lu: 0.0,
        width_ceiling_db: [None; N_BROAD],
        width_spread_db: 0.0,
        sub_tol_db: 0.0,
        air_tol_db: 0.0,
    };
}

/// Live FOH norms, derived by [`derive`] from the engineer's FOH mix (52/53 of the fixture show) over the
/// training songs `i-know-that-i-know` and `show-me-your-face` (14 sections passing `mix_fixtures::sanity`;
/// `holy-forever` held out for validation). Measured live values per song (median): PLR 18.6 / 13.5 dB, PSR
/// 17.0 / 12.3 dB vs the records' section PLR 7-15 dB; side/mid −9..−12 dB in every band >= 250 Hz vs the
/// records' −5..+4 dB. Re-derived and checked by `crates/mix-engine/tests/live_context.rs` (M037 log has the
/// tables).
pub const LIVE: ContextTargets = ContextTargets {
    plr_floor_db: Some(16.0),
    psr_floor_db: Some(14.5),
    psr_spread_db: 2.5,
    lra_lu: 1.0,
    width_ceiling_db: [
        None,
        None,
        Some(-10.5),
        Some(-10.0),
        Some(-10.5),
        Some(-9.5),
    ],
    width_spread_db: 1.0,
    sub_tol_db: 9.5,
    air_tol_db: 11.0,
};

/// Broadcast/video norms: the same derivation over the fixture's video/stream mix (55/56), a second human
/// baseline. It is as narrow as the FOH (side/mid ≈ −10 dB) and as unlimited (PLR norm 16 dB), slightly more
/// dynamic (PSR, LRA) and further from the record on the shelves.
pub const BROADCAST_VIDEO: ContextTargets = ContextTargets {
    plr_floor_db: Some(16.0),
    psr_floor_db: Some(15.5),
    psr_spread_db: 2.5,
    lra_lu: 2.0,
    width_ceiling_db: [
        None,
        None,
        Some(-10.0),
        Some(-10.0),
        Some(-10.0),
        Some(-10.0),
    ],
    width_spread_db: 0.5,
    sub_tol_db: 11.5,
    air_tol_db: 12.0,
};

/// One (human mix section, matched record section) pair, as [`derive`] consumes it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sample {
    /// Independent unit (song id): sections of one song share a record and a mix, so each song gets one vote.
    pub group: String,
    /// Live absolute PLR (Session), PSR (median of the 3 s windows).
    pub plr_db: Option<f32>,
    pub psr_db: Option<f32>,
    /// Live minus record LRA.
    pub lra_dev_lu: Option<f32>,
    /// Live absolute side/mid per Broad6 band (floored at [`WIDTH_FLOOR_DB`]; None below [`WIDTH_FROM_HZ`]).
    pub width_db: [Option<f32>; N_BROAD],
    /// Live minus record sub / air shelf (dB, as [`Deviations`]).
    pub sub_dev_db: f32,
    pub air_dev_db: f32,
}

/// A [`Sample`] from a live Session analysis (`live_psr_db`: median PSR of its 3 s windows, if known) against
/// the matched record section's profile.
pub fn sample(
    group: &str,
    live: &Features,
    live_psr_db: Option<f32>,
    target: &TargetProfile,
) -> Sample {
    let d = deviations(live, target);
    let mut width_db = [None; N_BROAD];
    if let Some(st) = &live.stereo {
        for i in width_bands() {
            width_db[i] = Some(st.band_width_db[i].max(WIDTH_FLOOR_DB));
        }
    }
    Sample {
        group: group.to_string(),
        plr_db: live.loudness.plr_db,
        psr_db: live_psr_db,
        lra_dev_lu: d.lra_lu,
        width_db,
        sub_dev_db: d.sub_db,
        air_dev_db: d.air_db,
    }
}

/// Raw deviations (measured − target p50) of one mix against one profile on the context dimensions, plus the
/// strict ones for reporting. `None` where the measurement does not exist (mono main, non-Session window).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Deviations {
    pub plr_db: Option<f32>,
    pub lra_lu: Option<f32>,
    /// Per Broad6 band, floored at [`WIDTH_FLOOR_DB`] on both sides (None below [`WIDTH_FROM_HZ`]).
    pub width_db: [Option<f32>; N_BROAD],
    pub sub_db: f32,
    pub air_db: f32,
    /// Strict (not offset), reported only.
    pub balance_db: Option<f32>,
    pub tilt_db_per_oct: f32,
}

/// ERB40 indices whose centre lies in `[lo, hi)`.
fn erb_range((lo, hi): (f32, f32)) -> Vec<usize> {
    index_range(BandLayout::Erb40, lo, hi)
}

fn index_range(layout: BandLayout, lo: f32, hi: f32) -> Vec<usize> {
    layout
        .centres_hz()
        .iter()
        .enumerate()
        .filter(|(_, &c)| c >= lo && c < hi)
        .map(|(i, _)| i)
        .collect()
}

fn width_bands() -> impl Iterator<Item = usize> {
    (0..N_BROAD).filter(|&i| BROAD6_EDGES_HZ[i] >= WIDTH_FROM_HZ)
}

/// Mean of `cur[i] − tgt[i].p50` over the ERB bands of `range`.
fn band_dev(f: &Features, p: &TargetProfile, range: (f32, f32)) -> f32 {
    let idx = erb_range(range);
    let n = idx.len().max(1) as f32;
    idx.iter()
        .map(|&i| f.spectrum.erb_mid_rel_db[i] - p.tonal.erb_mid_rel_db[i].p50)
        .sum::<f32>()
        / n
}

/// Deviations of a measured main mix from `p`, computed like the diagnosis' mix checks.
pub fn deviations(f: &Features, p: &TargetProfile) -> Deviations {
    let mut width_db = [None; N_BROAD];
    if let Some(st) = &f.stereo {
        for i in width_bands() {
            width_db[i] = Some(
                st.band_width_db[i].max(WIDTH_FLOOR_DB)
                    - p.stereo.band_width_db[i].p50.max(WIDTH_FLOOR_DB),
            );
        }
    }
    Deviations {
        plr_db: f.loudness.plr_db.map(|v| v - p.loudness.plr_db),
        lra_lu: f.loudness.lra_lu.map(|v| v - p.loudness.lra_lu),
        width_db,
        sub_db: band_dev(f, p, SUB_HZ),
        air_db: band_dev(f, p, AIR_HZ),
        balance_db: f
            .stereo
            .as_ref()
            .map(|s| s.balance_db - p.stereo.balance_db.p50),
        tilt_db_per_oct: f.spectrum.tilt_db_per_oct - p.tonal.tilt_db_per_oct.p50,
    }
}

/// Median (NaN-free; 0 when empty).
pub fn median(v: &[f32]) -> f32 {
    let mut v: Vec<f32> = v.iter().copied().filter(|x| x.is_finite()).collect();
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Median absolute deviation around the median, scaled to a normal σ (×1.4826).
pub fn mad_sigma(v: &[f32]) -> f32 {
    let m = median(v);
    let d: Vec<f32> = v.iter().map(|x| (x - m).abs()).collect();
    1.4826 * median(&d)
}

/// Per-group (song) median and robust within-group σ of the values `get` returns.
fn per_group(samples: &[Sample], get: &dyn Fn(&Sample) -> Option<f32>) -> Vec<(f32, f32)> {
    let mut groups: Vec<&str> = samples.iter().map(|s| s.group.as_str()).collect();
    groups.sort_unstable();
    groups.dedup();
    groups
        .into_iter()
        .filter_map(|g| {
            let v: Vec<f32> = samples
                .iter()
                .filter(|s| s.group == g)
                .filter_map(get)
                .filter(|x| x.is_finite())
                .collect();
            (!v.is_empty()).then(|| (median(&v), mad_sigma(&v)))
        })
        .collect()
}

/// Mean of the group medians (each song one vote) and the RMS spread around it
/// (`sqrt(mean_g(σ_g² + (m_g − centre)²))`: within-song plus between-song). `centre` = Some(c) fixes the centre
/// (spread around the record for the relax-only shelves).
fn pooled(g: &[(f32, f32)], centre: Option<f32>) -> Option<(f32, f32)> {
    if g.is_empty() {
        return None;
    }
    let n = g.len() as f32;
    let c = centre.unwrap_or_else(|| g.iter().map(|x| x.0).sum::<f32>() / n);
    let spread = (g
        .iter()
        .map(|(m, s)| s * s + (m - c) * (m - c))
        .sum::<f32>()
        / n)
        .sqrt();
    Some((c, spread))
}

/// Context targets from human-mix samples (module doc): norms = mean over groups of the group medians; spreads
/// and shelf tolerances = pooled RMS spread ([`pooled`]); everything rounded to 0.5 dB.
pub fn derive(samples: &[Sample]) -> ContextTargets {
    let r = |x: f32| (x * 2.0).round() / 2.0;
    let norm = |get: &dyn Fn(&Sample) -> Option<f32>| pooled(&per_group(samples, get), None);
    let plr = norm(&|s| s.plr_db);
    let psr = norm(&|s| s.psr_db);
    let lra = norm(&|s| s.lra_dev_lu);
    let mut width_ceiling_db = [None; N_BROAD];
    let mut width_sq = Vec::new();
    for i in width_bands() {
        if let Some((c, sp)) = norm(&|s| s.width_db[i]) {
            width_ceiling_db[i] = Some(r(c));
            width_sq.push(sp * sp);
        }
    }
    let width_spread = if width_sq.is_empty() {
        0.0
    } else {
        (width_sq.iter().sum::<f32>() / width_sq.len() as f32).sqrt()
    };
    let relax = |get: &dyn Fn(&Sample) -> Option<f32>| {
        pooled(&per_group(samples, get), Some(0.0)).map_or(0.0, |x| r(x.1))
    };
    ContextTargets {
        plr_floor_db: plr.map(|x| r(x.0)),
        psr_floor_db: psr.map(|x| r(x.0)),
        psr_spread_db: psr.map_or(0.0, |x| r(x.1)),
        lra_lu: lra.map_or(0.0, |x| r(x.0)),
        width_ceiling_db,
        width_spread_db: r(width_spread),
        sub_tol_db: relax(&|s| Some(s.sub_dev_db)),
        air_tol_db: relax(&|s| Some(s.air_dev_db)),
    }
}

/// The context applied to `p` (from its `applied_deltas`), `None` when none was (the profile is as built, i.e.
/// match-record).
pub fn context_of(p: &TargetProfile) -> Option<MixContext> {
    p.applied_deltas.iter().find_map(|d| {
        d.term
            .strip_prefix(TERM_PREFIX)
            .and_then(|n| n.parse().ok())
    })
}

/// Applies `ctx` to `p` in place, replacing any context applied before ([`MixContext::MatchRecord`] just
/// removes it). Records a [`MARKER_PATH`] entry plus one entry per edited path; edits that change nothing
/// (record already past the norm) are not recorded.
pub fn apply_context(p: &mut TargetProfile, ctx: MixContext) {
    apply_targets(p, ctx, &ctx.targets());
}

/// [`apply_context`] with explicit targets (recorded under `ctx`'s term), e.g. freshly [`derive`]d ones.
pub fn apply_targets(p: &mut TargetProfile, ctx: MixContext, t: &ContextTargets) {
    revert(p);
    let mut rec = Recorder {
        term: ctx.term(),
        out: Vec::new(),
    };
    // Marker (no edit) so the setting itself is on record, also for match_record.
    rec.push(MARKER_PATH, 0.0);
    if let Some(f) = t.plr_floor_db {
        let d = (f - p.loudness.plr_db).max(0.0);
        if d > 0.0 {
            p.loudness.plr_db += d;
            rec.push("loudness.plr_db", d);
        }
    }
    if let Some(f) = t.psr_floor_db {
        let d = (f - p.loudness.psr_db.p50).max(0.0);
        if d > 0.0 {
            shift(&mut p.loudness.psr_db, d);
            rec.push("loudness.psr_db", d);
        }
    }
    if t.psr_spread_db > 0.0 {
        widen(&mut p.loudness.psr_db, t.psr_spread_db);
        rec.push("loudness.psr_db.spread", t.psr_spread_db);
    }
    if t.lra_lu != 0.0 {
        // Floored at 0; record the delta actually applied so the revert is exact.
        let new = (p.loudness.lra_lu + t.lra_lu).max(0.0);
        if new != p.loudness.lra_lu {
            rec.push("loudness.lra_lu", new - p.loudness.lra_lu);
            p.loudness.lra_lu = new;
        }
    }
    for i in width_bands() {
        let Some(c) = t.width_ceiling_db[i] else {
            continue;
        };
        let s = &mut p.stereo.band_width_db[i];
        let d = (c - s.p50).min(0.0);
        let path = format!("stereo.band_width_db[{i}..{}]", i + 1);
        if d < 0.0 {
            shift(s, d);
            rec.push(&path, d);
        }
        if t.width_spread_db > 0.0 {
            widen(s, t.width_spread_db);
            rec.push(&format!("{path}.spread"), t.width_spread_db);
        }
    }
    for (range, tol) in [(SUB_HZ, t.sub_tol_db), (AIR_HZ, t.air_tol_db)] {
        if tol > 0.0 {
            if let Some((a, b)) = span(&erb_range(range)) {
                p.tonal.tolerance_db[a..b]
                    .iter_mut()
                    .for_each(|x| *x += tol);
                rec.push(&format!("tonal.tolerance_db[{a}..{b}]"), tol);
            }
        }
    }
    p.applied_deltas.extend(rec.out);
}

/// `p` with `ctx` applied (see [`apply_context`]).
pub fn with_context(p: &TargetProfile, ctx: MixContext) -> TargetProfile {
    with_targets(p, ctx, &ctx.targets())
}

/// `p` with explicit targets applied (see [`apply_targets`]).
pub fn with_targets(p: &TargetProfile, ctx: MixContext, t: &ContextTargets) -> TargetProfile {
    let mut q = p.clone();
    apply_targets(&mut q, ctx, t);
    q
}

struct Recorder {
    term: String,
    out: Vec<DescriptorDelta>,
}

impl Recorder {
    fn push(&mut self, path: &str, delta: f32) {
        self.out.push(DescriptorDelta {
            term: self.term.clone(),
            target_path: path.to_string(),
            delta,
        });
    }
}

fn shift(s: &mut mix_core::features::Stat, d: f32) {
    s.p10 += d;
    s.p50 += d;
    s.p90 += d;
}

fn widen(s: &mut mix_core::features::Stat, d: f32) {
    s.p10 -= d;
    s.p90 += d;
}

fn span(idx: &[usize]) -> Option<(usize, usize)> {
    Some((*idx.first()?, *idx.last()? + 1))
}

/// Parses `name[a..b]` / `name[a..b].spread` into (name, a, b, spread?).
fn parse_path(path: &str) -> Option<(&str, usize, usize, bool)> {
    let (name, rest) = path.split_once('[')?;
    let (inner, tail) = rest.split_once(']')?;
    let (a, b) = inner.split_once("..")?;
    Some((name, a.parse().ok()?, b.parse().ok()?, tail == ".spread"))
}

/// Undoes every recorded context delta and drops it from `applied_deltas`.
fn revert(p: &mut TargetProfile) {
    let (ctx, keep): (Vec<_>, Vec<_>) = std::mem::take(&mut p.applied_deltas)
        .into_iter()
        .partition(|d| d.term.starts_with(TERM_PREFIX));
    p.applied_deltas = keep;
    for d in ctx {
        let v = -d.delta;
        match d.target_path.as_str() {
            "loudness.plr_db" => p.loudness.plr_db += v,
            "loudness.lra_lu" => p.loudness.lra_lu += v,
            "loudness.psr_db" => shift(&mut p.loudness.psr_db, v),
            "loudness.psr_db.spread" => widen(&mut p.loudness.psr_db, v),
            path => {
                let Some((name, a, b, spread)) = parse_path(path) else {
                    continue;
                };
                match name {
                    "stereo.band_width_db" if b <= N_BROAD => {
                        for s in &mut p.stereo.band_width_db[a..b] {
                            if spread {
                                widen(s, v);
                            } else {
                                shift(s, v);
                            }
                        }
                    }
                    "tonal.tolerance_db" if b <= p.tonal.tolerance_db.len() => {
                        p.tonal.tolerance_db[a..b].iter_mut().for_each(|t| *t += v)
                    }
                    _ => {}
                }
            }
        }
    }
}

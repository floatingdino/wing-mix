//! [`GenericTransfer`]: a console-neutral [`TransferModel`] for surfaces other than the WING (M061, F3).
//!
//! EQ is evaluated per band *by its shape* ([`BandKind`]: peaking, low/high shelf, low/high cut) with the standard
//! RBJ audio-EQ-cookbook biquads (Behringer's X32 EQ is a cookbook-like design; the exact coefficients are not
//! public, so this is an approximation, not a hardware-verified model). The WING-specific `eq.mix` gain-scale and
//! bus tilt are ignored; `eq.mix` is applied as a plain gain scale when the surface reports one. Low/high cuts are
//! Butterworth. Pan law, compressor, gate, de-esser and time-dependent dynamics delegate to [`WingTransfer`]
//! (static textbook-like curves; no per-console data exists yet for other consoles).

use mix_core::bands::N_ERB;
use mix_surface::model::StripKind;
use std::f64::consts::PI;

use super::state::{
    BandKind, BandSlot, CutSettings, DeEsserSettings, DynSettings, EqSettings, GateSettings,
    StripSettings,
};
use super::transfer::{TransferModel, WingTransfer};

/// Console-neutral transfer model, see the module docs.
#[derive(Clone, Debug)]
pub struct GenericTransfer {
    inner: WingTransfer,
    /// Fixed low/high-cut slope (dB/oct) when the console's cut slope is not a parameter (X32: 12). `None`: use
    /// the slope the strip reports.
    cut_slope: Option<u32>,
}

impl GenericTransfer {
    pub fn new(sample_rate: f64) -> Self {
        GenericTransfer {
            inner: WingTransfer::new(sample_rate),
            cut_slope: None,
        }
    }

    /// Cuts always have this slope (dB/oct), whatever the strip reports.
    pub fn with_cut_slope(mut self, slope: u32) -> Self {
        self.cut_slope = Some(slope);
        self
    }

    fn fs(&self) -> f64 {
        self.inner.sample_rate
    }
}

/// Transfer model for a surface (`SurfaceInfo::surface`): the WING model for `wing`, the generic one otherwise
/// (X32/M32 cuts are 12 dB/oct).
pub fn transfer_for_surface(surface: &str, sample_rate: f64) -> Box<dyn TransferModel> {
    match surface {
        "wing" => Box::new(WingTransfer::new(sample_rate)),
        "x32" | "m32" => Box::new(GenericTransfer::new(sample_rate).with_cut_slope(12)),
        _ => Box::new(GenericTransfer::new(sample_rate)),
    }
}

/// Normalised biquad (`a0 = 1`) as `(b0, b1, b2, a1, a2)`.
type Biquad = (f64, f64, f64, f64, f64);

fn norm(b: [f64; 3], a: [f64; 3]) -> Biquad {
    (
        b[0] / a[0],
        b[1] / a[0],
        b[2] / a[0],
        a[1] / a[0],
        a[2] / a[0],
    )
}

fn peaking(fs: f64, f: f64, gain_db: f64, q: f64) -> Biquad {
    let a = 10f64.powf(gain_db / 40.0);
    let w = 2.0 * PI * f / fs;
    let (s, c) = w.sin_cos();
    let al = s / (2.0 * q);
    norm(
        [1.0 + al * a, -2.0 * c, 1.0 - al * a],
        [1.0 + al / a, -2.0 * c, 1.0 - al / a],
    )
}

fn shelf(fs: f64, f: f64, gain_db: f64, q: f64, high: bool) -> Biquad {
    let a = 10f64.powf(gain_db / 40.0);
    let w = 2.0 * PI * f / fs;
    let (s, c) = w.sin_cos();
    let t = 2.0 * a.sqrt() * (s / (2.0 * q));
    if high {
        norm(
            [
                a * ((a + 1.0) + (a - 1.0) * c + t),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
                a * ((a + 1.0) + (a - 1.0) * c - t),
            ],
            [
                (a + 1.0) - (a - 1.0) * c + t,
                2.0 * ((a - 1.0) - (a + 1.0) * c),
                (a + 1.0) - (a - 1.0) * c - t,
            ],
        )
    } else {
        norm(
            [
                a * ((a + 1.0) - (a - 1.0) * c + t),
                2.0 * a * ((a - 1.0) - (a + 1.0) * c),
                a * ((a + 1.0) - (a - 1.0) * c - t),
            ],
            [
                (a + 1.0) + (a - 1.0) * c + t,
                -2.0 * ((a - 1.0) + (a + 1.0) * c),
                (a + 1.0) + (a - 1.0) * c - t,
            ],
        )
    }
}

/// `|H|^2` in dB at `f`.
fn mag_db(bq: &Biquad, fs: f64, f: f64) -> f64 {
    let w = 2.0 * PI * f / fs;
    let (c1, s1) = (w.cos(), w.sin());
    let (c2, s2) = ((2.0 * w).cos(), (2.0 * w).sin());
    let (b0, b1, b2, a1, a2) = *bq;
    let nr = b0 + b1 * c1 + b2 * c2;
    let ni = -(b1 * s1 + b2 * s2);
    let dr = 1.0 + a1 * c1 + a2 * c2;
    let di = -(a1 * s1 + a2 * s2);
    10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di))
        .max(1e-30)
        .log10()
}

/// Butterworth low-/high-pass power gain (dB) of `slope` dB/oct at `f` for cutoff `fc`.
fn butterworth_db(fc: f64, slope: u32, high_pass: bool, f: f64) -> f64 {
    let n = (slope / 6).max(1) as i32;
    let r = if high_pass { fc / f } else { f / fc };
    -10.0 * (1.0 + r.powi(2 * n)).log10()
}

impl TransferModel for GenericTransfer {
    fn eq_erb_db(&self, eq: &EqSettings, _bus: bool) -> [f64; N_ERB] {
        if !eq.on {
            return [0.0; N_ERB];
        }
        let fs = self.fs();
        let scale = (eq.mix_pct / 100.0).clamp(0.0, 1.0);
        enum Section {
            Bq(Biquad),
            Cut { fc: f64, hp: bool },
        }
        let secs: Vec<Section> = eq
            .bands
            .iter()
            .filter_map(|(slot, b)| {
                let (g, f, q) = (
                    b.gain_db * scale,
                    b.freq_hz.clamp(10.0, fs * 0.45),
                    b.q.clamp(0.1, 20.0),
                );
                let kind = match (b.kind, slot) {
                    (BandKind::Shelf, BandSlot::Low) => BandKind::LowShelf,
                    (BandKind::Shelf, BandSlot::High) => BandKind::HighShelf,
                    (k, _) => k,
                };
                match kind {
                    BandKind::Peq if g.abs() > 1e-6 => Some(Section::Bq(peaking(fs, f, g, q))),
                    BandKind::LowShelf if g.abs() > 1e-6 => {
                        Some(Section::Bq(shelf(fs, f, g, q.clamp(0.3, 2.0), false)))
                    }
                    BandKind::HighShelf if g.abs() > 1e-6 => {
                        Some(Section::Bq(shelf(fs, f, g, q.clamp(0.3, 2.0), true)))
                    }
                    BandKind::LowCut => Some(Section::Cut { fc: f, hp: true }),
                    BandKind::HighCut => Some(Section::Cut { fc: f, hp: false }),
                    _ => None,
                }
            })
            .collect();
        let slope = self.cut_slope.unwrap_or(12);
        self.inner.average(|f| {
            secs.iter()
                .map(|s| match s {
                    Section::Bq(bq) => mag_db(bq, fs, f),
                    Section::Cut { fc, hp } => butterworth_db(*fc, slope, *hp, f),
                })
                .sum()
        })
    }

    fn cut_erb_db(&self, low: Option<&CutSettings>, high: Option<&CutSettings>) -> [f64; N_ERB] {
        let lc = low.filter(|c| c.on);
        let hc = high.filter(|c| c.on);
        if lc.is_none() && hc.is_none() {
            return [0.0; N_ERB];
        }
        let slope = |c: &CutSettings| self.cut_slope.unwrap_or(c.slope);
        self.inner.average(|f| {
            lc.map_or(0.0, |c| butterworth_db(c.freq_hz, slope(c), true, f))
                + hc.map_or(0.0, |c| butterworth_db(c.freq_hz, slope(c), false, f))
        })
    }

    fn pan_gains(&self, kind: StripKind, pan: f64) -> (f64, f64) {
        self.inner.pan_gains(kind, pan)
    }
    fn comp_gain_db(&self, d: &DynSettings, level_db: f64) -> f64 {
        self.inner.comp_gain_db(d, level_db)
    }
    fn gate_gain_db(&self, g: &GateSettings, level_db: f64) -> f64 {
        self.inner.gate_gain_db(g, level_db)
    }
    fn run_dynamics(
        &self,
        s: &StripSettings,
        x: &mut [f32],
        fs: f64,
        gate_shift_db: f64,
        dyn_shift_db: f64,
    ) -> bool {
        self.inner
            .run_dynamics(s, x, fs, gate_shift_db, dyn_shift_db)
    }
    fn gate_time_from_tau(&self, tau_ms: f64) -> f64 {
        self.inner.gate_time_from_tau(tau_ms)
    }
    fn deesser_shelf_db(&self, d: &DeEsserSettings, reduction_db: f64) -> [f64; N_ERB] {
        self.inner.deesser_shelf_db(d, reduction_db)
    }
    fn deesser_reduction_db(&self, d: &DeEsserSettings, hf_ratio_db: f64) -> f64 {
        self.inner.deesser_reduction_db(d, hf_ratio_db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::state::EqBand;
    use mix_core::bands::erb40_centres_hz;

    fn eq(bands: Vec<(BandSlot, EqBand)>) -> EqSettings {
        EqSettings {
            on: true,
            mix_pct: 100.0,
            bands,
            tilt_db: None,
        }
    }
    fn band(kind: BandKind, f: f64, g: f64) -> EqBand {
        EqBand {
            gain_db: g,
            freq_hz: f,
            q: 1.0,
            kind,
        }
    }
    fn at(r: &[f64; N_ERB], hz: f64) -> f64 {
        let c = erb40_centres_hz();
        let i = (0..N_ERB)
            .min_by(|&a, &b| {
                (c[a] as f64 - hz)
                    .abs()
                    .total_cmp(&(c[b] as f64 - hz).abs())
            })
            .unwrap();
        r[i]
    }

    #[test]
    fn shape_decides_the_response() {
        let t = GenericTransfer::new(48_000.0);
        let peq = t.eq_erb_db(
            &eq(vec![(BandSlot::Peak(1), band(BandKind::Peq, 1000.0, -6.0))]),
            false,
        );
        assert!(
            at(&peq, 1000.0) < -5.0 && at(&peq, 60.0).abs() < 0.5 && at(&peq, 12000.0).abs() < 0.5
        );
        // the same band as a high shelf cuts everything above, not just around 1 kHz
        let hs = t.eq_erb_db(
            &eq(vec![(
                BandSlot::Peak(1),
                band(BandKind::HighShelf, 1000.0, -6.0),
            )]),
            false,
        );
        assert!(
            at(&hs, 12000.0) < -5.0 && at(&hs, 60.0).abs() < 0.5,
            "{} {}",
            at(&hs, 12000.0),
            at(&hs, 60.0)
        );
        let ls = t.eq_erb_db(
            &eq(vec![(
                BandSlot::Peak(2),
                band(BandKind::LowShelf, 200.0, 5.0),
            )]),
            false,
        );
        assert!(at(&ls, 50.0) > 4.0 && at(&ls, 8000.0).abs() < 0.5);
        // a low-cut band removes the lows
        let lc = t.eq_erb_db(
            &eq(vec![(
                BandSlot::Peak(1),
                band(BandKind::LowCut, 100.0, 0.0),
            )]),
            false,
        );
        assert!(at(&lc, 30.0) < -10.0 && at(&lc, 2000.0).abs() < 0.2);
        // off = flat; a 0 dB band is flat
        let mut off = eq(vec![(BandSlot::Peak(1), band(BandKind::Peq, 1000.0, -6.0))]);
        off.on = false;
        assert!(t.eq_erb_db(&off, false).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn wing_slot_shelves_follow_their_slot() {
        let t = GenericTransfer::new(48_000.0);
        let lo = t.eq_erb_db(
            &eq(vec![(BandSlot::Low, band(BandKind::Shelf, 200.0, 4.0))]),
            false,
        );
        let hi = t.eq_erb_db(
            &eq(vec![(BandSlot::High, band(BandKind::Shelf, 5000.0, 4.0))]),
            false,
        );
        assert!(at(&lo, 50.0) > 3.0 && at(&lo, 10000.0).abs() < 0.5);
        assert!(at(&hi, 12000.0) > 3.0 && at(&hi, 100.0).abs() < 0.5);
    }

    #[test]
    fn cut_slope_override() {
        let c = CutSettings {
            on: true,
            freq_hz: 100.0,
            slope: 24,
        };
        let w = GenericTransfer::new(48_000.0);
        let x = GenericTransfer::new(48_000.0).with_cut_slope(12);
        assert!(
            at(&w.cut_erb_db(Some(&c), None), 50.0) < at(&x.cut_erb_db(Some(&c), None), 50.0) - 3.0
        );
    }

    #[test]
    fn factory_picks_by_surface() {
        // WING keeps its own designs: its PEQ differs from the cookbook one near the band edge only slightly, but
        // the factory must at least return a working model for every name.
        for n in ["wing", "x32", "m32", "other"] {
            let t = transfer_for_surface(n, 48_000.0);
            assert!(t
                .eq_erb_db(
                    &eq(vec![(BandSlot::Peak(1), band(BandKind::Peq, 1000.0, -6.0))]),
                    false
                )
                .iter()
                .any(|&x| x < -4.0));
        }
    }
}

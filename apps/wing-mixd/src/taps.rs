//! Which console taps the input carries, and where their channels sit in the capture ring.

use mix_core::ids::StripId;
use serde::Serialize;

use crate::protocol::TapSel;

/// A console signal the analysis sees.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Tap {
    /// Main mix (required; what the target is compared with).
    Main,
    /// Solo bus (inspections measure the soloed strip here).
    Solo,
    /// A strip post fader (per-strip diagnosis).
    Strip { strip: StripId },
    /// A monitor feed (feedback guard only).
    Monitor { name: String },
}

impl Tap {
    pub fn parse(s: &str) -> Result<Tap, String> {
        let s = s.trim();
        match s {
            "main" => Ok(Tap::Main),
            "solo" => Ok(Tap::Solo),
            _ => {
                if let Some(n) = s.strip_prefix("monitor:") {
                    if n.is_empty() {
                        return Err("monitor tap needs a name (monitor:IEM 1)".into());
                    }
                    return Ok(Tap::Monitor { name: n.into() });
                }
                let id = StripId::parse(s).ok_or_else(|| {
                    format!("unknown tap `{s}` (main, solo, input.N, monitor:NAME)")
                })?;
                Ok(Tap::Strip { strip: id })
            }
        }
    }

    pub fn label(&self) -> String {
        match self {
            Tap::Main => "main".into(),
            Tap::Solo => "solo".into(),
            Tap::Strip { strip } => strip.as_str().to_string(),
            Tap::Monitor { name } => format!("monitor:{name}"),
        }
    }
}

/// Layout of the capture ring: each tap's channel offsets within one interleaved frame.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TapLayout {
    pub taps: Vec<(Tap, Vec<usize>)>,
    /// Channels per ring frame.
    pub ring_channels: usize,
    /// 0-based source channels in ring order (the capture selection); empty for the sim.
    #[serde(skip)]
    pub selection: Vec<usize>,
}

impl TapLayout {
    /// From user tap selections (1-based source channels). Exactly one `main`; at most one `solo`; 1-2 channels
    /// each; a channel may be shared by taps.
    pub fn from_selection(sel: &[TapSel]) -> Result<TapLayout, String> {
        let mut selection: Vec<usize> = Vec::new();
        let mut taps: Vec<(Tap, Vec<usize>)> = Vec::new();
        for t in sel {
            let tap = Tap::parse(&t.tap)?;
            if taps.iter().any(|(x, _)| *x == tap) {
                return Err(format!("tap {} given twice", tap.label()));
            }
            if t.channels.is_empty() || t.channels.len() > 2 {
                return Err(format!("tap {}: 1 or 2 channels", tap.label()));
            }
            let mut offs = Vec::new();
            for &c in &t.channels {
                if c == 0 {
                    return Err("channels are 1-based".into());
                }
                let idx = c - 1;
                let off = match selection.iter().position(|&x| x == idx) {
                    Some(p) => p,
                    None => {
                        selection.push(idx);
                        selection.len() - 1
                    }
                };
                offs.push(off);
            }
            taps.push((tap, offs));
        }
        let mains = taps.iter().filter(|(t, _)| *t == Tap::Main).count();
        if mains != 1 {
            return Err("exactly one `main` tap is required".into());
        }
        Ok(TapLayout {
            ring_channels: selection.len(),
            taps,
            selection,
        })
    }

    /// Stereo taps in order, two ring channels each (the sim renderer's layout).
    pub fn stereo(taps: Vec<Tap>) -> TapLayout {
        let taps: Vec<(Tap, Vec<usize>)> = taps
            .into_iter()
            .enumerate()
            .map(|(i, t)| (t, vec![2 * i, 2 * i + 1]))
            .collect();
        TapLayout {
            ring_channels: 2 * taps.len(),
            taps,
            selection: Vec::new(),
        }
    }

    pub fn has(&self, tap: &Tap) -> bool {
        self.taps.iter().any(|(t, _)| t == tap)
    }

    pub fn labels(&self) -> Vec<String> {
        self.taps.iter().map(|(t, _)| t.label()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(tap: &str, ch: &[usize]) -> TapSel {
        TapSel {
            tap: tap.into(),
            channels: ch.to_vec(),
        }
    }

    #[test]
    fn layout_from_selection() {
        let l = TapLayout::from_selection(&[
            sel("main", &[1, 2]),
            sel("solo", &[5, 6]),
            sel("input.3", &[9]),
            sel("monitor:IEM 1", &[2]),
        ])
        .unwrap();
        assert_eq!(l.selection, vec![0, 1, 4, 5, 8]);
        assert_eq!(l.ring_channels, 5);
        assert_eq!(l.taps[2].1, vec![4]);
        assert_eq!(l.taps[3].1, vec![1]);
        assert_eq!(
            l.taps[2].0,
            Tap::Strip {
                strip: StripId::new("input", 3)
            }
        );
        assert!(TapLayout::from_selection(&[sel("solo", &[1, 2])]).is_err());
        assert!(TapLayout::from_selection(&[sel("main", &[1, 2, 3])]).is_err());
        assert!(TapLayout::from_selection(&[sel("main", &[0])]).is_err());
        assert!(TapLayout::from_selection(&[sel("main", &[1]), sel("main", &[2])]).is_err());
        assert!(TapLayout::from_selection(&[sel("main", &[1]), sel("bogus", &[2])]).is_err());
        let s = TapLayout::stereo(vec![Tap::Main, Tap::Solo]);
        assert_eq!(s.ring_channels, 4);
        assert_eq!(s.taps[1].1, vec![2, 3]);
    }
}

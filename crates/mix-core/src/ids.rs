//! Stable, surface-independent identifiers for strips and parameters.
//!
//! Grammar (docs/research/remit.md section 2.2): a [`ParamId`] is `<strip>/<block>[/<sub>]/<leaf>` where `<strip>`
//! is a [`StripId`] `<kind>.<n>` (`input.3`, `bus.8`, `fx.4`), or a non-strip domain (`system/clock/source`,
//! `mutegroup.1/mute`, `source.local.3/phantom`, `output.local.1/source`). Segments are separated by `/`. Adapters
//! translate their native addresses into these ids; the engine never sees surface paths.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Identifier of a strip: `<kind>.<n>` (e.g. `input.3`, `bus.12`, `main.1`, `fx.5`).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StripId(String);

impl StripId {
    /// Build from a kind name and a 1-based index.
    pub fn new(kind: &str, index: u32) -> Self {
        StripId(format!("{kind}.{index}"))
    }

    /// Wrap an existing `<kind>.<n>` string without validation.
    pub fn from_raw(s: impl Into<String>) -> Self {
        StripId(s.into())
    }

    /// Parse and validate `<kind>.<n>`: a non-empty lowercase/underscore kind and a decimal index.
    pub fn parse(s: &str) -> Option<Self> {
        let (kind, idx) = s.split_once('.')?;
        let kind_ok = !kind.is_empty() && kind.chars().all(|c| c.is_ascii_lowercase() || c == '_');
        let idx_ok = !idx.is_empty() && idx.chars().all(|c| c.is_ascii_digit());
        (kind_ok && idx_ok).then(|| StripId(s.to_string()))
    }

    /// The kind segment (`input` in `input.3`).
    pub fn kind_name(&self) -> &str {
        self.0.split_once('.').map(|(k, _)| k).unwrap_or(&self.0)
    }

    /// The 1-based index (`3` in `input.3`).
    pub fn index(&self) -> Option<u32> {
        self.0.split_once('.').and_then(|(_, n)| n.parse().ok())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for StripId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Identifier of a parameter: `/`-separated generic path (see module docs).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamId(String);

impl ParamId {
    pub fn new(s: impl Into<String>) -> Self {
        ParamId(s.into())
    }

    /// `<strip>/<rest>`.
    pub fn of(strip: &StripId, rest: &str) -> Self {
        ParamId(format!("{strip}/{rest}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `/`-separated segments.
    pub fn segments(&self) -> impl Iterator<Item = &str> {
        self.0.split('/')
    }

    /// First segment (strip id or domain).
    pub fn head(&self) -> &str {
        self.0.split('/').next().unwrap_or("")
    }

    /// Owning strip when the first segment is a well-formed `<kind>.<n>`. Domain ids such as `system/...` return
    /// `None`; `mutegroup.1`/`source.local.3` parse as strip-shaped heads only when they match the grammar, so callers
    /// must still check the kind against the surface model.
    pub fn strip(&self) -> Option<StripId> {
        StripId::parse(self.head())
    }

    /// Second segment: the processing block (`eq`, `in`, `send`, `insert.1`, `fader`, `model`, `param.decay`...).
    pub fn block(&self) -> Option<&str> {
        self.0.split('/').nth(1)
    }

    /// Last segment (`gain` in `input.3/in/gain`, `band.2.gain` in `input.3/eq/band.2.gain`).
    pub fn leaf(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or("")
    }

    /// Destination of a send param (`input.1/send/bus.8/level` -> `bus.8`).
    pub fn send_dest(&self) -> Option<StripId> {
        let mut it = self.0.split('/');
        it.next()?;
        if it.next()? != "send" {
            return None;
        }
        StripId::parse(it.next()?)
    }

    /// True if this is a send param (`<strip>/send/<dest>/...`).
    pub fn is_send(&self) -> bool {
        self.block() == Some("send")
    }
}

impl fmt::Display for ParamId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for ParamId {
    fn from(s: &str) -> Self {
        ParamId(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_id_parse() {
        let s = StripId::parse("input.3").unwrap();
        assert_eq!(s.kind_name(), "input");
        assert_eq!(s.index(), Some(3));
        assert_eq!(StripId::new("bus", 12).as_str(), "bus.12");
        assert!(StripId::parse("system").is_none());
        assert!(StripId::parse("source.local.3").is_none());
        assert!(StripId::parse("Input.3").is_none());
        assert!(StripId::parse("input.").is_none());
    }

    #[test]
    fn param_id_parts() {
        let p = ParamId::new("input.1/send/bus.8/level");
        assert_eq!(p.strip().unwrap().as_str(), "input.1");
        assert_eq!(p.block(), Some("send"));
        assert_eq!(p.leaf(), "level");
        assert_eq!(p.send_dest().unwrap().as_str(), "bus.8");
        assert!(p.is_send());

        let q = ParamId::new("input.3/eq/band.2.gain");
        assert_eq!(q.block(), Some("eq"));
        assert_eq!(q.leaf(), "band.2.gain");
        assert!(q.send_dest().is_none());

        let sys = ParamId::new("system/clock/source");
        assert!(sys.strip().is_none());
        assert_eq!(
            ParamId::of(&StripId::new("main", 1), "fader").as_str(),
            "main.1/fader"
        );
    }

    #[test]
    fn serde_transparent() {
        let p = ParamId::new("bus.1/fader");
        assert_eq!(serde_json::to_string(&p).unwrap(), "\"bus.1/fader\"");
    }
}

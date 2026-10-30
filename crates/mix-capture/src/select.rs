//! Channel selection (user-facing 1-based text such as `1-2` or `1,3,5`) and device-name matching.

use std::fmt;

/// Ordered list of input channels (stored 0-based) that become the ring's channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelSelection(Vec<usize>);

#[derive(Debug, PartialEq, Eq)]
pub struct SelectError(pub String);
impl fmt::Display for SelectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for SelectError {}

impl ChannelSelection {
    /// Stereo pair starting at 0-based channel `first` (`pair(0)` = channels 1-2).
    pub fn pair(first: usize) -> Self {
        Self(vec![first, first + 1])
    }
    /// From 0-based indices (must be non-empty).
    pub fn from_indices(idx: Vec<usize>) -> Result<Self, SelectError> {
        if idx.is_empty() {
            return Err(SelectError("empty channel selection".into()));
        }
        Ok(Self(idx))
    }
    /// Parse 1-based text: `"3"`, `"1-2"`, `"1,2,5-6"`.
    pub fn parse(s: &str) -> Result<Self, SelectError> {
        let bad = || SelectError(format!("bad channel list '{s}' (use e.g. 1-2 or 1,3,5)"));
        let mut v = Vec::new();
        for part in s.split(',') {
            let part = part.trim();
            let num = |t: &str| t.trim().parse::<usize>().ok().filter(|n| *n >= 1);
            if let Some((a, b)) = part.split_once('-') {
                let (a, b) = (num(a).ok_or_else(bad)?, num(b).ok_or_else(bad)?);
                if b < a {
                    return Err(bad());
                }
                v.extend((a..=b).map(|c| c - 1));
            } else {
                v.push(num(part).ok_or_else(bad)? - 1);
            }
        }
        Self::from_indices(v)
    }
    pub fn indices(&self) -> &[usize] {
        &self.0
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    /// Smallest channel count a device must offer.
    pub fn min_device_channels(&self) -> usize {
        self.0.iter().max().map_or(0, |m| m + 1)
    }
}

impl Default for ChannelSelection {
    fn default() -> Self {
        Self::pair(0)
    }
}

/// Pick one name from `names` for `query`: exact match, else unique case-insensitive substring.
/// Returns the index. (cpal exposes no stable device UID, so the device name is the identifier.)
pub fn match_name(names: &[String], query: &str) -> Result<usize, SelectError> {
    if let Some(i) = names.iter().position(|n| n == query) {
        return Ok(i);
    }
    let q = query.to_lowercase();
    let hits: Vec<usize> = (0..names.len())
        .filter(|&i| names[i].to_lowercase().contains(&q))
        .collect();
    match hits.len() {
        0 => Err(SelectError(format!("no input device matches '{query}'"))),
        1 => Ok(hits[0]),
        _ => Err(SelectError(format!(
            "'{query}' is ambiguous: {}",
            hits.iter()
                .map(|&i| names[i].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lists() {
        assert_eq!(ChannelSelection::parse("1-2").unwrap().indices(), &[0, 1]);
        assert_eq!(
            ChannelSelection::parse("1, 3,5-6").unwrap().indices(),
            &[0, 2, 4, 5]
        );
        assert_eq!(
            ChannelSelection::parse("48").unwrap().min_device_channels(),
            48
        );
        for bad in ["", "0", "a", "3-1", "1,,2"] {
            assert!(ChannelSelection::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn name_matching() {
        let n = vec![
            "WING USB".to_string(),
            "Wing Simulator".to_string(),
            "BlackHole 2ch".to_string(),
        ];
        assert_eq!(match_name(&n, "Wing Simulator").unwrap(), 1);
        assert_eq!(match_name(&n, "black").unwrap(), 2);
        assert_eq!(match_name(&n, "wing usb").unwrap(), 0);
        assert!(match_name(&n, "wing").is_err()); // ambiguous
        assert!(match_name(&n, "zzz").is_err());
    }
}

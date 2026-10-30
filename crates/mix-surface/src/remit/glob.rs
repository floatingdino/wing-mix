//! ParamId globs: `*` matches within one segment, `**` matches any number of segments (remit.md 2.2).

/// True if `path` matches `pattern`.
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let p: Vec<&str> = pattern.split('/').collect();
    let s: Vec<&str> = path.split('/').collect();
    match_segs(&p, &s)
}

fn match_segs(p: &[&str], s: &[&str]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(&"**") => (0..=s.len()).any(|i| match_segs(&p[1..], &s[i..])),
        Some(seg) => {
            !s.is_empty()
                && match_seg(seg.as_bytes(), s[0].as_bytes())
                && match_segs(&p[1..], &s[1..])
        }
    }
}

/// Wildcard match within one segment (`*` = any run of characters).
fn match_seg(p: &[u8], s: &[u8]) -> bool {
    match p.first() {
        None => s.is_empty(),
        Some(b'*') => (0..=s.len()).any(|i| match_seg(&p[1..], &s[i..])),
        Some(c) => s.first() == Some(c) && match_seg(&p[1..], &s[1..]),
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn globs() {
        assert!(glob_match("input.*/in/delay", "input.9/in/delay"));
        assert!(!glob_match("input.*/in/delay", "input.9/in/delay/on"));
        assert!(glob_match("bus.8/**", "bus.8/eq/band.1.gain"));
        assert!(glob_match("bus.8/**", "bus.8"));
        assert!(!glob_match("bus.8/**", "bus.80/fader"));
        assert!(glob_match("input.*/eq/**", "input.3/eq/band.2.gain"));
        assert!(glob_match("**/fader", "dca.1/fader"));
        assert!(!glob_match("*/fader", "input.1/send/main.1/fader/x"));
        assert!(glob_match(
            "input.1/eq/band.*.gain",
            "input.1/eq/band.2.gain"
        ));
        assert!(glob_match("input.1/fader", "input.1/fader"));
    }
}

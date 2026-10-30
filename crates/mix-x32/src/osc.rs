//! Minimal OSC 1.0 codec (stdlib only): messages with int32, float32, string and blob arguments.
//!
//! The X32 speaks plain OSC over UDP (no bundles for the messages this adapter uses). Old-style messages without a
//! type-tag string are accepted when decoding (they carry no arguments here).

/// One OSC argument.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Int(i32),
    Float(f32),
    Str(String),
    Blob(Vec<u8>),
}

/// An OSC message.
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub addr: String,
    pub args: Vec<Arg>,
}

impl Message {
    pub fn new(addr: impl Into<String>, args: Vec<Arg>) -> Self {
        Message {
            addr: addr.into(),
            args,
        }
    }

    /// A query: the console answers with the current value at the same address.
    pub fn query(addr: impl Into<String>) -> Self {
        Message::new(addr, Vec::new())
    }
}

fn pad(out: &mut Vec<u8>) {
    out.push(0);
    while !out.len().is_multiple_of(4) {
        out.push(0);
    }
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(s.as_bytes());
    pad(out);
}

/// Encode a message.
pub fn encode(m: &Message) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, &m.addr);
    let mut tags = String::from(",");
    for a in &m.args {
        tags.push(match a {
            Arg::Int(_) => 'i',
            Arg::Float(_) => 'f',
            Arg::Str(_) => 's',
            Arg::Blob(_) => 'b',
        });
    }
    put_str(&mut out, &tags);
    for a in &m.args {
        match a {
            Arg::Int(i) => out.extend_from_slice(&i.to_be_bytes()),
            Arg::Float(f) => out.extend_from_slice(&f.to_be_bytes()),
            Arg::Str(s) => put_str(&mut out, s),
            Arg::Blob(b) => {
                out.extend_from_slice(&(b.len() as i32).to_be_bytes());
                out.extend_from_slice(b);
                while !out.len().is_multiple_of(4) {
                    out.push(0);
                }
            }
        }
    }
    out
}

fn get_str(buf: &[u8], pos: &mut usize) -> Option<String> {
    let rest = buf.get(*pos..)?;
    let end = rest.iter().position(|&b| b == 0)?;
    let s = std::str::from_utf8(&rest[..end]).ok()?.to_string();
    *pos += (end + 1).div_ceil(4) * 4;
    Some(s)
}

fn get4(buf: &[u8], pos: &mut usize) -> Option<[u8; 4]> {
    let b: [u8; 4] = buf.get(*pos..*pos + 4)?.try_into().ok()?;
    *pos += 4;
    Some(b)
}

/// Decode a datagram; `None` if malformed or not a message (e.g. a bundle).
pub fn decode(buf: &[u8]) -> Option<Message> {
    let mut pos = 0;
    let addr = get_str(buf, &mut pos)?;
    if !addr.starts_with('/') {
        return None;
    }
    if pos >= buf.len() {
        return Some(Message::new(addr, Vec::new()));
    }
    let tags = get_str(buf, &mut pos)?;
    let tags = tags.strip_prefix(',')?;
    let mut args = Vec::new();
    for t in tags.chars() {
        args.push(match t {
            'i' => Arg::Int(i32::from_be_bytes(get4(buf, &mut pos)?)),
            'f' => Arg::Float(f32::from_be_bytes(get4(buf, &mut pos)?)),
            's' => Arg::Str(get_str(buf, &mut pos)?),
            'b' => {
                let n = i32::from_be_bytes(get4(buf, &mut pos)?);
                let n = usize::try_from(n).ok()?;
                let data = buf.get(pos..pos.checked_add(n)?)?.to_vec();
                pos += n.div_ceil(4) * 4;
                Arg::Blob(data)
            }
            _ => return None,
        });
    }
    Some(Message::new(addr, args))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let m = Message::new(
            "/ch/01/mix/fader",
            vec![
                Arg::Float(0.75),
                Arg::Int(-3),
                Arg::Str("Kick".into()),
                Arg::Blob(vec![1, 2, 3]),
            ],
        );
        let b = encode(&m);
        assert_eq!(b.len() % 4, 0);
        assert_eq!(decode(&b), Some(m));
    }

    #[test]
    fn known_bytes() {
        // `/xremote` with no arguments: address padded to 12 bytes, then an empty type-tag string.
        let b = encode(&Message::query("/xremote"));
        assert_eq!(b, b"/xremote\0\0\0\0,\0\0\0");
        // `/ch/01/mix/on ,i 1`
        let b = encode(&Message::new("/ch/01/mix/on", vec![Arg::Int(1)]));
        assert_eq!(b, b"/ch/01/mix/on\0\0\0,i\0\0\0\0\0\x01");
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(decode(b"nope\0\0\0\0"), None);
        assert_eq!(decode(b"/a\0\0,i\0\0\0\0"), None);
        assert_eq!(decode(b"/a\0\0,x\0\0"), None);
    }

    #[test]
    fn old_style_without_tags() {
        assert_eq!(decode(b"/info\0\0\0"), Some(Message::query("/info")));
    }
}

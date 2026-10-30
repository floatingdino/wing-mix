//! Minimal REAPER project (`.RPP`) parser: a generic tree of `<TAG args ... >` blocks plus a typed view of tracks
//! and items (names, file paths, position/offset/length, volume/pan/mute/solo, folder structure).
//!
//! Only what the fixture loader needs is interpreted; everything else (FX chains, base64 state) is ignored.

use std::fmt;

#[derive(Debug)]
pub struct RppError(pub String);
impl fmt::Display for RppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rpp: {}", self.0)
    }
}
impl std::error::Error for RppError {}

/// One `<TAG args` block: its own header tokens, `KEY args` lines and child blocks.
#[derive(Debug, Default, Clone)]
pub struct Node {
    pub tag: String,
    pub args: Vec<String>,
    pub lines: Vec<Vec<String>>,
    pub children: Vec<Node>,
}

impl Node {
    /// Arguments of the first line whose key is `key`.
    pub fn line(&self, key: &str) -> Option<&[String]> {
        self.lines
            .iter()
            .find(|l| l.first().map(String::as_str) == Some(key))
            .map(|l| &l[1..])
    }
    fn num(&self, key: &str, idx: usize) -> Option<f64> {
        self.line(key)?.get(idx)?.parse().ok()
    }
    fn text(&self, key: &str) -> Option<String> {
        self.line(key)?.first().cloned()
    }
}

/// Split a line into tokens honouring `"…"`, `'…'` and backtick quoting.
fn tokenize(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = line.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if matches!(c, '"' | '\'' | '`') {
            chars.next();
            let mut s = String::new();
            for d in chars.by_ref() {
                if d == c {
                    break;
                }
                s.push(d);
            }
            out.push(s);
        } else {
            let mut s = String::new();
            while let Some(&d) = chars.peek() {
                if d.is_whitespace() {
                    break;
                }
                s.push(d);
                chars.next();
            }
            out.push(s);
        }
    }
    out
}

/// Parse the generic block tree; returns the root (`REAPER_PROJECT`) node.
pub fn parse_tree(text: &str) -> Result<Node, RppError> {
    let mut stack: Vec<Node> = Vec::new();
    let mut root = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(head) = line.strip_prefix('<') {
            let mut toks = tokenize(head);
            if toks.is_empty() {
                return Err(RppError("empty block header".into()));
            }
            let tag = toks.remove(0);
            stack.push(Node {
                tag,
                args: toks,
                ..Node::default()
            });
        } else if line == ">" {
            let done = stack
                .pop()
                .ok_or_else(|| RppError("unbalanced '>'".into()))?;
            match stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => root = Some(done),
            }
        } else if let Some(top) = stack.last_mut() {
            top.lines.push(tokenize(line));
        }
    }
    if !stack.is_empty() {
        return Err(RppError("unterminated block".into()));
    }
    root.ok_or_else(|| RppError("no root block".into()))
}

#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    /// Timeline position, seconds.
    pub position: f64,
    pub length: f64,
    /// Offset into the source media, seconds (`SOFFS`).
    pub source_offset: f64,
    pub name: String,
    /// Item gain (linear).
    pub volume: f64,
    pub pan: f64,
    pub mute: bool,
    pub play_rate: f64,
    /// Source file path as written in the project (usually relative to the project directory).
    pub file: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub name: String,
    /// Track fader (linear) and pan.
    pub volume: f64,
    pub pan: f64,
    pub mute: bool,
    pub solo: bool,
    /// Folder (bus) parent track, `ISBUS 1 1`.
    pub is_folder: bool,
    /// Index of the enclosing folder track, if any.
    pub parent: Option<usize>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Project {
    /// Project sample rate (`SAMPLERATE`); media files may differ (they are authoritative for reading).
    pub sample_rate: u32,
    pub tracks: Vec<Track>,
}

fn item_from(n: &Node) -> Item {
    Item {
        position: n.num("POSITION", 0).unwrap_or(0.0),
        length: n.num("LENGTH", 0).unwrap_or(0.0),
        source_offset: n.num("SOFFS", 0).unwrap_or(0.0),
        name: n.text("NAME").unwrap_or_default(),
        volume: n.num("VOLPAN", 0).unwrap_or(1.0),
        pan: n.num("VOLPAN", 1).unwrap_or(0.0),
        mute: n.num("MUTE", 0).unwrap_or(0.0) != 0.0,
        play_rate: n.num("PLAYRATE", 0).unwrap_or(1.0),
        file: n
            .children
            .iter()
            .find(|c| c.tag == "SOURCE")
            .and_then(|s| s.text("FILE")),
    }
}

/// Parse a project into the typed view.
pub fn parse(text: &str) -> Result<Project, RppError> {
    let root = parse_tree(text)?;
    if root.tag != "REAPER_PROJECT" {
        return Err(RppError(format!("unexpected root '{}'", root.tag)));
    }
    let sample_rate = root.num("SAMPLERATE", 0).unwrap_or(44100.0) as u32;
    let mut tracks = Vec::new();
    // Folder nesting: ISBUS's second value is the depth delta applied after this track.
    let mut open: Vec<usize> = Vec::new();
    for n in root.children.iter().filter(|c| c.tag == "TRACK") {
        let isbus = n.line("ISBUS");
        let (is_folder, delta) = match isbus {
            Some(a) => {
                let f: i32 = a.first().and_then(|v| v.parse().ok()).unwrap_or(0);
                let d: i32 = a.get(1).and_then(|v| v.parse().ok()).unwrap_or(0);
                (f == 1, d)
            }
            None => (false, 0),
        };
        let idx = tracks.len();
        let parent = open.last().copied();
        let ms = n.line("MUTESOLO");
        let flag = |i: usize| {
            ms.and_then(|a| a.get(i))
                .is_some_and(|v| v.parse::<i32>().unwrap_or(0) != 0)
        };
        tracks.push(Track {
            name: n.text("NAME").unwrap_or_default(),
            volume: n.num("VOLPAN", 0).unwrap_or(1.0),
            pan: n.num("VOLPAN", 1).unwrap_or(0.0),
            mute: flag(0),
            solo: flag(1),
            is_folder,
            parent,
            items: n
                .children
                .iter()
                .filter(|c| c.tag == "ITEM")
                .map(item_from)
                .collect(),
        });
        if delta == 1 {
            open.push(idx);
        } else if delta < 0 {
            for _ in 0..(-delta) {
                open.pop();
            }
        }
    }
    Ok(Project {
        sample_rate,
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    pub const SAMPLE: &str = r#"<REAPER_PROJECT 0.1 "7.34/macOS-arm64" 1793362641
  SAMPLERATE 44100 0 0
  <TRACK {AAA}
    NAME Drums
    VOLPAN 0.5 0.25 -1 -1 1
    MUTESOLO 0 0 0
    ISBUS 1 1
    <FXCHAIN
      <VST "VST: ReaEQ (Cockos)" reaeq.vst.dylib 0 ""
        AAAA
      >
    >
  >
  <TRACK {BBB}
    NAME "Tom 1"
    VOLPAN 1 0 -1 -1 1
    MUTESOLO 1 0 0
    ISBUS 0 0
    <ITEM
      POSITION 1.5
      LENGTH 10.25
      SOFFS 2
      MUTE 0 0
      NAME "06-Tom 1-x.wav"
      VOLPAN 2 0 1 -1
      PLAYRATE 1 1 0 -1 0 0.0025
      <SOURCE WAVE
        FILE "Media/06-Tom 1-x.wav"
      >
    >
  >
  <TRACK {CCC}
    NAME Kick
    ISBUS 2 -1
    <ITEM
      POSITION 0
      LENGTH 3
      <SOURCE WAVE
        FILE "Media/03-Kick-x.wav"
      >
    >
  >
  <TRACK {DDD}
    NAME Master-ish
  >
>
"#;

    #[test]
    fn parses_tracks_items_and_folders() {
        let p = parse(SAMPLE).unwrap();
        assert_eq!(p.sample_rate, 44100);
        assert_eq!(p.tracks.len(), 4);
        let drums = &p.tracks[0];
        assert!(drums.is_folder && drums.parent.is_none());
        assert_eq!((drums.volume, drums.pan), (0.5, 0.25));
        assert!(drums.items.is_empty());
        let tom = &p.tracks[1];
        assert_eq!(tom.name, "Tom 1");
        assert_eq!(tom.parent, Some(0));
        assert!(tom.mute && !tom.solo);
        let it = &tom.items[0];
        assert_eq!(
            (it.position, it.length, it.source_offset, it.volume),
            (1.5, 10.25, 2.0, 2.0)
        );
        assert_eq!(it.file.as_deref(), Some("Media/06-Tom 1-x.wav"));
        assert_eq!(it.name, "06-Tom 1-x.wav");
        // folder closes after the track with ISBUS 2 -1
        assert_eq!(p.tracks[2].parent, Some(0));
        assert_eq!(p.tracks[3].parent, None);
        assert_eq!(p.tracks[2].items[0].volume, 1.0);
    }

    #[test]
    fn tokenizer_handles_quotes() {
        assert_eq!(
            tokenize(r#"FILE "a b/c.wav" 3 `x y`"#),
            vec!["FILE", "a b/c.wav", "3", "x y"]
        );
    }

    #[test]
    fn rejects_unbalanced() {
        assert!(parse_tree("<A\n<B\n>\n").is_err());
        assert!(parse("<OTHER\n>\n").is_err());
    }
}

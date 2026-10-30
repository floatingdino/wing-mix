//! Generic [`Topology`] from the WING tree (mirror store): what role detection (remit.md section 6) needs.
//!
//! Per strip: name, user tags (DCA `#Dn` / mute-group `#Mn` membership removed), chain (input: `in, filter,
//! insert.1, <proc order: gate/pre_eq+eq/dyn/insert.2>, fader, pan`; aux: `in, insert.1, eq, dyn, fader, pan`;
//! bus/main/matrix: `in, insert.1, eq, dyn, insert.2, fader, pan`), sends (bus/main/matrix with on, level, tap and
//! tap point), inserts, physical/computer/recorder output patches (`io/out` legs), re-inputs (`in/conn` from
//! BUS/MAIN/MTX), talkback source (`cfg/talk/assign`) and destinations (`cfg/talk/{A,B}/Bn|MXn|Mn`) and FX slot
//! models. Signal-flow facts and assumptions: wing-simulator docs/research/signal-flow.md, remit.md A.3.

use crate::map::STRIP_DOMAINS;
use mix_core::ids::StripId;
use mix_surface::model::{
    Block, FxCategory, FxSlotInfo, InsertInfo, OutputKind, SendInfo, SendTap, Strip, StripKind,
    Topology,
};
use std::collections::{BTreeMap, BTreeSet};
use wing_model::{ParamStore, Schema};

/// Time-based FX models (remit.md A.3) -> `FxCategory::Time`.
pub const TIME_FX_MODELS: &[&str] = &[
    "HALL", "ROOM", "CHAMBER", "PLATE", "CONCERT", "AMBI", "V-ROOM", "V-REV", "V-PLATE", "GATED",
    "REVERSE", "DEL/REV", "SHIMMER", "SPRING", "ST-DL", "TAP-DL", "TAPE-DL", "OILCAN", "BBD-DL",
    "VSS3", "BPLATE", "DIMCRS", "CHORUS", "FLANGER", "PITCH", "D-PITCH", "DOUBLE",
];

/// Thin read helpers over the mirror.
pub struct Reader<'a>(pub &'a ParamStore);

impl Reader<'_> {
    pub fn token(&self, path: &str) -> Option<String> {
        let id = self.0.lookup(path)?;
        let mut s = String::new();
        self.0.format_token(id, &mut s).ok()?;
        Some(s)
    }
    pub fn num(&self, path: &str) -> Option<f64> {
        self.0.get_f64(self.0.lookup(path)?)
    }
    pub fn on(&self, path: &str) -> bool {
        self.num(path).is_some_and(|v| v != 0.0)
    }
    pub fn text(&self, path: &str) -> String {
        self.0
            .lookup(path)
            .and_then(|id| self.0.with_str(id, |s| s.to_string()))
            .unwrap_or_default()
    }
}

/// User tags of a WING tag string (`"#D2,#M4,NOAI"` -> `["NOAI"]`).
pub fn user_tags(tags: &str) -> Vec<String> {
    tags.split([',', ' '])
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .filter(|t| {
            let b = t.as_bytes();
            !(b.len() > 2
                && b[0] == b'#'
                && matches!(b[1], b'D' | b'M' | b'd' | b'm')
                && b[2..].iter().all(u8::is_ascii_digit))
        })
        .map(|t| t.to_string())
        .collect()
}

fn proc_blocks(order: &str) -> Vec<Block> {
    let mut v = Vec::new();
    for c in order.chars() {
        match c {
            'G' => v.push(Block::Gate),
            'E' => {
                v.push(Block::PreEq);
                v.push(Block::Eq);
            }
            'D' => v.push(Block::Dyn),
            'I' => v.push(Block::Insert(2)),
            _ => {}
        }
    }
    if v.is_empty() {
        v = vec![
            Block::Gate,
            Block::PreEq,
            Block::Eq,
            Block::Dyn,
            Block::Insert(2),
        ];
    }
    v
}

/// Last block before a channel's PRE tap (`ptap`): IN, FILT, 3/4/5 (after proc block 1/2/3), PFL, AFL, POST.
fn ch_pre_tap(ptap: &str, proc: &str) -> Block {
    let letters: Vec<char> = proc.chars().filter(|c| "GEDI".contains(*c)).collect();
    let nth = |k: usize| match letters.get(k) {
        Some('G') => Block::Gate,
        Some('E') => Block::Eq,
        Some('D') => Block::Dyn,
        Some('I') => Block::Insert(2),
        _ => Block::Dyn,
    };
    match ptap {
        "IN" => Block::In,
        "FILT" => Block::Filter,
        "3" => nth(0),
        "4" => nth(1),
        "5" => nth(2),
        "PFL" => nth(3),
        "AFL" => Block::Fader,
        "POST" => Block::Pan,
        _ => nth(2),
    }
}

fn kind_of(generic: &str) -> StripKind {
    generic.parse().unwrap_or(StripKind::Input)
}

/// Strip of a WING source/destination token (`BUS.5`, `CH40`, `AUX.8`, `MAIN.1`, `MTX.2`).
pub fn strip_of_token(tok: &str) -> Option<StripId> {
    let t = tok.to_ascii_uppercase();
    let i = t.find(|c: char| c.is_ascii_digit())?;
    let (pre, num) = (t[..i].trim_end_matches('.'), &t[i..]);
    let n: u32 = num.parse().ok()?;
    let kind = match pre {
        "CH" => "input",
        "AUX" => "aux",
        "BUS" | "B" => "bus",
        "MAIN" | "M" => "main",
        "MTX" | "MX" => "matrix",
        "DCA" => "dca",
        _ => return None,
    };
    Some(StripId::new(kind, n))
}

fn output_kind(grp: &str) -> Option<OutputKind> {
    match grp {
        "LCL" | "AUX" | "A" | "B" | "C" | "SC" | "CRD" | "MOD" | "AES" => {
            Some(OutputKind::Physical)
        }
        "USB" => Some(OutputKind::Computer),
        "REC" => Some(OutputKind::Recorder),
        _ => None,
    }
}

/// Strip fed by a mono output/patch leg `k` of group `grp` (`BUS`, `MAIN`, `MTX`): stereo strips use 2 legs.
fn leg_strip(grp: &str, k: u32) -> Option<StripId> {
    let kind = match grp {
        "BUS" => "bus",
        "MAIN" => "main",
        "MTX" => "matrix",
        _ => return None,
    };
    (k >= 1).then(|| StripId::new(kind, k.div_ceil(2)))
}

/// Build the topology from the mirror.
pub fn build(store: &ParamStore) -> Topology {
    let r = Reader(store);
    let s = Schema::get();

    // outputs per strip
    let mut outputs: BTreeMap<StripId, BTreeSet<OutputKind>> = BTreeMap::new();
    if let Some(out) = s.by_path("io/out") {
        for g in s.children(out) {
            let gname = s.name(g);
            let Some(kind) = output_kind(gname) else {
                continue;
            };
            for leg in s.children(g) {
                let base = format!("io/out/{gname}/{}", s.name(leg));
                let (Some(src), Some(k)) = (
                    r.token(&format!("{base}/grp")),
                    r.num(&format!("{base}/in")),
                ) else {
                    continue;
                };
                if let Some(st) = leg_strip(&src, k as u32) {
                    outputs.entry(st).or_default().insert(kind);
                }
            }
        }
    }

    // talkback
    let tb_source = r.token("cfg/talk/assign").and_then(|t| strip_of_token(&t));
    let mut tb_dest: BTreeSet<StripId> = BTreeSet::new();
    for ab in ["A", "B"] {
        if let Some(node) = s.by_path(&format!("cfg/talk/{ab}")) {
            for c in s.children(node) {
                let name = s.name(c);
                if name.starts_with('$') || !(name.starts_with('B') || name.starts_with('M')) {
                    continue;
                }
                if r.on(&format!("cfg/talk/{ab}/{name}")) {
                    if let Some(st) = strip_of_token(name) {
                        tb_dest.insert(st);
                    }
                }
            }
        }
    }

    let mut strips = Vec::new();
    for &(dom, generic, count) in STRIP_DOMAINS {
        let kind = kind_of(generic);
        for n in 1..=count {
            let id = StripId::new(generic, n);
            let p = format!("{dom}/{n}");
            let mut st = Strip::new(id.clone(), kind, r.text(&format!("{p}/name")));
            st.tags = user_tags(&r.text(&format!("{p}/tags")));
            st.talkback_source = tb_source.as_ref() == Some(&id);
            st.talkback_dest = tb_dest.contains(&id);
            st.outputs = outputs
                .get(&id)
                .map(|o| o.iter().copied().collect())
                .unwrap_or_default();
            match dom {
                "ch" | "aux" => input_strip(&r, &p, dom, &mut st),
                "bus" | "main" | "mtx" => bus_strip(&r, &p, dom, n, &mut st),
                "dca" => st.chain = vec![Block::Fader],
                "fx" => {
                    st.chain = vec![Block::Model, Block::Mix, Block::Param];
                    let model = r.token(&format!("{p}/mdl")).unwrap_or_default();
                    let category = if TIME_FX_MODELS.contains(&model.as_str()) {
                        FxCategory::Time
                    } else {
                        FxCategory::Other
                    };
                    st.fx = Some(FxSlotInfo { model, category });
                }
                _ => {}
            }
            strips.push(st);
        }
    }
    Topology::new(strips)
}

fn inserts(r: &Reader, p: &str, which: &[(u8, &str)]) -> Vec<InsertInfo> {
    which
        .iter()
        .filter_map(|(n, leaf)| {
            let tok = r.token(&format!("{p}/{leaf}/ins"))?;
            let slot = tok
                .strip_prefix("FX")
                .and_then(|k| k.parse::<u32>().ok())
                .map(|k| StripId::new("fx", k));
            Some(InsertInfo {
                point: Block::Insert(*n),
                slot,
                on: r.on(&format!("{p}/{leaf}/on")),
            })
        })
        .collect()
}

fn send(
    r: &Reader,
    base: &str,
    dest: StripId,
    pre_after: &Block,
    post_after: Block,
) -> Option<SendInfo> {
    let level_db = r.num(&format!("{base}/lvl"))?;
    let tap = match r.token(&format!("{base}/mode")).as_deref() {
        Some("PRE") => SendTap::Pre,
        Some("GRP") => SendTap::Group,
        Some(_) => SendTap::Post,
        None => {
            if r.on(&format!("{base}/pre")) {
                SendTap::Pre
            } else {
                SendTap::Post
            }
        }
    };
    let tap_after = match tap {
        SendTap::Pre => pre_after.clone(),
        _ => post_after,
    };
    Some(SendInfo {
        dest,
        on: r.on(&format!("{base}/on")),
        level_db,
        tap,
        tap_after,
    })
}

fn input_strip(r: &Reader, p: &str, dom: &str, st: &mut Strip) {
    let pre_after;
    if dom == "ch" {
        let proc = r
            .token(&format!("{p}/proc"))
            .unwrap_or_else(|| "GEDI".into());
        let mut chain = vec![Block::In, Block::Filter, Block::Insert(1)];
        chain.extend(proc_blocks(&proc));
        chain.extend([Block::Fader, Block::Pan]);
        st.chain = chain;
        let ptap = r.token(&format!("{p}/ptap")).unwrap_or_else(|| "5".into());
        pre_after = ch_pre_tap(&ptap, &proc);
        st.inserts = inserts(r, p, &[(1, "preins"), (2, "postins")]);
    } else {
        st.chain = vec![
            Block::In,
            Block::Insert(1),
            Block::Eq,
            Block::Dyn,
            Block::Fader,
            Block::Pan,
        ];
        pre_after = Block::Dyn;
        st.inserts = inserts(r, p, &[(1, "preins")]);
    }
    let mut sends = Vec::new();
    for b in 1..=16 {
        sends.extend(send(
            r,
            &format!("{p}/send/{b}"),
            StripId::new("bus", b),
            &pre_after,
            Block::Fader,
        ));
    }
    for m in 1..=4 {
        sends.extend(send(
            r,
            &format!("{p}/main/{m}"),
            StripId::new("main", m),
            &pre_after,
            Block::Pan,
        ));
    }
    for k in 1..=8 {
        sends.extend(send(
            r,
            &format!("{p}/send/MX{k}"),
            StripId::new("matrix", k),
            &pre_after,
            Block::Fader,
        ));
    }
    st.sends = sends;
    // re-input of a bus/main/matrix output
    if let (Some(grp), Some(k)) = (
        r.token(&format!("{p}/in/conn/grp")),
        r.num(&format!("{p}/in/conn/in")),
    ) {
        st.reinput_of = leg_strip(&grp, k as u32);
    }
}

fn bus_strip(r: &Reader, p: &str, dom: &str, n: u32, st: &mut Strip) {
    st.chain = vec![
        Block::In,
        Block::Insert(1),
        Block::Eq,
        Block::Dyn,
        Block::Insert(2),
        Block::Fader,
        Block::Pan,
    ];
    st.inserts = inserts(r, p, &[(1, "preins"), (2, "postins")]);
    let pre_after = Block::Insert(2);
    let mut sends = Vec::new();
    if dom == "bus" {
        for b in 1..=16u32 {
            if b == n {
                continue;
            }
            sends.extend(send(
                r,
                &format!("{p}/send/{b}"),
                StripId::new("bus", b),
                &pre_after,
                Block::Fader,
            ));
        }
        for m in 1..=4 {
            sends.extend(send(
                r,
                &format!("{p}/main/{m}"),
                StripId::new("main", m),
                &pre_after,
                Block::Pan,
            ));
        }
    }
    if dom != "mtx" {
        for k in 1..=8 {
            sends.extend(send(
                r,
                &format!("{p}/send/MX{k}"),
                StripId::new("matrix", k),
                &pre_after,
                Block::Fader,
            ));
        }
    }
    st.sends = sends;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_drop_membership() {
        assert_eq!(user_tags("#D2,#M4,NOAI"), vec!["NOAI"]);
        assert_eq!(user_tags("DCA.7,MC/PRCH"), vec!["DCA.7", "MC/PRCH"]);
        assert!(user_tags("").is_empty());
    }

    #[test]
    fn tokens_and_legs() {
        assert_eq!(strip_of_token("CH40"), Some(StripId::new("input", 40)));
        assert_eq!(strip_of_token("BUS.5"), Some(StripId::new("bus", 5)));
        assert_eq!(strip_of_token("B12"), Some(StripId::new("bus", 12)));
        assert_eq!(strip_of_token("MX3"), Some(StripId::new("matrix", 3)));
        assert_eq!(leg_strip("BUS", 9), Some(StripId::new("bus", 5)));
        assert_eq!(leg_strip("BUS", 10), Some(StripId::new("bus", 5)));
        assert_eq!(ch_pre_tap("4", "EDIG"), Block::Dyn);
        assert_eq!(ch_pre_tap("PFL", "GEDI"), Block::Insert(2));
    }

    #[test]
    fn factory_topology_is_consistent() {
        let st = ParamStore::new();
        let topo = build(&st);
        assert_eq!(topo.strips.len(), 40 + 8 + 16 + 4 + 8 + 16 + 16);
        let ch1 = topo.get(&StripId::new("input", 1)).unwrap();
        assert!(ch1.sends.iter().any(|s| s.dest == StripId::new("main", 1)));
        assert_eq!(ch1.inserts.len(), 2);
        for s in &topo.strips {
            for x in &s.sends {
                assert!(topo.get(&x.dest).is_some(), "{} -> {}", s.id, x.dest);
                assert_ne!(x.dest, s.id);
            }
        }
        // factory: io/out/LCL/1 carries BUS leg 1 -> bus 1 has a physical output
        assert!(topo
            .get(&StripId::new("bus", 1))
            .unwrap()
            .outputs
            .contains(&OutputKind::Physical));
        // talkback source and destination
        st.set_enum_name(st.lookup("cfg/talk/assign").unwrap(), "CH40", 0)
            .unwrap();
        st.set(
            st.lookup("cfg/talk/A/B3").unwrap(),
            wing_model::Value::Int(1),
        )
        .unwrap();
        let topo = build(&st);
        assert!(
            topo.get(&StripId::new("input", 40))
                .unwrap()
                .talkback_source
        );
        assert!(topo.get(&StripId::new("bus", 3)).unwrap().talkback_dest);
    }
}

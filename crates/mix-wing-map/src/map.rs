//! WING tree <-> generic [`ParamId`] mapping (docs/research/remit.md appendix A.1).
//!
//! The mapping is *functional*: a WING node's generic id and class are derived from its path, and for
//! slot-positional model params (`ch/1/eq/5`, `fx/3/12`) from the **active model** of the block as the mirror
//! store currently holds it. Nothing is cached per model, so a model change re-maps automatically; the surface only
//! has to announce it ([`mix_surface::SurfaceEvent::ParamsRemapped`]).
//!
//! Generic ids follow remit.md 2.2: `<strip>/<block>[/<sub>]/<leaf>` for strips (`input.3/eq/band.2.gain`,
//! `input.3/send/bus.8/level`, `bus.5/insert.1/slot`, `fx.4/param.decay`), `source.<grp>.<n>/<leaf>` for preamps
//! and sources, `output.<grp>.<n>/<leaf>` for the output patch, `mutegroup.<n>/...` and `system/<wing path>` for
//! everything else. `$` live values become read-only `<strip>/state/<path>` params; solo/listen leaves (`$solo`,
//! `$solobd`, `$sololed`, `$presolo`) are not params at all (they are driven through the solo API).

use mix_core::ids::{ParamId, StripId};
use mix_surface::model::ParamClass;
use std::collections::{BTreeSet, HashMap};
use std::sync::OnceLock;
use wing_model::{NodeId, ParamStore, Schema};

/// How a param's value is translated between WING and generic form (beyond the descriptor's own kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// Kind follows the descriptor (fader/lin/log -> float, 0..1 int -> bool, enum -> token, ...).
    Native,
    /// Insert slot `NONE`/`FXn` <-> `none`/`fx.n`.
    InsertSlot,
    /// Channel/aux send mode `PRE`/`POST`/`GRP` <-> `pre`/`post`/`group`.
    SendMode,
    /// Bus/main send or main-assign `pre` flag 0/1 <-> `post`/`pre`.
    PreFlag,
}

/// A WING param as the generic model sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Mapped {
    pub id: ParamId,
    pub class: ParamClass,
    pub read_only: bool,
    pub codec: Codec,
    /// Meaning depends on the block's current model.
    pub model_dependent: bool,
}

/// WING strip domains and their generic kinds.
pub const STRIP_DOMAINS: &[(&str, &str, u32)] = &[
    ("ch", "input", 40),
    ("aux", "aux", 8),
    ("bus", "bus", 16),
    ("main", "main", 4),
    ("mtx", "matrix", 8),
    ("dca", "dca", 16),
    ("fx", "fx", 16),
];

/// Generic strip kind name for a WING domain (`ch` -> `input`).
pub fn generic_kind(domain: &str) -> Option<&'static str> {
    STRIP_DOMAINS
        .iter()
        .find(|(d, _, _)| *d == domain)
        .map(|(_, g, _)| *g)
}

/// WING domain for a generic kind (`input` -> `ch`).
pub fn wing_domain(kind: &str) -> Option<&'static str> {
    STRIP_DOMAINS
        .iter()
        .find(|(_, g, _)| *g == kind)
        .map(|(d, _, _)| *d)
}

/// WING path prefix of a generic strip (`input.3` -> `ch/3`).
pub fn strip_path(strip: &StripId) -> Option<String> {
    let dom = wing_domain(strip.kind_name())?;
    let n = strip.index()?;
    let max = STRIP_DOMAINS.iter().find(|(d, _, _)| *d == dom)?.2;
    (1..=max).contains(&n).then(|| format!("{dom}/{n}"))
}

/// Generic strip of a WING path (`ch/3/eq/1` -> `input.3`).
pub fn strip_of_path(path: &str) -> Option<StripId> {
    let mut it = path.split('/');
    let kind = generic_kind(it.next()?)?;
    let n: u32 = it.next()?.parse().ok()?;
    Some(StripId::new(kind, n))
}

/// Solo/listen leaves: driven through the solo API, never exposed as params.
pub fn is_solo_leaf(name: &str) -> bool {
    matches!(name, "$solo" | "$solobd" | "$sololed" | "$presolo")
}

/// Target child names of every model group, keyed by group key.
fn group_targets() -> &'static HashMap<&'static str, BTreeSet<&'static str>> {
    static T: OnceLock<HashMap<&'static str, BTreeSet<&'static str>>> = OnceLock::new();
    T.get_or_init(|| {
        let s = Schema::get();
        let mut m = HashMap::new();
        for g in s.groups() {
            let set: &mut BTreeSet<&'static str> = m.entry(g.key).or_default();
            for model in s.models(g) {
                for (t, _) in s.model_params(model) {
                    set.insert(t);
                }
            }
        }
        m
    })
}

/// Whether `id` is a model target (its descriptor depends on the parent block's model).
pub fn is_model_target(id: NodeId) -> bool {
    let s = Schema::get();
    let Some(parent) = s.parent(id) else {
        return false;
    };
    let Some(g) = s.group_of_block(parent) else {
        return false;
    };
    group_targets()
        .get(g.key)
        .is_some_and(|t| t.contains(s.name(id)))
}

/// Whether `id` is a model selector (`mdl`, `mode`, `dlymode` of a model block).
pub fn is_model_selector(id: NodeId) -> bool {
    let s = Schema::get();
    let Some(parent) = s.parent(id) else {
        return false;
    };
    s.group_of_block(parent)
        .is_some_and(|g| g.selector == s.name(id))
}

/// A slot-positional target: numeric node name inside a model block (`eq/5`, `fx/3/12`, `flt/1`).
pub fn is_slot(id: NodeId) -> bool {
    let s = Schema::get();
    s.name(id).bytes().all(|b| b.is_ascii_digit())
        && s.parent(id).is_some_and(|p| s.group_of_block(p).is_some())
}

/// Generic leaf for a model parameter name (`thr` -> `threshold`, `2g` -> `band.2.gain`, `lf` -> `low.freq`).
pub fn semantic_leaf(name: &str) -> String {
    let fixed = match name {
        "thr" => Some("threshold"),
        "att" => Some("attack"),
        "hld" => Some("hold"),
        "rel" => Some("release"),
        "det" => Some("detector"),
        "env" => Some("envelope"),
        "acc" => Some("accent"),
        "lg" | "lsg" => Some("low.gain"),
        "lf" | "lsf" => Some("low.freq"),
        "lq" => Some("low.q"),
        "leq" => Some("low.type"),
        "hg" | "hsg" => Some("high.gain"),
        "hf" | "hsf" => Some("high.freq"),
        "hq" => Some("high.q"),
        "heq" => Some("high.type"),
        _ => None,
    };
    if let Some(f) = fixed {
        return f.to_string();
    }
    // `<n>g`, `<n>f`, `<n>q`, `<n>eq`: EQ bands.
    let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
    if !digits.is_empty() {
        let rest = &name[digits.len()..];
        let what = match rest {
            "g" => Some("gain"),
            "f" => Some("freq"),
            "q" => Some("q"),
            "eq" => Some("type"),
            _ => None,
        };
        if let Some(w) = what {
            return format!("band.{digits}.{w}");
        }
    }
    sanitize(name)
}

fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() {
        "x".into()
    } else {
        s
    }
}

/// Generic block prefix of a model block (`ch/3/eq` -> `input.3/eq/`, `fx/2` -> `fx.2/param.`), the class of its
/// slots, and the generic block name used in `ParamsRemapped`.
pub fn block_prefix(block: NodeId) -> Option<(String, ParamClass, &'static str)> {
    let s = Schema::get();
    let path = s.path(block);
    let strip = strip_of_path(&path)?;
    let segs: Vec<&str> = path.split('/').collect();
    match segs.as_slice() {
        ["fx", _] => Some((format!("{strip}/param."), ParamClass::FxParam, "fx")),
        [_, _, "eq"] => Some((format!("{strip}/eq/"), ParamClass::Eq, "eq")),
        [_, _, "gate"] => Some((format!("{strip}/gate/"), ParamClass::Dynamics, "gate")),
        [_, _, "dyn"] => Some((format!("{strip}/dyn/"), ParamClass::Dynamics, "dyn")),
        [_, _, "flt"] => Some((
            format!("{strip}/filter/tool."),
            ParamClass::Filter,
            "filter",
        )),
        [_, _, "peq"] => Some((format!("{strip}/pre_eq/"), ParamClass::Eq, "pre_eq")),
        [_, _, "in", "set"] => Some((format!("{strip}/in/"), ParamClass::InputDelay, "in")),
        [_, _, "postins"] => Some((
            format!("{strip}/insert.2/"),
            ParamClass::Automix,
            "insert.2",
        )),
        [_, _, "dly"] => Some((
            format!("{strip}/out_delay/"),
            ParamClass::OutDelay,
            "out_delay",
        )),
        _ => None,
    }
}

/// Generic block name of a model selector's block (for `ParamsRemapped`).
pub fn remap_block_name(selector: NodeId) -> Option<(StripId, &'static str)> {
    let s = Schema::get();
    let block = s.parent(selector)?;
    let (_, _, name) = block_prefix(block)?;
    Some((strip_of_path(&s.path(block))?, name))
}

/// `(slot node, generic leaf)` for every slot of `block` that the active model uses. Leaves are unique: on a
/// collision the slot number is appended (`.s<n>`).
pub fn slot_leaves(store: &ParamStore, block: NodeId) -> Vec<(NodeId, String)> {
    let s = Schema::get();
    let mut out: Vec<(NodeId, String)> = Vec::new();
    for c in s.children(block) {
        if !is_slot(c) || store.is_inactive(c) {
            continue;
        }
        let Some(d) = store.desc(c) else { continue };
        let name = if d.model_param && !d.name.is_empty() {
            semantic_leaf(d.name)
        } else {
            format!("slot.{}", s.name(c))
        };
        out.push((c, name));
    }
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (_, n) in &out {
        *counts.entry(n.clone()).or_default() += 1;
    }
    for (c, n) in &mut out {
        if counts[n.as_str()] > 1 {
            *n = format!("{n}.s{}", s.name(*c));
        }
    }
    out
}

/// FX models whose slot params are an external loop (hard-forbidden `fx_external`).
pub const EXTERNAL_FX_MODELS: &[&str] = &["EXT"];

/// Map a slot node through the block's active model.
pub fn map_slot(store: &ParamStore, id: NodeId) -> Option<Mapped> {
    let s = Schema::get();
    let block = s.parent(id)?;
    let (prefix, mut class, _) = block_prefix(block)?;
    if class == ParamClass::FxParam {
        let mdl = s.child(block, "mdl")?;
        let mut tok = String::new();
        let _ = store.format_token(mdl, &mut tok);
        if EXTERNAL_FX_MODELS.contains(&tok.as_str()) {
            class = ParamClass::FxExternal;
        }
    }
    let leaf = slot_leaves(store, block)
        .into_iter()
        .find(|(c, _)| *c == id)?
        .1;
    Some(Mapped {
        id: ParamId::new(format!("{prefix}{leaf}")),
        class,
        read_only: false,
        codec: Codec::Native,
        model_dependent: true,
    })
}

/// Map any WING param node. `None` for containers, unused slots and solo/listen leaves.
pub fn map_node(store: &ParamStore, id: NodeId) -> Option<Mapped> {
    let s = Schema::get();
    if !s.is_param(id) {
        return None;
    }
    if is_slot(id) {
        if store.is_inactive(id) {
            return None;
        }
        return map_slot(store, id);
    }
    let path = s.path(id);
    let mut m = map_static_path(&path)?;
    m.model_dependent |= is_model_target(id);
    Some(m)
}

fn mk(id: String, class: ParamClass) -> Option<Mapped> {
    Some(Mapped {
        id: ParamId::new(id),
        class,
        read_only: false,
        codec: Codec::Native,
        model_dependent: false,
    })
}

fn ro(id: String) -> Option<Mapped> {
    Some(Mapped {
        id: ParamId::new(id),
        class: ParamClass::Readonly,
        read_only: true,
        codec: Codec::Native,
        model_dependent: false,
    })
}

fn with_codec(m: Option<Mapped>, codec: Codec) -> Option<Mapped> {
    m.map(|mut m| {
        m.codec = codec;
        m
    })
}

/// Map a non-slot param by path (remit.md A.1, first match wins).
pub fn map_static_path(path: &str) -> Option<Mapped> {
    let segs: Vec<&str> = path.split('/').collect();
    if segs.last().is_some_and(|l| is_solo_leaf(l)) && strip_of_path(path).is_some() {
        return None;
    }
    if let Some(strip) = strip_of_path(path) {
        return map_strip(&strip, &segs[0..1], &segs[2..]);
    }
    map_system(path, &segs)
}

fn map_strip(strip: &StripId, dom: &[&str], r: &[&str]) -> Option<Mapped> {
    use ParamClass as C;
    let dom = dom[0];
    let g = |rest: &str| format!("{strip}/{rest}");
    // `$` / `~` live values: read-only state.
    if r.iter().any(|x| x.starts_with('$') || x.starts_with('~')) {
        let leaf: Vec<String> = r
            .iter()
            .map(|x| x.trim_start_matches(['$', '~']).to_string())
            .collect();
        return ro(g(&format!("state/{}", leaf.join("."))));
    }
    if dom == "fx" {
        return match r {
            ["mdl"] => mk(g("model"), C::FxModel),
            ["fxmix"] => mk(g("mix"), C::FxMix),
            [x @ ("egrp" | "ein" | "emode" | "lat" | "trim")] => {
                mk(g(&format!("external.{x}")), C::FxExternal)
            }
            _ => mk(g(&format!("other/{}", r.join("."))), C::Unknown),
        };
    }
    if dom == "dca" {
        return match r {
            ["fdr"] => mk(g("fader"), C::Fader),
            ["mute"] => mk(g("mute"), C::Mute),
            [x @ ("name" | "col" | "icon" | "led")] => mk(g(&format!("meta/{x}")), C::Metadata),
            ["mon"] => mk(g("solo_mon"), C::SoloConfig),
            _ => mk(g(&format!("other/{}", r.join("."))), C::Unknown),
        };
    }
    let input_like = matches!(dom, "ch" | "aux");
    match r {
        ["in", "set", "trim"] => mk(g("in/trim"), C::Trim),
        ["in", "set", "bal"] => mk(g("in/balance"), C::Balance),
        ["in", "set", "inv"] => mk(g("in/polarity"), C::Polarity),
        ["in", "set", "dly"] => mk(g("in/delay"), C::InputDelay),
        ["in", "set", "dlyon"] => mk(g("in/delay.on"), C::InputDelay),
        ["in", "set", "dlymode"] => mk(g("in/delay.mode"), C::InputDelay),
        ["in", "set", "altsrc"] => mk(g("source/alt_select"), C::RoutingAltSource),
        ["in", "set", "srcauto"] => mk(g("source/auto"), C::RoutingAltSource),
        ["in", "conn", x] => mk(g(&format!("source/{x}")), C::RoutingInputPatch),
        ["flt", x] => {
            let (leaf, class) = match *x {
                "lc" => ("lowcut.on", C::Filter),
                "lcf" => ("lowcut.freq", C::Filter),
                "lcs" => ("lowcut.slope", C::Filter),
                "hc" => ("highcut.on", C::Filter),
                "hcf" => ("highcut.freq", C::Filter),
                "hcs" => ("highcut.slope", C::Filter),
                "tf" => ("tool.on", C::Filter),
                "mdl" => ("model", C::FilterModel),
                other => return mk(g(&format!("filter/{}", sanitize(other))), C::Filter),
            };
            mk(g(&format!("filter/{leaf}")), class)
        }
        ["peq", x] => mk(
            g(&format!(
                "pre_eq/{}",
                if *x == "on" {
                    "on".to_string()
                } else {
                    semantic_leaf(x)
                }
            )),
            C::Eq,
        ),
        ["eq", "mdl"] => mk(g("eq/model"), C::EqModel),
        ["eq", x] => mk(g(&format!("eq/{}", sanitize(x))), C::Eq),
        [b @ ("gate" | "dyn"), "mdl"] => mk(g(&format!("{b}/model")), C::DynamicsModel),
        [b @ ("gate" | "dyn"), "gain"] => mk(g(&format!("{b}/makeup")), C::Dynamics),
        [b @ ("gate" | "dyn"), x] => mk(g(&format!("{b}/{}", sanitize(x))), C::Dynamics),
        [sc @ ("gatesc" | "dynsc"), x] => {
            let b = if *sc == "gatesc" { "gate" } else { "dyn" };
            match *x {
                "type" => mk(g(&format!("{b}/key.type")), C::SidechainFilter),
                "f" => mk(g(&format!("{b}/key.freq")), C::SidechainFilter),
                "q" => mk(g(&format!("{b}/key.q")), C::SidechainFilter),
                "src" => mk(g(&format!("{b}/key.source")), C::SidechainSource),
                "tap" => mk(g(&format!("{b}/key.tap")), C::SidechainTap),
                o => mk(g(&format!("{b}/key.{}", sanitize(o))), C::SidechainFilter),
            }
        }
        ["dynxo", x] => {
            let leaf = match *x {
                "f" => "freq",
                o => o,
            };
            mk(g(&format!("dyn/xover.{}", sanitize(leaf))), C::Dynamics)
        }
        [ins @ ("preins" | "postins"), x] => {
            let p = if *ins == "preins" {
                "insert.1"
            } else {
                "insert.2"
            };
            match *x {
                "on" => mk(g(&format!("{p}/on")), C::InsertEnable),
                "ins" => with_codec(
                    mk(g(&format!("{p}/slot")), C::InsertSlot),
                    Codec::InsertSlot,
                ),
                "mode" => mk(g(&format!("{p}/mode")), C::Automix),
                "w" => mk(g(&format!("{p}/weight")), C::Automix),
                o => mk(g(&format!("{p}/{}", sanitize(o))), C::Unknown),
            }
        }
        ["main", m, x] => {
            let base = format!("send/main.{m}");
            match *x {
                "on" => mk(g(&format!("{base}/on")), C::SendOn),
                "lvl" => mk(g(&format!("{base}/level")), C::SendLevel),
                "pre" => with_codec(mk(g(&format!("{base}/tap")), C::SendTap), Codec::PreFlag),
                o => mk(g(&format!("{base}/{}", sanitize(o))), C::Unknown),
            }
        }
        ["send", dest, x] => {
            let d = match dest.strip_prefix("MX") {
                Some(k) => format!("matrix.{k}"),
                None => format!("bus.{dest}"),
            };
            let base = format!("send/{d}");
            match *x {
                "on" => mk(g(&format!("{base}/on")), C::SendOn),
                "lvl" => mk(g(&format!("{base}/level")), C::SendLevel),
                "pan" => mk(g(&format!("{base}/pan")), C::SendPan),
                "plink" => mk(g(&format!("{base}/pan_link")), C::SendPanLink),
                "mode" => with_codec(mk(g(&format!("{base}/tap")), C::SendTap), Codec::SendMode),
                "pre" => with_codec(mk(g(&format!("{base}/tap")), C::SendTap), Codec::PreFlag),
                // `pon`: send follows the pre/post switch of the tap ("pre on") [I]; changes what the dest gets.
                "pon" => mk(g(&format!("{base}/tap.pon")), C::SendTap),
                o => mk(g(&format!("{base}/{}", sanitize(o))), C::Unknown),
            }
        }
        ["fdr"] => mk(g("fader"), C::Fader),
        ["mute"] => mk(g("mute"), C::Mute),
        ["pan"] => mk(g("pan"), C::Pan),
        ["wid"] => mk(g("width"), C::Width),
        ["busmono"] => mk(g("mono"), C::Mono),
        ["proc"] if input_like => mk(g("order"), C::ProcessingOrder),
        ["ptap"] if input_like => mk(g("tap"), C::TapPoint),
        ["tapwid"] if input_like => mk(g("tap_width"), C::TapWidth),
        ["dly", x] => mk(g(&format!("out_delay/{}", sanitize(x))), C::OutDelay),
        ["dir", x] => mk(g(&format!("direct/{}", sanitize(x))), C::RoutingDirectIn),
        [x @ ("name" | "col" | "icon" | "led")] => mk(g(&format!("meta/{x}")), C::Metadata),
        ["tags"] => mk(g("meta/tags"), C::Tags),
        ["clink"] => mk(g("link"), C::Link),
        ["mon"] => mk(g("solo_mon"), C::SoloConfig),
        ["solosafe"] => mk(g("solo_safe"), C::SoloConfig),
        _ => mk(g(&format!("other/{}", r.join("."))), C::Unknown),
    }
}

fn map_system(path: &str, segs: &[&str]) -> Option<Mapped> {
    use ParamClass as C;
    let sys = || format!("system/{path}");
    let class = match segs {
        ["$ctl", "lib", ..] => C::Scene,
        ["$ctl", ..] => C::ControlSurface,
        ["$globals", "$savenow"] => C::ControlSurface,
        ["$globals", x] if matches!(*x, "clkrate" | "clksrc" | "custsync") => C::Clock,
        ["$globals", ..] | ["$syscfg", ..] | ["cards", ..] | ["cfg", "dcamgrp"] => C::System,
        ["$stat", ..] => return ro(sys()),
        ["cfg", "rta", x] | ["cfg", "mtr", x, ..] if x.starts_with('$') => C::InspectRta,
        ["play", ..] => C::Player,
        ["rec", ..] => C::Recorder,
        ["cfg", "mon", ..] => C::MonitorSection,
        ["cfg", "talk", ..] => C::Talkback,
        ["cfg", "solo", ..] => C::SoloConfig,
        ["cfg", "rta", ..] | ["cfg", "mtr", ..] => C::MeterConfig,
        ["cfg", "mainlink"] => C::Link,
        ["cfg", "amix", ..] => C::AutomixConfig,
        ["io", "in", grp, n, rest @ ..] => {
            let id = format!("source.{}.{n}/{}", grp.to_ascii_lowercase(), rest.join("."));
            if rest.iter().any(|x| x.starts_with('$')) {
                return ro(id);
            }
            let c = if *grp == "OSC" {
                C::Oscillator
            } else if *grp == "USR" && rest.first() == Some(&"user") {
                C::RoutingUserSignal
            } else {
                match rest {
                    ["g"] => C::PreampGain,
                    ["vph"] => C::Phantom,
                    ["pol"] => C::SourcePolarity,
                    ["mute"] => C::SourceMute,
                    ["mode"] => C::SourceMode,
                    ["rmt"] | ["rcvc"] => C::PreampRemote,
                    ["col"] | ["name"] | ["icon"] | ["tags"] => C::Metadata,
                    _ => C::Unknown,
                }
            };
            return mk(id, c);
        }
        ["io", "out", grp, n, rest @ ..] => {
            let id = format!("output.{}.{n}/{}", grp.to_ascii_lowercase(), rest.join("."));
            if rest.iter().any(|x| x.starts_with('$')) {
                return ro(id);
            }
            return mk(id, C::RoutingOutputPatch);
        }
        ["io", "altsw"] | ["io", "autoaltovr"] => C::RoutingAltSource,
        ["mgrp", n, "mute"] => return mk(format!("mutegroup.{n}/mute"), C::MutegroupMute),
        ["mgrp", n, x] => return mk(format!("mutegroup.{n}/meta/{x}"), C::Metadata),
        _ if segs.iter().any(|x| x.starts_with('$')) => return ro(sys()),
        _ => C::Unknown,
    };
    let live = !segs[0].starts_with('$') && segs.iter().any(|x| x.starts_with('$'));
    if live && class != C::InspectRta {
        // live values inside configuration domains: read-only, keep the domain class for display only
        let mut m = mk(sys(), class)?;
        m.read_only = true;
        m.class = C::Readonly;
        return Some(m);
    }
    let mut m = mk(sys(), class)?;
    if class == C::InspectRta {
        m.read_only = true;
    }
    Some(m)
}

// ---- preamp aliases (remit.md A.1/A.3) --------------------------------------------------------------------------

/// `<strip>/preamp/<leaf>` aliases of the source params that feed a strip: (generic leaf, `io/in/<grp>/<n>/` leaf).
pub const PREAMP_ALIASES: &[(&str, &str)] = &[("gain", "g"), ("phantom", "vph")];

/// WING path prefix (`io/in/A/3`) of the source currently feeding an input/aux strip: its `in/conn` patch, or the
/// alt source (`in/conn/altgrp`/`altin`) while `in/set/altsrc` is on. `None` for other strips, unpatched or
/// source-less (`OFF`) strips.
pub fn preamp_source(store: &ParamStore, strip: &StripId) -> Option<String> {
    let root = strip_path(strip)?;
    if !root.starts_with("ch/") && !root.starts_with("aux/") {
        return None;
    }
    let alt = store
        .lookup(&format!("{root}/in/set/altsrc"))
        .and_then(|n| store.get_f64(n))
        .is_some_and(|v| v != 0.0);
    let (g, i) = if alt {
        ("altgrp", "altin")
    } else {
        ("grp", "in")
    };
    let grp = store.lookup(&format!("{root}/in/conn/{g}"))?;
    let mut tok = String::new();
    store.format_token(grp, &mut tok).ok()?;
    let n = store.get_f64(store.lookup(&format!("{root}/in/conn/{i}"))?)? as i64;
    (n > 0).then(|| format!("io/in/{tok}/{n}"))
}

/// The `<strip>/preamp/<leaf>` alias params of `strip` that resolve under the current patch, as (source node,
/// mapping). The mapping has the alias id and the class/codec of the source param (`preamp_gain`, `phantom`), so
/// remit treats the alias exactly like the source.
pub fn preamp_aliases(store: &ParamStore, strip: &StripId) -> Vec<(NodeId, Mapped)> {
    let Some(prefix) = preamp_source(store, strip) else {
        return Vec::new();
    };
    PREAMP_ALIASES
        .iter()
        .filter_map(|(leaf, src)| {
            let node = store.lookup(&format!("{prefix}/{src}"))?;
            let mut m = map_node(store, node)?;
            m.id = ParamId::new(format!("{strip}/preamp/{leaf}"));
            Some((node, m))
        })
        .collect()
}

/// Resolve a `<strip>/preamp/<leaf>` alias id.
pub fn resolve_preamp_alias(store: &ParamStore, id: &ParamId) -> Option<(NodeId, Mapped)> {
    let strip = id.strip()?;
    preamp_aliases(store, &strip)
        .into_iter()
        .find(|(_, m)| &m.id == id)
}

/// Every input/aux strip whose preamp aliases currently resolve to `source` (a node below `io/in/<grp>/<n>/`),
/// with the alias mappings: used to mirror source changes onto the aliases.
pub fn aliases_of_source(store: &ParamStore, source: NodeId) -> Vec<(StripId, Mapped)> {
    let s = Schema::get();
    let path = s.path(source);
    let Some((prefix, _)) = path.rsplit_once('/') else {
        return Vec::new();
    };
    if !path.starts_with("io/in/") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (dom, kind, n) in STRIP_DOMAINS {
        if !matches!(*dom, "ch" | "aux") {
            continue;
        }
        for i in 1..=*n {
            let strip = StripId::new(kind, i);
            if preamp_source(store, &strip).as_deref() == Some(prefix) {
                out.extend(
                    preamp_aliases(store, &strip)
                        .into_iter()
                        .filter(|(n, _)| *n == source)
                        .map(|(_, m)| (strip.clone(), m)),
                );
            }
        }
    }
    out
}

/// The strip whose patch/alt-source selector `node` is (`ch/3/in/conn/grp`, `.../in`, `altgrp`, `altin`,
/// `in/set/altsrc`): its preamp aliases then point somewhere else.
pub fn patch_strip_of(node: NodeId) -> Option<StripId> {
    let s = Schema::get();
    let path = s.path(node);
    let segs: Vec<&str> = path.split('/').collect();
    match segs.as_slice() {
        [d @ ("ch" | "aux"), _, "in", "conn", "grp" | "in" | "altgrp" | "altin"]
        | [d @ ("ch" | "aux"), _, "in", "set", "altsrc"] => {
            strip_of_path(&path).filter(|_| generic_kind(d).is_some())
        }
        _ => None,
    }
}

/// Reverse index for non-slot params (generic id -> node) and model-block prefixes (for slot lookups).
pub struct Index {
    by_id: HashMap<ParamId, NodeId>,
    blocks: HashMap<String, NodeId>,
}

impl Index {
    /// Build over the whole schema (a few ms in release).
    pub fn build(store: &ParamStore) -> Index {
        let s = Schema::get();
        let mut by_id = HashMap::new();
        let mut blocks = HashMap::new();
        for id in s.ids() {
            if !s.is_param(id) {
                if s.group_of_block(id).is_some() {
                    if let Some((prefix, _, _)) = block_prefix(id) {
                        blocks.insert(prefix, id);
                    }
                }
                continue;
            }
            if is_slot(id) {
                continue;
            }
            if let Some(m) = map_node(store, id) {
                by_id.insert(m.id, id);
            }
        }
        Index { by_id, blocks }
    }

    /// Node of a generic id under the store's current models.
    pub fn node(&self, store: &ParamStore, id: &ParamId) -> Option<NodeId> {
        if let Some(n) = self.by_id.get(id) {
            return Some(*n);
        }
        let s = id.as_str();
        // slot ids: `<prefix><leaf>` where prefix ends in `/` or `/param.` / `/filter/tool.`
        for (cut, _) in s.match_indices(['/', '.']) {
            let (prefix, leaf) = s.split_at(cut + 1);
            if let Some(&block) = self.blocks.get(prefix) {
                if let Some((n, _)) = slot_leaves(store, block)
                    .into_iter()
                    .find(|(_, l)| l == leaf)
                {
                    return Some(n);
                }
            }
        }
        None
    }

    /// Number of static entries (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn preamp_alias_resolves_through_the_patch_and_alt_source() {
        use wing_model::Value as W;
        let store = ParamStore::new();
        let strip = StripId::new("input", 2);
        assert!(preamp_aliases(&store, &StripId::new("bus", 1)).is_empty());
        let set = |p: &str, v: W| store.set_from(store.lookup(p).unwrap(), v, 1).unwrap();
        let tok = |p: &str, t: &str| store.set_enum_name(store.lookup(p).unwrap(), t, 1).unwrap();
        tok("ch/2/in/conn/grp", "LCL");
        set("ch/2/in/conn/in", W::Int(4));
        tok("ch/2/in/conn/altgrp", "A");
        set("ch/2/in/conn/altin", W::Int(9));
        let a = preamp_aliases(&store, &strip);
        let ids: Vec<_> = a.iter().map(|(_, m)| m.id.as_str().to_string()).collect();
        assert_eq!(ids, ["input.2/preamp/gain", "input.2/preamp/phantom"]);
        assert_eq!(Schema::get().path(a[0].0), "io/in/LCL/4/g");
        assert_eq!(a[0].1.class, ParamClass::PreampGain);
        assert_eq!(a[1].1.class, ParamClass::Phantom);
        let (n, _) = resolve_preamp_alias(&store, &ParamId::new("input.2/preamp/gain")).unwrap();
        assert!(aliases_of_source(&store, n)
            .iter()
            .any(|(s, _)| *s == strip));
        assert!(patch_strip_of(store.lookup("ch/2/in/conn/in").unwrap()) == Some(strip.clone()));
        // alt source on: the alias follows io/in/A/9
        set("ch/2/in/set/altsrc", W::Int(1));
        let a = preamp_aliases(&store, &strip);
        assert_eq!(Schema::get().path(a[0].0), "io/in/A/9/g");
        assert!(!aliases_of_source(&store, n)
            .iter()
            .any(|(s, _)| *s == strip));
        // no source: no alias
        tok("ch/2/in/conn/altgrp", "OFF");
        assert!(preamp_aliases(&store, &strip).is_empty());
    }

    use super::*;
    use std::collections::BTreeMap;

    fn store() -> ParamStore {
        ParamStore::new()
    }

    fn map(st: &ParamStore, p: &str) -> Mapped {
        let id = st.lookup(p).unwrap_or_else(|| panic!("no node {p}"));
        map_node(st, id).unwrap_or_else(|| panic!("{p} unmapped"))
    }

    #[test]
    fn strip_params_map_per_remit_a1() {
        let st = store();
        let cases = [
            ("ch/3/in/set/trim", "input.3/in/trim", ParamClass::Trim),
            (
                "ch/3/flt/lcf",
                "input.3/filter/lowcut.freq",
                ParamClass::Filter,
            ),
            ("ch/3/eq/mdl", "input.3/eq/model", ParamClass::EqModel),
            ("ch/3/eq/1", "input.3/eq/low.gain", ParamClass::Eq),
            ("ch/3/eq/8", "input.3/eq/band.2.gain", ParamClass::Eq),
            ("ch/3/dyn/1", "input.3/dyn/threshold", ParamClass::Dynamics),
            (
                "ch/3/gate/1",
                "input.3/gate/threshold",
                ParamClass::Dynamics,
            ),
            (
                "ch/3/gatesc/src",
                "input.3/gate/key.source",
                ParamClass::SidechainSource,
            ),
            (
                "ch/3/send/8/lvl",
                "input.3/send/bus.8/level",
                ParamClass::SendLevel,
            ),
            (
                "ch/3/send/8/mode",
                "input.3/send/bus.8/tap",
                ParamClass::SendTap,
            ),
            (
                "ch/3/main/1/on",
                "input.3/send/main.1/on",
                ParamClass::SendOn,
            ),
            (
                "aux/2/send/MX3/lvl",
                "aux.2/send/matrix.3/level",
                ParamClass::SendLevel,
            ),
            (
                "bus/5/preins/ins",
                "bus.5/insert.1/slot",
                ParamClass::InsertSlot,
            ),
            ("bus/5/dly/on", "bus.5/out_delay/on", ParamClass::OutDelay),
            ("main/1/fdr", "main.1/fader", ParamClass::Fader),
            (
                "mtx/2/dir/in",
                "matrix.2/direct/in",
                ParamClass::RoutingDirectIn,
            ),
            ("dca/4/mute", "dca.4/mute", ParamClass::Mute),
            ("ch/1/tags", "input.1/meta/tags", ParamClass::Tags),
            ("ch/1/clink", "input.1/link", ParamClass::Link),
            (
                "ch/1/in/conn/grp",
                "input.1/source/grp",
                ParamClass::RoutingInputPatch,
            ),
            ("ch/1/$fdr", "input.1/state/fdr", ParamClass::Readonly),
            ("fx/1/mdl", "fx.1/model", ParamClass::FxModel),
            ("mgrp/2/mute", "mutegroup.2/mute", ParamClass::MutegroupMute),
            (
                "cfg/solo/mode",
                "system/cfg/solo/mode",
                ParamClass::SoloConfig,
            ),
            (
                "cfg/rta/rtasrc",
                "system/cfg/rta/rtasrc",
                ParamClass::MeterConfig,
            ),
            (
                "$globals/clksrc",
                "system/$globals/clksrc",
                ParamClass::Clock,
            ),
            ("io/in/LCL/3/g", "source.lcl.3/g", ParamClass::PreampGain),
            ("io/in/A/3/vph", "source.a.3/vph", ParamClass::Phantom),
        ];
        for (p, id, class) in cases {
            let m = map(&st, p);
            assert_eq!((m.id.as_str(), m.class), (id, class), "{p}");
        }
        assert!(map(&st, "ch/1/$fdr").read_only);
        assert!(map(&st, "ch/1/eq/8").model_dependent);
        assert_eq!(map(&st, "bus/1/main/1/pre").codec, Codec::PreFlag);
        // solo leaves are not params
        assert!(map_node(&st, st.lookup("ch/1/$solo").unwrap()).is_none());
        assert!(map_node(&st, st.lookup("ch/1/eq/$solobd").unwrap()).is_none());
    }

    #[test]
    fn every_param_maps_once_and_nothing_is_unknown() {
        let st = store();
        let s = Schema::get();
        let mut seen: BTreeMap<String, String> = BTreeMap::new();
        let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
        let mut unknown = Vec::new();
        for id in s.ids().filter(|&i| s.is_param(i)) {
            let Some(m) = map_node(&st, id) else { continue };
            *counts.entry(m.class.as_str()).or_default() += 1;
            if m.class == ParamClass::Unknown {
                unknown.push(s.path(id));
            }
            if let Some(prev) = seen.insert(m.id.to_string(), s.path(id)) {
                panic!("{} and {} both map to {}", prev, s.path(id), m.id);
            }
        }
        assert!(
            unknown.is_empty(),
            "unclassified: {:?}",
            &unknown[..unknown.len().min(20)]
        );
        assert!(seen.len() > 30_000, "{}", seen.len());
        assert_eq!(counts["fader"], 92);
        assert_eq!(counts["mute"], 92);
        assert_eq!(counts["trim"], 76);
    }

    #[test]
    fn index_round_trips_including_slots() {
        let st = store();
        let idx = Index::build(&st);
        let s = Schema::get();
        for id in s.ids().filter(|&i| s.is_param(i)) {
            if let Some(m) = map_node(&st, id) {
                assert_eq!(idx.node(&st, &m.id), Some(id), "{}", m.id);
            }
        }
        // switching the EQ model renames the slots
        let mdl = st.lookup("ch/1/eq/mdl").unwrap();
        st.set_enum_name(mdl, "SOUL", 0).unwrap();
        assert!(idx
            .node(&st, &ParamId::new("input.1/eq/band.2.gain"))
            .is_none());
        let soul: Vec<String> = slot_leaves(&st, s.parent(mdl).unwrap())
            .into_iter()
            .map(|x| x.1)
            .collect();
        assert!(!soul.is_empty());
        for l in &soul {
            let id = ParamId::new(format!("input.1/eq/{l}"));
            assert!(idx.node(&st, &id).is_some(), "{id}");
        }
    }

    #[test]
    fn semantic_leaves() {
        assert_eq!(semantic_leaf("thr"), "threshold");
        assert_eq!(semantic_leaf("4q"), "band.4.q");
        assert_eq!(semantic_leaf("heq"), "high.type");
        assert_eq!(semantic_leaf("Decay Time"), "decay_time");
    }
}

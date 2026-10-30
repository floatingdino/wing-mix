//! Consistency checker: every cross-reference resolves, ranges are ordered, units and names are known.
//!
//! [`check`] returns every problem found (not just the first), each prefixed with `file:item:` so a broken data file
//! points at the offending entry.

use std::collections::{BTreeSet, HashSet};

use crate::types::*;
use crate::KnowledgeBase;

/// Symbolic parameter values allowed in actions (workflow.toml `[schema.action].symbolic_values`).
const SYMBOLS: &[&str] = &[
    "resonance_hz",
    "masking_peak_hz",
    "band_center",
    "useful_range_hz.hi",
    "useful_range_hz.lo",
    "loudness_match",
    "from_source",
    "<self>",
    "<other>",
    "<yielding>",
    "<keeping>",
];

/// Non-block action keys that are still legal params of any action (side-chain key source etc.).
const EXTRA_PARAMS: &[&str] = &["sidechain", "range_db", "level_db", "position_pct", "slope_db_per_oct"];

/// Descriptive groups of FX buses an fx rule may target.
const FX_TARGET_GROUPS: &[&str] = &["reverb fx_bus", "reverb fx_bus returns"];

/// Statistics a threshold can be relative to.
const THRESHOLD_STATS: &[&str] = &["noise_floor_dbfs", "rms_dbfs", "peak_dbfs", "lufs_s"];

/// Param-name suffixes with a known unit (workflow.toml `[schema.action].fields.param_suffixes`).
const UNIT_SUFFIXES: &[&str] = &["_ratio", "_hz", "_db", "_ms", "_pct", "_s", "_oct", "_lu", "_beats", "_dbfs", "_dbtp", "_per_oct"];

struct Ctx<'a> {
    kb: &'a KnowledgeBase,
    out: Vec<String>,
    source_ids: HashSet<&'a str>,
    families: HashSet<&'static str>,
    fx_ids: HashSet<&'a str>,
    gate_ids: HashSet<&'a str>,
    comp_ids: HashSet<&'a str>,
    deess_ids: HashSet<&'a str>,
    send_ids: HashSet<&'a str>,
}

impl Ctx<'_> {
    fn err(&mut self, ctx: &str, msg: impl AsRef<str>) {
        self.out.push(format!("{ctx}: {}", msg.as_ref()));
    }

    fn range(&mut self, ctx: &str, what: &str, r: &Range) {
        if !r.is_ordered() {
            self.err(ctx, format!("range {what} = [{}, {}] is not ordered (lo <= hi)", r.lo(), r.hi()));
        }
    }

    fn opt_range(&mut self, ctx: &str, what: &str, r: &Option<Range>) {
        if let Some(r) = r {
            self.range(ctx, what, r);
        }
    }

    fn refs(&mut self, ctx: &str, keys: &[String]) {
        if keys.is_empty() {
            self.err(ctx, "`sources` is empty");
        }
        for k in keys {
            if !self.kb.workflow.sources.contains_key(k) {
                self.err(ctx, format!("unknown source reference `{k}` (not in workflow.toml [sources])"));
            }
        }
    }

    fn unique<'b>(&mut self, file: &str, kind: &str, ids: impl Iterator<Item = &'b str>) {
        let mut seen = HashSet::new();
        for id in ids {
            if !seen.insert(id) {
                self.err(file, format!("duplicate {kind} id `{id}`"));
            }
        }
    }

    /// A source id or family name.
    fn is_scope(&self, s: &str) -> bool {
        self.source_ids.contains(s) || self.families.contains(s)
    }

    /// `bare_band_ok`: in conditions a per-band feature without suffix means "in whichever band is being examined".
    fn feature(&mut self, ctx: &str, feature: &str, bare_band_ok: bool) {
        let parts: Vec<&str> = feature.split('.').collect();
        let Some(def) = self.kb.workflow.features.get(parts[0]) else {
            self.err(ctx, format!("unknown feature `{feature}`"));
            return;
        };
        let mut rest = &parts[1..];
        if def.per.as_deref() == Some("band") {
            match rest.first() {
                Some(b) if self.kb.workflow.bands.contains_key(*b) => rest = &rest[1..],
                Some(b) => {
                    self.err(ctx, format!("feature `{feature}`: `{b}` is not a band in workflow.toml [bands]"));
                    return;
                }
                None if bare_band_ok => return,
                None => {
                    self.err(ctx, format!("feature `{feature}` is per-band and needs a band suffix"));
                    return;
                }
            }
        }
        match rest {
            [] => {}
            [scope] if self.is_scope(scope) => {}
            [scope] => self.err(ctx, format!("feature `{feature}`: scope `{scope}` is not a source id or family")),
            _ => self.err(ctx, format!("feature `{feature}` has too many suffixes")),
        }
    }

    fn conditions(&mut self, ctx: &str, conds: &[Condition]) {
        for c in conds {
            self.feature(ctx, &c.feature, true);
            match (c.op, c.value) {
                (Op::Between | Op::Outside, NumOrRange::Range(r)) => self.range(ctx, &c.feature, &r),
                (Op::Between | Op::Outside, NumOrRange::Num(_)) => {
                    self.err(ctx, format!("condition on `{}`: between/outside need a [lo, hi] value", c.feature))
                }
                (_, NumOrRange::Range(_)) => {
                    self.err(ctx, format!("condition on `{}`: {:?} needs a scalar value", c.feature, c.op))
                }
                _ => {}
            }
        }
    }

    fn param_name(&mut self, ctx: &str, name: &str) {
        let known_block_param = self.kb.workflow.blocks.values().any(|b| b.params.iter().any(|p| p == name));
        let is_feature = self.kb.workflow.features.contains_key(name);
        if !known_block_param && !is_feature && !EXTRA_PARAMS.contains(&name) {
            self.err(ctx, format!("unknown parameter `{name}` (not in any workflow.toml [blocks.*].params or [features])"));
        }
        if EXTRA_PARAMS.contains(&name) || is_feature {
            return;
        }
        let has_unit = UNIT_SUFFIXES.iter().any(|s| name.ends_with(s))
            || matches!(name, "q" | "ratio" | "size" | "kind" | "band" | "type" | "mode" | "detector")
            || name.starts_with("threshold_rel")
            || name == "model_class"
            || name == "target"
            || name == "tap";
        if !has_unit {
            self.err(ctx, format!("parameter `{name}` has no known unit suffix"));
        }
    }

    fn action(&mut self, ctx: &str, a: &Action) {
        let Some(block) = self.kb.workflow.blocks.get(&a.block) else {
            self.err(ctx, format!("action block `{}` is not in workflow.toml [blocks]", a.block));
            return;
        };
        if let Some(kind) = &a.kind {
            if !block.kinds.contains(kind) {
                self.err(ctx, format!("action kind `{kind}` is not allowed for block {} (allowed: {:?})", a.block, block.kinds));
            }
        }
        if let Some(ty) = &a.ty {
            if !block.types.contains(ty) {
                self.err(ctx, format!("action type `{ty}` is not allowed for block {} (allowed: {:?})", a.block, block.types));
            }
        }
        if let Some(m) = &a.mode {
            if !block.modes.contains(m) {
                self.err(ctx, format!("action mode `{m}` is not allowed for block {} (allowed: {:?})", a.block, block.modes));
            }
        }
        if let Some(q) = &a.q_class {
            if !self.kb.eq.q_class.contains_key(q) {
                self.err(ctx, format!("unknown q_class `{q}` (not in eq.toml [q_class])"));
            }
            if a.params.contains_key("q") {
                self.err(ctx, "both q_class and q given");
            }
        }
        if let Some(p) = &a.preset {
            if p != "from_source" && !self.comp_ids.contains(p.as_str()) && !self.gate_ids.contains(p.as_str()) {
                self.err(ctx, format!("unknown preset `{p}`"));
            }
        }
        if let Some(t) = &a.target {
            if !SYMBOLS.contains(&t.as_str()) && !self.is_scope(t) && t != "*" {
                self.err(ctx, format!("unknown action target `{t}`"));
            }
        }
        for (k, v) in &a.params {
            if k != "band" && !block.params.contains(k) && !EXTRA_PARAMS.contains(&k.as_str()) {
                self.err(ctx, format!("parameter `{k}` is not a parameter of block {} (allowed: {:?})", a.block, block.params));
            }
            self.param_name(ctx, k);
            match v {
                Param::Range(r) => self.range(ctx, k, r),
                Param::Symbol(s) => {
                    if k == "band" {
                        if !block.bands.contains(s) {
                            self.err(ctx, format!("unknown Eq band `{s}`"));
                        }
                    } else if !SYMBOLS.contains(&s.as_str()) && !block.model_classes.contains(s) {
                        self.err(ctx, format!("unknown symbolic value `{s}` for parameter `{k}`"));
                    }
                }
                Param::Num(_) => {}
            }
        }
    }

    fn rule(&mut self, file: &str, r: &Rule) {
        let ctx = format!("{file}:{}", r.id);
        // A rule without conditions is always applicable (fx_tempo_sync, eq_source_problem).
        self.conditions(&ctx, &r.when);
        self.conditions(&ctx, &r.when_any);
        for a in &r.applies_to {
            if a != "*" && a != "mix" && !self.is_scope(a) {
                self.err(&ctx, format!("applies_to `{a}` is not a source id, family, \"*\" or \"mix\""));
            }
        }
        if r.adjust.is_empty() && r.action.is_none() && r.strategy.is_none() {
            self.err(&ctx, "rule has neither adjust, action nor strategy");
        }
        if let Some(p) = r.priority {
            if !(1..=5).contains(&p) {
                self.err(&ctx, format!("priority {p} is outside 1..=5"));
            }
        }
        if let Some(s) = &r.scope {
            if s != "strip" && s != "mix" {
                self.err(&ctx, format!("scope `{s}` must be strip or mix"));
            }
        }
        for adj in &r.adjust {
            self.param_name(&ctx, &adj.param);
            if adj.by.is_none() && adj.to.is_none() && adj.order.is_none() {
                self.err(&ctx, format!("adjust `{}` has neither by, to nor order", adj.param));
            }
            if adj.by.is_some() && adj.to.is_some() {
                self.err(&ctx, format!("adjust `{}` has both by and to", adj.param));
            }
            self.opt_range(&ctx, &adj.param, &adj.by);
            match &adj.to {
                Some(Param::Range(rg)) => self.range(&ctx, &adj.param, rg),
                Some(Param::Symbol(s)) if !SYMBOLS.contains(&s.as_str()) && !s.starts_with("tempo.") => {
                    self.err(&ctx, format!("unknown symbolic value `{s}` in adjust `{}`", adj.param))
                }
                _ => {}
            }
            if let Some(t) = &adj.target {
                if !self.fx_ids.contains(t.as_str()) && !self.is_scope(t) && !FX_TARGET_GROUPS.contains(&t.as_str()) && t != "*" && t != "mix" {
                    self.err(&ctx, format!("adjust target `{t}` is not an fx_bus, fx group, source id, family, \"*\" or \"mix\""));
                }
            }
        }
        match &r.action {
            Some(RuleAction::Block(a)) => self.action(&ctx, a),
            Some(RuleAction::Delegate(d)) if d.from != "sources.toml problems" => {
                self.err(&ctx, format!("unknown delegation `{}` (only \"sources.toml problems\" exists)", d.from))
            }
            _ => {}
        }
        self.refs(&ctx, &r.sources);
    }
}

/// Run every consistency check; an empty vector means the knowledge base is consistent.
pub fn check(kb: &KnowledgeBase) -> Vec<String> {
    let wf = &kb.workflow;
    let mut c = Ctx {
        kb,
        out: Vec::new(),
        source_ids: kb.sources.source.iter().map(|s| s.id.as_str()).collect(),
        families: [
            Family::Drums,
            Family::Bass,
            Family::Guitar,
            Family::Keys,
            Family::Vocal,
            Family::Playback,
            Family::Ambience,
            Family::Speech,
            Family::BrassStrings,
        ]
        .into_iter()
        .map(Family::as_str)
        .collect(),
        fx_ids: kb.fx.fx_bus.iter().map(|b| b.id.as_str()).collect(),
        gate_ids: kb.dynamics.gate_preset.iter().map(|p| p.id.as_str()).collect(),
        comp_ids: kb.dynamics.comp_preset.iter().map(|p| p.id.as_str()).collect(),
        deess_ids: kb.dynamics.deess_preset.iter().map(|p| p.id.as_str()).collect(),
        send_ids: kb.fx.send_preset.iter().map(|p| p.id.as_str()).collect(),
    };

    // ---- headers
    for (name, h) in [
        ("workflow", &kb.workflow.schema),
        ("sources", &kb.sources.schema),
        ("dynamics", &kb.dynamics.schema),
        ("eq", &kb.eq.schema),
        ("fx", &kb.fx.schema),
        ("vocabulary", &kb.vocabulary.schema),
    ] {
        if h.version != 1 {
            c.err(name, format!("unsupported schema version {} (this crate reads 1)", h.version));
        }
        if h.file != name {
            c.err(name, format!("[schema] file = \"{}\" does not match the file name", h.file));
        }
    }

    // ---- workflow.toml
    for (name, f) in &wf.features {
        if !wf.schema.units.contains_key(&f.unit) {
            c.err(&format!("workflow:features.{name}"), format!("unknown unit `{}`", f.unit));
        }
        if let Some(per) = &f.per {
            if per != "band" {
                c.err(&format!("workflow:features.{name}"), format!("unknown `per` value `{per}`"));
            }
        }
    }
    {
        let mut bands: Vec<(&String, &Range)> = wf.bands.iter().collect();
        bands.sort_by(|a, b| a.1.lo().total_cmp(&b.1.lo()));
        for (n, r) in &bands {
            c.range(&format!("workflow:bands.{n}"), "band", r);
        }
        for w in bands.windows(2) {
            if (w[0].1.hi() - w[1].1.lo()).abs() > 1e-9 {
                c.err(
                    "workflow:bands",
                    format!("bands `{}` and `{}` are not contiguous ({} vs {})", w[0].0, w[1].0, w[0].1.hi(), w[1].1.lo()),
                );
            }
        }
    }
    for (name, b) in &wf.blocks {
        let ctx = format!("workflow:blocks.{name}");
        if b.params.is_empty() {
            c.err(&ctx, "no params");
        }
    }
    let mut orders: Vec<u32> = wf.phase.iter().map(|p| p.order).collect();
    orders.sort_unstable();
    if orders != (1..=wf.phase.len() as u32).collect::<Vec<_>>() {
        c.err("workflow:phase", format!("phase `order` values must be 1..={} without gaps, got {orders:?}", wf.phase.len()));
    }
    c.unique("workflow", "phase", wf.phase.iter().map(|p| p.id.as_str()));
    let phase_ids: BTreeSet<&str> = wf.phase.iter().map(|p| p.id.as_str()).collect();
    for p in &wf.phase {
        let ctx = format!("workflow:phase.{}", p.id);
        for b in &p.blocks {
            if !wf.blocks.contains_key(b) {
                c.err(&ctx, format!("unknown block `{b}`"));
            }
        }
        c.conditions(&ctx, &p.done_when);
        c.conditions(&ctx, p.gate_tolerance.as_slice());
        c.refs(&ctx, &p.sources);
    }
    for id in &wf.iteration.revisit_lower_phases_after {
        if !phase_ids.contains(id.as_str()) {
            c.err("workflow:iteration", format!("revisit_lower_phases_after: unknown phase `{id}`"));
        }
    }
    c.range("workflow:iteration", "settle_s", &wf.iteration.settle_s);
    let t = &wf.targets;
    c.range("workflow:targets.channel", "peak_dbfs", &t.channel.peak_dbfs);
    c.range("workflow:targets.channel", "rms_dbfs", &t.channel.rms_dbfs);
    c.refs("workflow:targets.channel", &t.channel.sources);
    c.range("workflow:targets.mix_bus", "peak_dbfs", &t.mix_bus.peak_dbfs);
    c.refs("workflow:targets.mix_bus", &t.mix_bus.sources);
    c.refs("workflow:targets.delivery", &t.delivery.sources);
    for (n, p) in &t.delivery.profiles {
        let ctx = format!("workflow:targets.delivery.{n}");
        if let Some(NumOrRange::Range(r)) = p.lufs_i {
            c.range(&ctx, "lufs_i", &r);
        }
        c.opt_range(&ctx, "plr_db", &p.plr_db);
    }
    for band in t.reference_matching.band_tolerance_db.keys() {
        if !wf.bands.contains_key(band) {
            c.err("workflow:targets.reference_matching", format!("band_tolerance_db: unknown band `{band}`"));
        }
    }
    for band in wf.bands.keys() {
        if !t.reference_matching.band_tolerance_db.contains_key(band) {
            c.err("workflow:targets.reference_matching", format!("band_tolerance_db is missing band `{band}`"));
        }
    }
    c.refs("workflow:targets.reference_matching", &t.reference_matching.sources);

    // ---- sources.toml
    c.unique("sources", "source", kb.sources.source.iter().map(|s| s.id.as_str()));
    for s in &kb.sources.source {
        let ctx = format!("sources:{}", s.id);
        if s.name_hints.is_empty() {
            c.err(&ctx, "name_hints is empty");
        }
        for h in &s.name_hints {
            if h.trim().is_empty() || h.starts_with('?') {
                c.err(&ctx, format!("malformed name hint `{h}`"));
            }
        }
        c.range(&ctx, "fundamental_hz", &s.fundamental_hz);
        c.range(&ctx, "useful_range_hz", &s.useful_range_hz);
        c.range(&ctx, "pan_pct", &s.pan_pct);
        c.range(&ctx, "level_rel_mix_lu", &s.level_rel_mix_lu);
        if !(Range([-100.0, 100.0])).contains_range(&s.pan_pct) {
            c.err(&ctx, "pan_pct must lie within -100..100");
        }
        if !s.useful_range_hz.contains_range(&s.fundamental_hz) {
            c.err(&ctx, "fundamental_hz must lie within useful_range_hz");
        }
        for (name, f) in [("hpf", Some(&s.hpf)), ("lpf", s.lpf.as_ref())] {
            if let Some(f) = f {
                c.range(&ctx, name, &f.freq_hz);
                if !f.freq_hz.contains(f.start_hz) {
                    c.err(&ctx, format!("{name}.start_hz {} is outside freq_hz [{}, {}]", f.start_hz, f.freq_hz.lo(), f.freq_hz.hi()));
                }
            }
        }
        for ch in &s.character {
            c.range(&ctx, &format!("character.{}", ch.name), &ch.freq_hz);
        }
        c.unique(&ctx, "problem", s.problems.iter().map(|p| p.id.as_str()));
        for p in &s.problems {
            let pctx = format!("{ctx}.{}", p.id);
            c.range(&pctx, "freq_hz", &p.freq_hz);
            if p.when.is_empty() && p.when_any.is_empty() {
                c.err(&pctx, "problem has no conditions");
            }
            c.conditions(&pctx, &p.when);
            c.conditions(&pctx, &p.when_any);
            c.action(&pctx, &p.action);
        }
        let none_or = |v: &str, set: &HashSet<&str>| v == "none" || set.contains(v);
        if !none_or(&s.gate_preset, &c.gate_ids) {
            c.err(&ctx, format!("gate_preset `{}` not found in dynamics.toml", s.gate_preset));
        }
        if !none_or(&s.dyn_preset, &c.comp_ids) {
            c.err(&ctx, format!("dyn_preset `{}` not found in dynamics.toml [[comp_preset]]", s.dyn_preset));
        }
        if let Some(d) = &s.deess_preset {
            if !none_or(d, &c.deess_ids) {
                c.err(&ctx, format!("deess_preset `{d}` not found in dynamics.toml"));
            }
        }
        if !none_or(&s.fx_preset, &c.send_ids) {
            c.err(&ctx, format!("fx_preset `{}` not found in fx.toml [[send_preset]]", s.fx_preset));
        }
        c.refs(&ctx, &s.sources);
    }

    // ---- dynamics.toml
    let d = &kb.dynamics;
    c.unique("dynamics", "gate_preset", d.gate_preset.iter().map(|p| p.id.as_str()));
    c.unique("dynamics", "comp_preset", d.comp_preset.iter().map(|p| p.id.as_str()));
    c.unique("dynamics", "deess_preset", d.deess_preset.iter().map(|p| p.id.as_str()));
    c.unique("dynamics", "rule", d.rule.iter().map(|p| p.id.as_str()));
    let stat = |c: &mut Ctx, ctx: &str, s: &str| {
        if !THRESHOLD_STATS.contains(&s) {
            c.err(ctx, format!("threshold statistic `{s}` must be one of {THRESHOLD_STATS:?}"));
        }
    };
    for p in &d.gate_preset {
        let ctx = format!("dynamics:{}", p.id);
        if p.kind != "gate" && p.kind != "expander" {
            c.err(&ctx, format!("gate kind `{}` must be gate or expander", p.kind));
        }
        stat(&mut c, &ctx, &p.threshold_rel);
        if let Some(tc) = &p.threshold_clamp {
            stat(&mut c, &ctx, &tc.max_rel);
        }
        c.range(&ctx, "threshold_offset_db", &p.threshold_offset_db);
        for (n, r) in [
            ("range_db", &p.range_db),
            ("attack_ms", &p.attack_ms),
            ("hold_ms", &p.hold_ms),
            ("release_ms", &p.release_ms),
            ("expander_ratio", &p.expander_ratio),
            ("sidechain_hpf_hz", &p.sidechain_hpf_hz),
            ("sidechain_lpf_hz", &p.sidechain_lpf_hz),
        ] {
            c.opt_range(&ctx, n, r);
        }
        if p.kind == "expander" && p.expander_ratio.is_none() {
            c.err(&ctx, "expander preset needs expander_ratio");
        }
        if let (Some(h), Some(l)) = (&p.sidechain_hpf_hz, &p.sidechain_lpf_hz) {
            if h.hi() >= l.lo() {
                c.err(&ctx, "sidechain high-pass range must lie below the low-pass range");
            }
        }
        c.refs(&ctx, &p.sources);
    }
    for p in &d.comp_preset {
        let ctx = format!("dynamics:{}", p.id);
        if p.kind != "compressor" && p.kind != "limiter" {
            c.err(&ctx, format!("comp kind `{}` must be compressor or limiter", p.kind));
        }
        stat(&mut c, &ctx, &p.threshold_rel);
        if let Some(det) = &p.detector {
            if det != "peak" && det != "rms" {
                c.err(&ctx, format!("detector `{det}` must be peak or rms"));
            }
        }
        if let Some(m) = &p.makeup {
            if m != "loudness_match" && m != "none" {
                c.err(&ctx, format!("makeup `{m}` must be loudness_match or none"));
            }
        }
        c.range(&ctx, "threshold_offset_db", &p.threshold_offset_db);
        c.range(&ctx, "ratio", &p.ratio);
        c.range(&ctx, "attack_ms", &p.attack_ms);
        c.range(&ctx, "release_ms", &p.release_ms);
        for (n, r) in [("target_gr_db", &p.target_gr_db), ("knee_db", &p.knee_db), ("mix_pct", &p.mix_pct)] {
            c.opt_range(&ctx, n, r);
        }
        if p.ratio.lo() < 1.0 {
            c.err(&ctx, "ratio below 1:1");
        }
        if let Some(m) = &p.mix_pct {
            if !Range([0.0, 100.0]).contains_range(m) {
                c.err(&ctx, "mix_pct must lie within 0..100");
            }
        }
        c.refs(&ctx, &p.sources);
    }
    for p in &d.deess_preset {
        let ctx = format!("dynamics:{}", p.id);
        if p.kind != "deesser" {
            c.err(&ctx, format!("deess kind `{}` must be deesser", p.kind));
        }
        c.range(&ctx, "freq_hz", &p.freq_hz);
        c.range(&ctx, "range_db", &p.range_db);
        c.range(&ctx, "target_gr_db", &p.target_gr_db);
        c.refs(&ctx, &p.sources);
    }
    for r in &d.rule {
        c.rule("dynamics", r);
    }

    // ---- eq.toml
    let e = &kb.eq;
    for (n, q) in &e.q_class {
        c.range(&format!("eq:q_class.{n}"), "q", &q.q);
    }
    c.range("eq:limits", "preferred_boost_db", &e.limits.preferred_boost_db);
    c.range("eq:limits", "surface_gain_range_db", &e.limits.surface_gain_range_db);
    if e.limits.preferred_boost_db.hi() > e.limits.max_boost_db_peq {
        c.err("eq:limits", "preferred_boost_db exceeds max_boost_db_peq");
    }
    if e.limits.max_step_db > e.limits.max_total_boost_db_per_strip.max(e.limits.max_cut_db_peq) {
        c.err("eq:limits", "max_step_db exceeds the per-strip limits");
    }
    c.refs("eq:limits", &e.limits.sources);
    c.refs("eq:policy", &e.policy.sources);
    c.refs("eq:masking", &e.masking.sources);
    c.range("eq:masking", "kick_owns_hz", &e.masking.low_end_split.kick_owns_hz);
    c.range("eq:masking", "bass_owns_hz", &e.masking.low_end_split.bass_owns_hz);
    c.unique("eq:masking", "priority_order entry", e.masking.priority_order.iter().map(String::as_str));
    for id in &e.masking.priority_order {
        if !c.source_ids.contains(id.as_str()) {
            c.err("eq:masking", format!("priority_order: unknown source id `{id}`"));
        }
    }
    c.unique("eq", "rule", e.rule.iter().map(|p| p.id.as_str()));
    for r in &e.rule {
        c.rule("eq", r);
    }
    c.unique("eq", "masking_strategy", e.masking_strategy.iter().map(|p| p.id.as_str()));
    let mut ord = HashSet::new();
    for m in &e.masking_strategy {
        let ctx = format!("eq:{}", m.id);
        if !ord.insert(m.order) {
            c.err(&ctx, format!("duplicate masking_strategy order {}", m.order));
        }
        if m.when.is_empty() {
            c.err(&ctx, "strategy has no conditions");
        }
        c.conditions(&ctx, &m.when);
        if m.actions.is_empty() {
            c.err(&ctx, "strategy has no actions");
        }
        for a in &m.actions {
            c.action(&ctx, a);
        }
        if m.cost <= 0.0 {
            c.err(&ctx, "cost must be positive");
        }
        c.refs(&ctx, &m.sources);
    }

    // ---- fx.toml
    let f = &kb.fx;
    c.refs("fx:tempo", &f.tempo.sources);
    if f.tempo.fallback_bpm <= 0.0 {
        c.err("fx:tempo", "fallback_bpm must be positive");
    }
    c.refs("fx:context", &f.context.sources);
    for (n, m) in [
        ("live_reverberant_venue", &f.context.live_reverberant_venue),
        ("live_dry_venue_or_outdoor", &f.context.live_dry_venue_or_outdoor),
        ("broadcast_or_recording_tap", &f.context.broadcast_or_recording_tap),
    ] {
        c.range(&format!("fx:context.{n}"), "wet_offset_lu", &m.wet_offset_lu);
    }
    c.unique("fx", "fx_bus", f.fx_bus.iter().map(|p| p.id.as_str()));
    c.unique("fx", "send_preset", f.send_preset.iter().map(|p| p.id.as_str()));
    c.unique("fx", "rule", f.rule.iter().map(|p| p.id.as_str()));
    let classes = &wf.blocks.get("FxSlot").map(|b| b.model_classes.clone()).unwrap_or_default();
    for b in &f.fx_bus {
        let ctx = format!("fx:{}", b.id);
        if !classes.contains(&b.model_class) {
            c.err(&ctx, format!("model_class `{}` is not in workflow.toml [blocks.FxSlot].model_classes", b.model_class));
        }
        for (n, r) in [
            ("decay_s", &b.decay_s),
            ("predelay_ms", &b.predelay_ms),
            ("time_ms", &b.time_ms),
            ("time_beats", &b.time_beats),
            ("feedback_pct", &b.feedback_pct),
            ("lo_cut_hz", &b.lo_cut_hz),
            ("hi_cut_hz", &b.hi_cut_hz),
        ] {
            c.opt_range(&ctx, n, r);
        }
        if let (Some(lo), Some(hi)) = (&b.lo_cut_hz, &b.hi_cut_hz) {
            if lo.hi() >= hi.lo() {
                c.err(&ctx, "lo_cut_hz range must lie below hi_cut_hz range");
            }
        }
        for a in &b.return_filter {
            c.action(&ctx, a);
        }
        c.refs(&ctx, &b.sources);
    }
    for p in &f.send_preset {
        let ctx = format!("fx:{}", p.id);
        if p.sends.is_empty() {
            c.err(&ctx, "send_preset has no sends");
        }
        for s in &p.sends {
            if !c.fx_ids.contains(s.fx.as_str()) {
                c.err(&ctx, format!("send to unknown fx_bus `{}`", s.fx));
            }
            c.range(&ctx, "wet_rel_dry_lu", &s.wet_rel_dry_lu);
            if let Some(t) = &s.tap {
                if t != "pre" && t != "post" {
                    c.err(&ctx, format!("tap `{t}` must be pre or post"));
                }
            }
        }
        c.refs(&ctx, &p.sources);
    }
    for r in &f.rule {
        c.rule("fx", r);
    }

    // ---- vocabulary.toml
    let v = &kb.vocabulary;
    c.unique("vocabulary", "term", v.term.iter().map(|t| t.id.as_str()));
    let term_ids: HashSet<&str> = v.term.iter().map(|t| t.id.as_str()).collect();
    let mut words: std::collections::HashMap<String, &str> = std::collections::HashMap::new();
    for t in &v.term {
        for w in std::iter::once(&t.id).chain(t.synonyms.iter()) {
            let norm = crate::normalize_word(w);
            if norm != *w && norm.is_empty() {
                c.err(&format!("vocabulary:{}", t.id), format!("empty word `{w}`"));
            }
            if let Some(prev) = words.insert(norm, t.id.as_str()) {
                if prev != t.id {
                    c.err(&format!("vocabulary:{}", t.id), format!("word `{w}` also resolves to term `{prev}`"));
                }
            }
        }
    }
    for t in &v.term {
        let ctx = format!("vocabulary:{}", t.id);
        if t.id != t.id.to_lowercase() || t.synonyms.iter().any(|s| *s != s.to_lowercase()) {
            c.err(&ctx, "id and synonyms must be lowercase");
        }
        if let Some(a) = t.antonym_id() {
            if !term_ids.contains(a) {
                c.err(&ctx, format!("antonym `{a}` is not a term"));
            }
            if a == t.id {
                c.err(&ctx, "term is its own antonym");
            }
        }
        for x in &t.conflicts {
            if !term_ids.contains(x.as_str()) {
                c.err(&ctx, format!("conflicts: `{x}` is not a term"));
            }
        }
        if t.default_scope != "mix" && !c.is_scope(&t.default_scope) {
            c.err(&ctx, format!("default_scope `{}` is not mix, a source id or a family", t.default_scope));
        }
        for b in &t.blocks_hint {
            if !wf.blocks.contains_key(b) {
                c.err(&ctx, format!("blocks_hint: unknown block `{b}`"));
            }
        }
        if t.deltas.is_empty() && t.note.is_none() {
            c.err(&ctx, "term has no deltas (and no note explaining why)");
        }
        for d in &t.deltas {
            c.feature(&ctx, &d.feature, true);
            c.range(&ctx, &d.feature, &d.delta);
        }
        c.refs(&ctx, &t.sources);
    }
    if !v.modifiers.intensity.contains_key("") {
        c.err("vocabulary:modifiers", "intensity needs a \"\" entry (the unmodified default)");
    }
    for (w, x) in &v.modifiers.intensity {
        if *x <= 0.0 {
            c.err("vocabulary:modifiers", format!("intensity `{w}` must be positive"));
        }
    }
    if v.modifiers.negation.factor >= 0.0 {
        c.err("vocabulary:modifiers", "negation factor must be negative");
    }
    let l = &v.limits;
    for (n, x) in [
        ("max_band_delta_db", l.max_band_delta_db),
        ("max_level_delta_lu", l.max_level_delta_lu),
        ("max_width_delta_db", l.max_width_delta_db),
        ("max_reverb_delta_db", l.max_reverb_delta_db),
        ("max_crest_delta_db", l.max_crest_delta_db),
    ] {
        if x <= 0.0 {
            c.err("vocabulary:limits", format!("{n} must be positive"));
        }
    }

    // ---- all `sources` of the bibliography are used? (not an error), but keys must be non-empty titles
    for (k, r) in &wf.sources {
        if r.title.trim().is_empty() {
            c.err(&format!("workflow:sources.{k}"), "empty title");
        }
    }

    c.out
}

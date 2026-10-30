//! Control protocol messages (docs/control-protocol.md).
//!
//! Requests are one JSON object per line with a `cmd` field (`{"cmd":"set_mode","mode":"suggest"}`) and an optional
//! `id` that is echoed in the reply. A bare word (`ping`, `status`, `STOP`, `PANIC`) is accepted as `{"cmd":word}`.
//! Every request gets exactly one reply line: `{"ok":true,"id":..,"result":..}` or `{"ok":false,"id":..,"error":".."}`.
//! After a successful `subscribe` the connection carries event lines (`{"source":..,"type":..,...}`) until closed.

use std::collections::BTreeMap;

use mix_engine::control::Mode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

fn yes() -> bool {
    true
}

/// A control request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Liveness: `{"name":"wing-mixd","version":..,"pid":..}`.
    Ping,
    /// The status document (same as status.json).
    Status,
    /// Audio input devices (cpal) plus the embedded sim's taps when the sim is connected.
    ListDevices,
    /// Consoles: the embedded sim and WINGs found by UDP discovery.
    ListConsoles {
        /// Broadcast `WING?` on the LAN (port 2222).
        #[serde(default = "yes")]
        discover: bool,
        #[serde(default)]
        wait_ms: Option<u64>,
        /// Unicast probe of one address (`ip` or `ip:port`) instead of / besides the broadcast.
        #[serde(default)]
        probe: Option<String>,
        /// List a wing-sim running on this machine (from its `status.json`; M065).
        #[serde(default = "yes")]
        local: bool,
    },
    ConnectConsole(ConnectConsole),
    DisconnectConsole,
    SelectInput(SelectInput),
    LoadReference(LoadReference),
    LoadDescription(LoadDescription),
    /// Role detection per strip, with the confirmed roles of this console + scene.
    ListRoles,
    /// Confirm strip roles (`{"bus.5":"monitor"}`); persisted per console + scene. One request = one batch: a
    /// `null` role clears the confirmation (back to detection); the reply's `previous` undoes the whole batch.
    ConfirmRoles {
        roles: BTreeMap<String, Option<String>>,
        #[serde(default)]
        scene: Option<String>,
    },
    SetMode {
        mode: Mode,
    },
    /// Live-context target setting (M037): `live` (default), `broadcast_video`, `match_record`.
    SetLiveContext {
        context: LiveContext,
    },
    /// Start (or restart after `stop`) the closed loop.
    Start(StartOptions),
    /// End the loop: the controller stops ticking and the actuator freezes; undo/PANIC still work.
    Stop,
    /// STOP: freeze every move now (the loop pauses until `resume`). Bypasses the engine queue.
    #[serde(alias = "STOP", alias = "estop")]
    EmergencyStop,
    /// PANIC: ramp every touched param back to its session baseline, then stay stopped; mode drops to observe.
    #[serde(alias = "PANIC")]
    Panic,
    /// Lift a STOP / safety trip.
    Resume,
    /// Give a remit confirmation for pending moves.
    Confirm {
        key: String,
    },
    /// Moves waiting for a confirmation.
    Pending,
    /// Ramp the last completed move back.
    Undo,
    /// Live state: spectra, loudness, problems, plan, move log, safety.
    Snapshot,
    /// Strip names of the connected console (`{"strips":[{"id","kind","name"}]}`, unnamed strips left out).
    Strips,
    /// Read console params (`input.1/fader`).
    Params {
        ids: Vec<String>,
    },
    /// Mix slots (M064): store the managed console state (+ the active target) as slot `name` (default: the next
    /// free letter A, B, C...; an existing name is overwritten, except `Original`).
    SlotStore {
        #[serde(default)]
        name: Option<String>,
    },
    /// Slots of this console + scene, with what differs from the console now.
    SlotList,
    /// Snap the console to slot `name` (one batch, not ramped) and make its target the active one. Pauses the loop
    /// (comparison) until `slot_commit` / `slot_resume`. `keep_engineer` (default true): params the engineer moved
    /// since the slot was stored keep the engineer's value. `confirm_live`: needed once per connection on a real
    /// console (the audience hears the switch).
    SlotRecall {
        name: String,
        #[serde(default)]
        keep_engineer: Option<bool>,
        #[serde(default)]
        confirm_live: bool,
    },
    SlotRename {
        name: String,
        to: String,
    },
    SlotDelete {
        name: String,
    },
    /// End the comparison on slot `name` (default: the active one) as one undoable journal batch.
    SlotCommit {
        #[serde(default)]
        name: Option<String>,
    },
    /// Continue tuning from the current slot (commits a running comparison first); `mode` default autopilot.
    SlotResume {
        #[serde(default)]
        mode: Option<Mode>,
    },
    /// What differs between slot `a` and slot `b` (default `current`: the console now).
    SlotDiff {
        a: String,
        #[serde(default)]
        b: Option<String>,
    },
    /// Turn this connection into an event stream.
    Subscribe,
    /// Shut the daemon down.
    Quit,
}

impl Request {
    /// Parse one request line. Returns the request and the echo `id`.
    pub fn parse_line(line: &str) -> Result<(Request, Option<Value>), (String, Option<Value>)> {
        let line = line.trim();
        if line.is_empty() {
            return Err(("empty request".into(), None));
        }
        let mut v: Value = if line.starts_with('{') {
            serde_json::from_str(line).map_err(|e| (format!("bad json: {e}"), None))?
        } else if line.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            json!({ "cmd": line })
        } else {
            return Err((
                format!("expected a JSON object or a bare command, got {line:?}"),
                None,
            ));
        };
        let id = v.as_object_mut().and_then(|o| o.remove("id"));
        match serde_json::from_value::<Request>(v) {
            Ok(r) => Ok((r, id)),
            Err(e) => Err((e.to_string(), id)),
        }
    }

    /// Short name (`connect_console`).
    pub fn name(&self) -> String {
        serde_json::to_value(self)
            .ok()
            .and_then(|v| v.get("cmd").and_then(Value::as_str).map(str::to_owned))
            .unwrap_or_default()
    }
}

/// How strictly the target follows the reference's mastering-chain dimensions (M037).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveContext {
    /// A console mix played through a PA: relax PLR/width/sub/air, keep balance strict (default).
    #[default]
    Live,
    BroadcastVideo,
    /// Chase the reference as it is.
    MatchRecord,
}

impl LiveContext {
    /// The matching `mix_describe` context.
    pub fn mix_context(self) -> mix_describe::MixContext {
        match self {
            LiveContext::Live => mix_describe::MixContext::Live,
            LiveContext::BroadcastVideo => mix_describe::MixContext::BroadcastVideo,
            LiveContext::MatchRecord => mix_describe::MixContext::MatchRecord,
        }
    }
}

/// `connect_console`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ConnectConsole {
    /// `sim` (embedded console), `wing` (needs `ip`) or an IP address.
    pub console: String,
    #[serde(default)]
    pub ip: Option<String>,
    /// Native port (default 2222).
    #[serde(default)]
    pub port: Option<u16>,
    /// Scene name used as the key of persisted role confirmations (default `default`).
    #[serde(default)]
    pub scene: Option<String>,
    #[serde(default)]
    pub sim: Option<SimOptions>,
}

/// Embedded sim settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SimOptions {
    /// Multichannel WAV whose channel n feeds USB in n -> input n, or `synthetic` (mix-sim's 16-track song).
    #[serde(default)]
    pub multitrack: Option<String>,
    /// Strip names for input 1.. (default: the synthetic track names for `synthetic`).
    #[serde(default)]
    pub names: Option<Vec<String>>,
    /// Synthetic song seed / length (s).
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub seconds: Option<f64>,
    /// `factory` (default), `reference`, `degraded` (mix-sim scenes) or a `.snap` path.
    #[serde(default)]
    pub scene: Option<String>,
    /// Render speed: 1 = real time (default), N = N x real time, 0 = as fast as the analysis consumes.
    #[serde(default)]
    pub speed: Option<f64>,
}

/// Where the analysed audio comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputKind {
    /// An audio input device (cpal/CoreAudio).
    Device,
    /// A WAV file played in real time (loops).
    File,
    /// The embedded sim console's own taps.
    Sim,
    /// Raw interleaved little-endian f32 frames read from a TCP stream (`addr`, `channels`, `sample_rate`): a
    /// network audio bridge, e.g. a headless wing-sim's USB outputs in the end-to-end harness (M052).
    Stream,
}

/// One console tap carried by input channels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TapSel {
    /// `main`, `solo`, `input.N` (a post-fader strip tap) or `monitor:<name>` (feedback guard only).
    pub tap: String,
    /// 1-based device/file channels (1 or 2).
    pub channels: Vec<usize>,
}

/// `select_input`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectInput {
    pub source: InputKind,
    /// Device name (exact or unique substring; default input when omitted).
    #[serde(default)]
    pub device: Option<String>,
    /// WAV path for `file`.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sample_rate: Option<u32>,
    /// Taps (default: `main` on channels 1-2; sim: main, solo and every input).
    #[serde(default)]
    pub taps: Option<Vec<TapSel>>,
    /// `stream`: `host:port` the daemon connects to.
    #[serde(default)]
    pub addr: Option<String>,
    /// `stream`: channels per frame on the wire (default: the highest tap channel).
    #[serde(default)]
    pub channels: Option<usize>,
}

/// `load_reference`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadReference {
    /// Audio file (wav/flac/mp3/m4a...) or a TargetProfile `.json`.
    pub path: String,
    /// Section index the targets describe (default: whole song).
    #[serde(default)]
    pub section: Option<usize>,
    /// Per-stem targets via the optional separation sidecar.
    #[serde(default)]
    pub stems: bool,
    /// Reply only when loaded (default: reply at once, progress as events).
    #[serde(default)]
    pub wait: bool,
}

/// `load_description`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoadDescription {
    pub text: String,
    /// Base profile: `current` (the loaded target) or a TargetProfile `.json` path.
    #[serde(default)]
    pub base: Option<String>,
    #[serde(default)]
    pub genre: Option<String>,
    /// Let the Claude Code CLI interpret phrases the vocabulary does not know.
    #[serde(default)]
    pub llm: bool,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub wait: bool,
}

/// `start` options (all optional; live defaults from the knowledge base).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StartOptions {
    #[serde(default)]
    pub mode: Option<Mode>,
    /// Measurement window (s); live default 20.
    #[serde(default)]
    pub window_s: Option<f64>,
    /// Multiply settle/window/cool-downs (test clocks); applied before `window_s`.
    #[serde(default)]
    pub time_scale: Option<f64>,
    #[serde(default)]
    pub max_batches: Option<u32>,
    #[serde(default)]
    pub target_objective: Option<f64>,
    /// Follow the plan's solo inspections (default true; needs a `solo` tap).
    #[serde(default)]
    pub inspect: Option<bool>,
    /// Feedback guard (default on for WING, off for the sim: synthetic loops false-trip it, see M033).
    #[serde(default)]
    pub feedback_guard: Option<bool>,
    /// M062: measure strips without a tap through solo sweeps in autopilot (default true; needs a `solo` tap and
    /// `inspect`).
    #[serde(default)]
    pub sweep: Option<bool>,
    /// M062: estimate strips nobody has measured (mix-level fallback; default true).
    #[serde(default)]
    pub priors: Option<bool>,
}

/// Reply line.
pub fn reply(id: Option<Value>, result: Result<Value, String>) -> String {
    let mut o = serde_json::Map::new();
    match result {
        Ok(v) => {
            o.insert("ok".into(), Value::Bool(true));
            if let Some(id) = id {
                o.insert("id".into(), id);
            }
            o.insert("result".into(), v);
        }
        Err(e) => {
            o.insert("ok".into(), Value::Bool(false));
            if let Some(id) = id {
                o.insert("id".into(), id);
            }
            o.insert("error".into(), Value::String(e));
        }
    }
    Value::Object(o).to_string()
}

/// Parse a reply line into the result (`Err` for `ok: false` or garbage).
pub fn parse_reply(line: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(line.trim()).map_err(|e| format!("bad reply: {e}"))?;
    match v.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(v.get("result").cloned().unwrap_or(Value::Null)),
        Some(false) => Err(v
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("error")
            .to_string()),
        None => Err(format!("bad reply {line:?}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_json_and_bare_words() {
        assert_eq!(Request::parse_line("ping").unwrap().0, Request::Ping);
        assert_eq!(
            Request::parse_line("STOP").unwrap().0,
            Request::EmergencyStop
        );
        assert_eq!(Request::parse_line("PANIC").unwrap().0, Request::Panic);
        let (r, id) =
            Request::parse_line(r#"{"cmd":"set_mode","mode":"autopilot","id":7}"#).unwrap();
        assert_eq!(
            r,
            Request::SetMode {
                mode: Mode::Autopilot
            }
        );
        assert_eq!(id, Some(json!(7)));
        let (r, _) = Request::parse_line(
            r#"{"cmd":"connect_console","console":"sim","sim":{"multitrack":"synthetic","speed":0}}"#,
        )
        .unwrap();
        let Request::ConnectConsole(c) = r else {
            panic!()
        };
        assert_eq!(c.sim.unwrap().speed, Some(0.0));
        let (r, _) = Request::parse_line(
            r#"{"cmd":"select_input","source":"device","device":"WING","taps":[{"tap":"main","channels":[1,2]},{"tap":"solo","channels":[3,4]}]}"#,
        )
        .unwrap();
        assert!(matches!(r, Request::SelectInput(s) if s.taps.as_ref().map(Vec::len) == Some(2)));
        assert_eq!(
            Request::parse_line(r#"{"cmd":"start"}"#).unwrap().0,
            Request::Start(StartOptions::default())
        );
        let e = Request::parse_line(r#"{"cmd":"nope","id":"a"}"#).unwrap_err();
        assert_eq!(e.1, Some(json!("a")));
        assert!(Request::parse_line(r#"{"cmd":"set_mode","mode":"loud"}"#).is_err());
        assert!(Request::parse_line(r#"{"cmd":"set_mode","mode":"observe","x":1}"#).is_err());
        assert!(Request::parse_line("rm -rf /").is_err());
        assert!(Request::parse_line("").is_err());
        assert_eq!(Request::Undo.name(), "undo");
    }

    #[test]
    fn reply_round_trip() {
        let ok = reply(Some(json!(1)), Ok(json!({"a": 1})));
        assert_eq!(parse_reply(&ok), Ok(json!({"a": 1})));
        assert!(ok.contains("\"id\":1"));
        let err = reply(None, Err("nope\nline".into()));
        assert!(!err.contains('\n'));
        assert_eq!(parse_reply(&err), Err("nope\nline".into()));
    }
}

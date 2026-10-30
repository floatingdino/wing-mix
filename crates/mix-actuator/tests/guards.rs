//! Safety layer on FakeSurface: gain limits, envelopes, coupled caution, ceilings, feedback guard, analysis
//! watchdog, discrete changes, link loss, RTA restore, bounded logs.

mod common;

use common::*;
use mix_actuator::{
    AbortReason, ActuatorConfig, ActuatorEvent, InspectRequest, Job, PolicyGate, Refusal,
    RemitGate, SafetyConfig, SafetyGate, SafetyMonitor, Trip,
};
use mix_core::action::{Action, ParamChange};
use mix_core::features::{Features, Window};
use mix_core::ids::StripId;
use mix_core::value::Value;
use mix_surface::model::ParamClass;
use mix_surface::remit::{Decision, Envelope, InspectionKind, Reason, WriteRequest};
use mix_surface::surface::{MeterFrame, RtaSource, StripMeters};
use mix_surface::testkit::FakeSurface;
use mix_surface::Surface;
use std::sync::Arc;

fn mv(label: &str, id: &str, v: f64) -> Action {
    Action::new(label).with(ParamChange::new(id, v))
}

/// Rig whose gate is `SafetyGate(PolicyGate)` with a monitor attached to the actuator.
fn safe_rig(cfg: SafetyConfig) -> (Rig, SafetyMonitor) {
    let s = Arc::new(FakeSurface::demo());
    safe_rig_on(s, cfg, None)
}

/// `tweak` edits the inner decision (stand-in for policy outcomes the demo console does not produce).
type Tweak = Box<dyn Fn(&WriteRequest, &mut Decision) + Send>;

struct Tweaked {
    inner: PolicyGate,
    tweak: Tweak,
}

impl RemitGate for Tweaked {
    fn check_write(&self, req: &WriteRequest, base: &Value) -> Decision {
        let mut d = self.inner.check_write(req, base);
        (self.tweak)(req, &mut d);
        d
    }
    fn check_inspection(&self, k: InspectionKind, s: &StripId) -> Decision {
        self.inner.check_inspection(k, s)
    }
    fn refresh(&mut self, s: &dyn Surface) {
        self.inner.refresh(s)
    }
}

fn safe_rig_on(
    s: Arc<FakeSurface>,
    cfg: SafetyConfig,
    tweak: Option<Tweak>,
) -> (Rig, SafetyMonitor) {
    let monitor = SafetyMonitor::new(cfg);
    let pg = PolicyGate::new(policy(), &*s).unwrap();
    let inner: Box<dyn RemitGate> = match tweak {
        Some(tweak) => Box::new(Tweaked { inner: pg, tweak }),
        None => Box::new(pg),
    };
    let gate = SafetyGate::new(inner, s.clone(), monitor.clone());
    let mut rig = Rig::with_gate(s, Box::new(gate), ActuatorConfig::default());
    rig.a.set_safety(monitor.clone());
    (rig, monitor)
}

fn safety_refusal(r: &Rig, id: mix_actuator::MoveId) -> Option<String> {
    r.events().find_map(|e| match e {
        ActuatorEvent::Refused {
            id: i,
            reason:
                Refusal::Remit {
                    reason: Reason::Safety,
                    detail,
                    ..
                },
            ..
        } if *i == id => Some(detail.clone()),
        _ => None,
    })
}

fn refusal(r: &Rig, id: mix_actuator::MoveId) -> String {
    safety_refusal(r, id).unwrap_or_else(|| panic!("no safety refusal for {id}: {:#?}", r.log))
}

fn features(short_term: f32, momentary: f32, tp: f32) -> Features {
    let mut f = Features::new("main", Window::ShortTerm, true);
    f.loudness.short_term_lufs = short_term;
    f.loudness.momentary_lufs = momentary;
    f.loudness.true_peak_dbtp = tp;
    f
}

fn quiet() -> SafetyConfig {
    SafetyConfig::without_watchdog()
}

#[test]
fn per_move_and_per_session_boost_limits() {
    let (mut r, _m) = safe_rig(quiet());
    r.s.external_set(&p("input.2/fader"), f(-20.0)).unwrap();
    r.run_for(0.1);
    let big = r.a.submit(mv("big", "input.2/fader", -15.0)); // +5 in one move
    let ok1 = r.a.submit(mv("ok1", "input.2/fader", -17.0));
    let ok2 = r.a.submit(mv("ok2", "input.2/fader", -14.0)); // +6 over the session start
    let over = r.a.submit(mv("over", "input.2/fader", -13.0)); // +7
    r.run_idle(30.0);
    assert!(refusal(&r, big).contains("per-move"));
    assert!(r.done(ok1) && r.done(ok2));
    assert!(refusal(&r, over).contains("per-session"));
    assert_eq!(r.readf("input.2/fader"), -14.0);
    // cutting is never limited, and neither is moving back up to the session start or below
    let cut = r.a.submit(mv("cut", "input.2/fader", -28.0));
    let back = r.a.submit(mv("back", "input.2/fader", -20.0));
    r.run_idle(30.0);
    assert!(r.done(cut) && r.done(back), "{:#?}", r.log);
    assert_eq!(r.readf("input.2/fader"), -20.0);
}

#[test]
fn fader_ceiling_is_absolute() {
    let (mut r, _m) = safe_rig(quiet());
    r.s.external_set(&p("input.2/fader"), f(5.0)).unwrap();
    r.run_for(0.1);
    let id = r.a.submit(mv("up", "input.2/fader", 7.0));
    r.run_idle(5.0);
    assert!(refusal(&r, id).contains("ceiling"));
    assert_eq!(r.readf("input.2/fader"), 5.0);
}

#[test]
fn undo_restores_a_cut_even_above_the_per_move_limit() {
    let (mut r, _m) = safe_rig(quiet());
    r.s.external_set(&p("input.2/fader"), f(-20.0)).unwrap();
    r.run_for(0.1);
    r.a.submit(mv("up", "input.2/fader", -17.0));
    r.a.submit(mv("cut", "input.2/fader", -28.0));
    r.run_idle(30.0);
    r.a.submit(Job::Undo); // -28 -> -17 is +11 dB in one move, but it restores a written, in-limit value
    r.run_idle(30.0);
    assert_eq!(r.readf("input.2/fader"), -17.0);
}

#[test]
fn monitor_coupled_params_get_half_the_limits() {
    // input.* EQ reaches the IEM (pre send tapped after dyn): coupled, remit-allowed with a scaled envelope
    let tweak: Tweak = Box::new(|_, d| {
        d.coupled = true;
        d.envelope = None;
    });
    let (mut r, _m) = safe_rig_on(Arc::new(FakeSurface::demo()), quiet(), Some(tweak));
    let big = r.a.submit(mv("2 dB", "input.1/eq/band.1.gain", 2.0)); // limit 3 * 0.5 = 1.5
    let ok = r.a.submit(mv("1 dB", "input.1/eq/band.1.gain", 1.0));
    r.run_idle(10.0);
    assert!(refusal(&r, big).contains("monitor-coupled"));
    assert!(r.done(ok));
    // uncoupled: the same +2 dB is fine
    let tweak: Tweak = Box::new(|_, d| d.coupled = false);
    let (mut r2, _m) = safe_rig_on(Arc::new(FakeSurface::demo()), quiet(), Some(tweak));
    let id = r2.a.submit(mv("2 dB", "input.1/eq/band.1.gain", 2.0));
    r2.run_idle(10.0);
    assert!(r2.done(id), "{:#?}", r2.log);
}

#[test]
fn remit_envelope_is_enforced_from_the_session_baseline() {
    let tweak: Tweak = Box::new(|_, d| {
        d.envelope = Some(Envelope {
            max_boost_db: Some(2.0),
            max_cut_db: Some(4.0),
            ..Default::default()
        })
    });
    let cfg = SafetyConfig {
        eq_gain: mix_actuator::safety::GainLimits {
            max_boost_per_move_db: 10.0,
            max_boost_per_session_db: 10.0,
        },
        ..quiet()
    };
    let (mut r, _m) = safe_rig_on(Arc::new(FakeSurface::demo()), cfg, Some(tweak));
    let boost = r.a.submit(mv("boost 3", "input.1/eq/band.1.gain", 3.0));
    let cut = r.a.submit(mv("cut 5", "input.1/eq/band.1.gain", -5.0));
    let ok = r.a.submit(mv("cut 4", "input.1/eq/band.1.gain", -4.0));
    let back = r.a.submit(mv("back", "input.1/eq/band.1.gain", 0.0));
    r.run_idle(30.0);
    assert!(refusal(&r, boost).contains("envelope"));
    assert!(refusal(&r, cut).contains("envelope"));
    assert!(r.done(ok) && r.done(back));
}

#[test]
fn mains_ceiling_holds_increases_but_not_cuts() {
    let (mut r, m) = safe_rig(quiet());
    m.feed_mains(&features(-5.0, -4.0, -3.0));
    assert!(m.status().ceiling_hold);
    let up = r.a.submit(mv("up", "input.2/fader", 2.0));
    let down = r.a.submit(mv("down", "input.2/fader", -6.0));
    r.run_idle(10.0);
    assert!(refusal(&r, up).contains("ceiling"));
    assert!(r.done(down));
    assert!(r.events().any(|e| matches!(
        e,
        ActuatorEvent::Safety {
            trip: Trip::MainsLoudness { .. },
            reverted: None
        }
    )));
    // hysteresis: still held at -9.5 (ceiling -8, 1.5 dB), released at -12
    m.feed_mains(&features(-9.5, -9.5, -20.0));
    assert!(m.status().ceiling_hold);
    m.feed_mains(&features(-12.0, -12.0, -20.0));
    assert!(!m.status().ceiling_hold);
    let again = r.a.submit(mv("up", "input.2/fader", -3.0));
    r.run_idle(10.0);
    assert!(r.done(again));
}

#[test]
fn true_peak_trip_stops_and_reverts_the_last_move() {
    let (mut r, m) = safe_rig(quiet());
    let id = r.a.submit(mv("up", "input.2/fader", 2.0));
    r.run_idle(10.0);
    assert_eq!(r.readf("input.2/fader"), 2.0);
    m.feed_mains(&features(-14.0, -14.0, 0.5));
    r.run_for(3.0);
    let Some(ActuatorEvent::Safety { trip, reverted }) =
        r.find(|e| matches!(e, ActuatorEvent::Safety { .. }))
    else {
        panic!("{:#?}", r.log)
    };
    assert!(matches!(trip, Trip::MainsTruePeak { .. }));
    assert!(reverted.is_some());
    assert!(r.a.is_stopped());
    assert_eq!(r.readf("input.2/fader"), 0.0, "reverted {id}");
    assert!(r.find(|e| matches!(e, ActuatorEvent::Stopped)).is_some());
    // later jobs are refused until the operator resumes
    let later = r.a.submit(mv("later", "input.2/fader", -1.0));
    r.run_for(0.2);
    assert!(r.find(|e| matches!(e, ActuatorEvent::Refused { id, reason: Refusal::Stopped, .. } if *id == later)).is_some());
}

fn spectrum(peak_db: f32, band: usize) -> Vec<f32> {
    (0..31)
        .map(|b| if b == band { peak_db } else { -50.0 })
        .collect()
}

#[test]
fn feedback_buildup_stops_and_reverts_mid_move() {
    let (mut r, m) = safe_rig(quiet());
    // a +3 dB boost (0.5 s at 6 dB/s) of the vocal fader; the room starts to ring a moment later
    let id = r.a.submit(mv("vox up", "input.3/fader", 3.0));
    let mut tripped_at = None;
    for i in 0..400 {
        r.tick();
        if i % 3 == 0 {
            // ~120 ms frames; band 20 climbs ~1 dB per frame
            m.feed_spectrum("main", &spectrum(-50.0 + (i / 3) as f32, 20), &[]);
        }
        if tripped_at.is_none() && r.a.is_stopped() {
            tripped_at = Some(r.t);
        }
    }
    let t = tripped_at.expect("feedback guard never tripped");
    assert!(t > 1.5 && t < 6.0, "tripped at {t}");
    assert!(r.done(id), "{:#?}", r.log);
    let trip = r.events().find_map(|e| match e {
        ActuatorEvent::Safety {
            trip: Trip::Feedback { band, .. },
            reverted,
        } => Some((*band, *reverted)),
        _ => None,
    });
    let (band, reverted) = trip.expect("Safety event");
    assert_eq!(band, 20);
    assert!(reverted.is_some());
    assert_eq!(r.readf("input.3/fader"), 0.0, "last move reverted");
    assert!(m.status().feedback_latched);
    // while latched no boost is accepted even after resume until the guard is cleared by resume itself
    r.a.resume();
    assert!(!m.status().feedback_latched);
    let again = r.a.submit(mv("again", "input.3/fader", 1.0));
    r.run_idle(5.0);
    assert!(r.done(again), "{:#?}", r.log);
}

#[test]
fn feedback_latch_blocks_boosts_but_not_cuts_and_old_moves_are_not_reverted() {
    let (mut r, m) = safe_rig(quiet());
    let id = r.a.submit(mv("old", "input.3/fader", 1.0));
    r.run_idle(5.0);
    assert!(r.done(id));
    r.run_for(40.0); // longer than revert_window_s
    for i in 0..30 {
        m.feed_spectrum_at(
            "main",
            i as f64 * 0.1 + r.t,
            &spectrum(-50.0 + i as f32, 8),
            &[],
        );
    }
    r.run_for(0.2);
    let rev = r.events().find_map(|e| match e {
        ActuatorEvent::Safety { reverted, .. } => Some(*reverted),
        _ => None,
    });
    assert_eq!(rev, Some(None), "{:#?}", r.log);
    assert_eq!(r.readf("input.3/fader"), 1.0);
    assert!(r.a.is_stopped());
}

#[test]
fn suspect_state_blocks_coupled_boosts_only() {
    let tweak: Tweak = Box::new(|req, d| d.coupled = req.param.as_str().starts_with("input.1/"));
    let (mut r, m) = safe_rig_on(Arc::new(FakeSurface::demo()), quiet(), Some(tweak));
    // peak rises 6 dB over 2.0 s: past half the trip rise (suspect), short of the trip
    for i in 0..21 {
        m.feed_spectrum_at(
            "main",
            i as f64 * 0.1,
            &spectrum(-40.0 + i as f32 * 0.3, 14),
            &[],
        );
    }
    // prominence is 10 dB at the end, half of 12 -> suspect but not tripped
    let st = m.status();
    assert!(st.feedback_suspect && !st.feedback_latched, "{st:?}");
    let coupled = r.a.submit(mv("coupled", "input.1/eq/band.1.gain", 1.0));
    let free = r.a.submit(mv("free", "input.2/eq/band.1.gain", 1.0));
    r.run_idle(10.0);
    assert!(refusal(&r, coupled).contains("monitor mix"));
    assert!(r.done(free));
}

#[test]
fn analysis_stall_holds_boosts_then_stops() {
    let (mut r, m) = safe_rig(SafetyConfig::default());
    // long boost; analysis never reports
    let id = r.a.submit(mv("slow up", "input.2/fader", 3.0));
    r.run_for(0.5);
    m.heartbeat();
    r.run_for(2.5);
    // stall hold at 2 s without analysis: the running boost is aborted by the gate
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Aborted { id: i, reason: AbortReason::Remit { reason: Reason::Safety, .. }, .. } if *i == id))
        .is_some()
        || r.done(id));
    assert!(r
        .find(|e| matches!(
            e,
            ActuatorEvent::Safety {
                trip: Trip::AnalysisStalled { .. },
                ..
            }
        ))
        .is_some());
    let boost = r.a.submit(mv("boost", "input.2/fader", 4.0));
    r.run_for(0.5);
    assert!(refusal(&r, boost).contains("stalled"));
    // cuts still work during the stall
    let cut = r.a.submit(mv("cut", "input.2/fader", -6.0));
    r.run_idle(10.0);
    assert!(r.done(cut));
    // 10 s without analysis: STOP
    r.run_for(10.0);
    assert!(r.a.is_stopped());
    assert!(r
        .find(|e| matches!(
            e,
            ActuatorEvent::Safety {
                trip: Trip::AnalysisStopped { .. },
                ..
            }
        ))
        .is_some());
    // analysis comes back: hold released; resume lifts the STOP
    m.heartbeat();
    r.a.resume();
    let ok = r.a.submit(mv("ok", "input.2/fader", -5.0));
    r.run_for(0.5);
    m.heartbeat();
    r.run_idle(10.0);
    assert!(r.done(ok), "{:#?}", r.log);
}

fn meters(strip: &str, db: f32) -> MeterFrame {
    let mut m = StripMeters::silent(StripId::parse(strip).unwrap());
    m.output_db = [db, db];
    MeterFrame {
        strips: vec![m],
        ..Default::default()
    }
}

#[test]
fn discrete_changes_only_when_the_strip_is_inaudible() {
    let tweak: Tweak = Box::new(|req, d| {
        if req.class == ParamClass::Polarity {
            d.discrete = true;
            d.class = mix_surface::remit::Verdict::Allowed;
            d.reason = Reason::Rule;
        }
    });
    let (mut r, _m) = safe_rig_on(Arc::new(FakeSurface::demo()), quiet(), Some(tweak));
    let flip = |r: &mut Rig| {
        r.a.submit(Action::new("polarity").with(ParamChange::new("input.1/in/polarity", true)))
    };
    // no meter for the strip: fail closed
    r.s.set_meters(MeterFrame::default());
    let a = flip(&mut r);
    r.run_idle(2.0);
    assert!(refusal(&r, a).contains("no meter"));
    r.s.set_meters(meters("input.1", -20.0));
    let b = flip(&mut r);
    r.run_idle(2.0);
    assert!(refusal(&r, b).contains("audible"));
    r.s.set_meters(meters("input.1", -90.0));
    let c = flip(&mut r);
    r.run_idle(2.0);
    assert!(r.done(c));
    assert_eq!(r.read("input.1/in/polarity"), Value::Bool(true));
}

#[test]
fn link_loss_freezes_drops_the_queue_and_never_replays() {
    let (mut r, _m) = safe_rig(quiet());
    let a = r.a.submit(mv("long", "input.2/fader", -9.0));
    let b = r.a.submit(mv("queued", "input.3/fader", -9.0));
    r.run_for(0.6);
    let frozen = r.readf("input.2/fader");
    assert!(frozen < 0.0 && frozen > -9.0, "{frozen}");
    r.s.set_connected(false);
    r.run_for(0.2);
    assert!(r.a.is_link_down() && r.a.is_stopped());
    assert!(r.find(|e| matches!(e, ActuatorEvent::Aborted { id, reason: AbortReason::Disconnected, .. } if *id == a)).is_some());
    assert!(r.find(|e| matches!(e, ActuatorEvent::Aborted { id, reason: AbortReason::Disconnected, .. } if *id == b)).is_some());
    let c = r.a.submit(mv("while down", "input.2/fader", -1.0));
    r.run_for(0.2);
    assert!(r.find(|e| matches!(e, ActuatorEvent::Refused { id, reason: Refusal::Disconnected, .. } if *id == c)).is_some());
    r.a.resume(); // nothing to resume while down
    assert!(r.a.is_stopped());
    // the engineer moved a param meanwhile
    r.s.external_set(&p("input.2/fader"), f(-3.0)).unwrap();
    let writes_before = r.s.writes().len();
    r.s.set_connected(true);
    r.run_for(1.0);
    let Some(ActuatorEvent::LinkRestored { drifted }) =
        r.find(|e| matches!(e, ActuatorEvent::LinkRestored { .. }))
    else {
        panic!("{:#?}", r.log)
    };
    assert_eq!(drifted, &vec![p("input.2/fader")]);
    assert!(!r.a.is_link_down() && !r.a.is_stopped());
    assert_eq!(
        r.s.writes().len(),
        writes_before,
        "no stale ramp is replayed"
    );
    assert_eq!(r.readf("input.2/fader"), -3.0, "engineer's value wins");
    assert!(r.a.is_idle());
    // and the actuator works again (the drifted param is yielded for its cool-down first)
    let d = r.a.submit(mv("after", "input.3/fader", -2.0));
    r.run_idle(10.0);
    assert!(r.done(d));
}

#[test]
fn user_stop_survives_a_reconnect() {
    let (mut r, _m) = safe_rig(quiet());
    r.a.stop();
    r.s.set_connected(false);
    r.run_for(0.2);
    r.s.set_connected(true);
    r.run_for(0.2);
    assert!(
        r.a.is_stopped(),
        "a deliberate STOP is not lifted by the reconnect"
    );
    assert!(!r.a.is_link_down());
}

#[test]
fn rta_source_is_saved_and_restored() {
    let (mut r, _m) = safe_rig(quiet());
    let prior = RtaSource {
        strip: StripId::parse("main.1").unwrap(),
        tap: "POST".into(),
    };
    let ours = RtaSource {
        strip: StripId::parse("input.3").unwrap(),
        tap: "PREEQ".into(),
    };
    r.s.set_rta_source(Some(&prior)).unwrap();
    let seen = Arc::new(std::sync::Mutex::new(None));
    let seen2 = seen.clone();
    let req = InspectRequest::strip(
        StripId::parse("input.3").unwrap(),
        std::time::Duration::from_millis(400),
    )
    .with_rta(ours.clone())
    .measure(move |v| {
        *seen2.lock().unwrap() = v.surface.rta_source().unwrap();
        Vec::new()
    });
    let id = r.a.submit(Job::Inspect(req));
    r.run_idle(5.0);
    assert!(r.done(id));
    assert_eq!(
        *seen.lock().unwrap(),
        Some(ours.clone()),
        "RTA pointed at the strip while inspecting"
    );
    assert_eq!(r.s.rta_source().unwrap(), Some(prior.clone()));
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::RtaRestored { exact: true, .. }))
        .is_some());

    // STOP mid-inspection restores it too
    let req = InspectRequest::strip(
        StripId::parse("input.3").unwrap(),
        std::time::Duration::from_secs(5),
    )
    .with_rta(ours.clone());
    r.a.submit(Job::Inspect(req));
    r.run_for(0.5);
    assert_eq!(r.s.rta_source().unwrap(), Some(ours.clone()));
    r.a.stop();
    r.run_for(0.1);
    assert_eq!(r.s.rta_source().unwrap(), Some(prior.clone()));

    // the engineer picks another source meanwhile: left alone
    r.a.resume();
    let other = RtaSource {
        strip: StripId::parse("input.1").unwrap(),
        tap: "POST".into(),
    };
    let req = InspectRequest::strip(
        StripId::parse("input.3").unwrap(),
        std::time::Duration::from_millis(400),
    )
    .with_rta(ours);
    r.a.submit(Job::Inspect(req));
    r.run_for(0.2);
    r.s.set_rta_source(Some(&other)).unwrap();
    r.run_idle(5.0);
    assert_eq!(r.s.rta_source().unwrap(), Some(other));
}

#[test]
fn write_log_and_undo_history_are_bounded() {
    let s = Arc::new(FakeSurface::demo());
    let cfg = ActuatorConfig {
        write_log_cap: 40,
        history_cap: 3,
        ..ActuatorConfig::default()
    };
    let gate = PolicyGate::new(policy(), &*s).unwrap();
    let mut r = Rig::with_gate(s, Box::new(gate), cfg);
    for i in 0..12 {
        r.a.submit(mv(
            "wiggle",
            "input.2/fader",
            if i % 2 == 0 { -6.0 } else { 0.0 },
        ));
    }
    r.run_idle(60.0);
    let total = r.s.writes().len() as u64;
    assert!(total > 100);
    assert!(r.a.write_log().len() <= 40);
    assert_eq!(
        r.a.write_log().len() as u64 + r.a.write_log_dropped(),
        total
    );
    // the newest record is the last write
    assert_eq!(
        r.a.write_log().last().unwrap().value,
        r.read("input.2/fader")
    );
    // only the last 3 moves can be undone
    for _ in 0..5 {
        r.a.submit(Job::Undo);
    }
    r.run_idle(60.0);
    let nothing = r
        .events()
        .filter(|e| {
            matches!(
                e,
                ActuatorEvent::Refused {
                    reason: Refusal::NothingToUndo,
                    ..
                }
            )
        })
        .count();
    assert_eq!(nothing, 2);
}

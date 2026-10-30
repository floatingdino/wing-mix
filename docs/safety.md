# Safety layer (M041)

Hard limits that hold **whatever the planner asks for**. Code: `crates/mix-actuator/src/safety.rs` (`SafetyGate`,
`SafetyMonitor`, `SafetyConfig`) plus hooks in `actuator.rs`. Defaults below are `SafetyConfig::default()`.
The remit (docs/research/remit.md) says what may be touched at all; this layer limits *how much, how fast and when*.

## Wiring

```rust
let monitor = SafetyMonitor::new(SafetyConfig::default());
let gate = SafetyGate::new(Box::new(PolicyGate::new(policy, &*surface)?), surface.clone(), monitor.clone());
let mut actuator = Actuator::new(surface, Box::new(gate), ActuatorConfig::default());
actuator.set_safety(monitor.clone());
// analysis thread: monitor.feed_mains(&features) every snapshot; monitor.feed_features(&tap_features) for
// monitor-bus taps; monitor.feed_spectrum("rta", &bands_db, &centres_hz) for the console RTA.
```

`SafetyGate` wraps the remit gate. It is asked before every planned move and again before **every write**, so a limit
that becomes active mid-ramp (ceiling, feedback, stall) aborts the move in place. It can only tighten the remit's
decision. Refusals show up as `Refusal::Remit` / `AbortReason::Remit` with `reason: safety` and a `safety: ...` detail.
Decreases are never held back: undo, PANIC and the automatic revert always run (they go through `check_restore`, where
only the holds below apply, not the size limits).

## Limits

| Guard | Default | Behaviour |
|---|---|---|
| Boost per move (fader, send, trim/preamp, EQ gain) | +3 dB each | A move that raises the value by more than this is refused. Checked at plan time (value now to target). |
| Boost per session (same kinds) | +6 dB above the value the session first touched | Refused if the target ends higher than baseline + limit. Baseline = `Actuator::baseline` (captured before the first write). |
| Fader ceiling | +6 dB absolute | Faders and sends are never raised above it. |
| Remit envelope | per policy (`max_delta_db`, `max_boost_db`, `max_cut_db`, `max_delta_oct`, `max_delta_pct`) | Enforced from the session baseline (the remit evaluator only reports it). A write that moves the param back toward its baseline is never refused. `max_delta_pct` applies to percent and pan params (M039: pans were not checked before; a stereo-pair side gets `input-pair-spread`'s +-100 on its own side, mono sources +-50). |
| Monitor-coupled params | limits x 0.5 (`monitor_caution_scale`) | Params the remit marks `coupled` (EQ/dyn before an IEM send tap, FX bus re-input to IEMs) get half the per-move/session limits, and their boosts are also held while feedback is *suspected* (below). The policy's own `envelope_scale` applies on top. |
| Discrete changes (remit `discrete`: polarity, delay, models) | strip output below -60 dB on live meters | Refused when the strip is audible or has no meter (fails closed). Replaces fading the path, which would write params outside the move. |
| Mains loudness | short-term > -8 LUFS or momentary > -5 LUFS | **Hold**: every gain increase refused (running boosts abort) until both are 1.5 dB below the ceiling. Event `Safety{MainsLoudness}`. |
| Mains true peak | > -1 dBTP | Hold as above **and** STOP + ramped revert of the last move. |
| Feedback guard | see below | STOP + revert last move; latched until `resume`. |
| Analysis stall | no `feed_*`/`heartbeat` for 2 s | **Hold** (increases refused, running boosts abort). At 10 s: STOP (no revert). A fresh feed releases the hold; `resume` lifts the STOP. Disable with `SafetyConfig::without_watchdog()` only where there is no live analysis. |

Holds block gain *increases* only (value goes up by more than the param tolerance): cuts, filters, moves toward the
baseline and non-gain params continue. While a hold is active the operator can still cut.

## Feedback-risk guard

Input: third-octave frames from `Features` (main mix, or any tap via `feed_features`) or console RTA bands. Per
source it keeps 4 s of frames. For each band it computes **prominence** = level minus the **power** mean of the four
neighbouring bands (peak-to-neighbouring-power ratio, PNPR; M039: it was a dB mean, so one neighbour falling silent,
e.g. a bass line moving from 50 to 80 Hz, made the new note look 12 dB prominent and stopped a real-song closed loop;
flat neighbours give the same value as before), and trips when all hold over the window:

- prominence now >= 12 dB, and band level >= -70 dB;
- prominence rose >= 9 dB over the window (span >= 1.5 s);
- the rise is **gradual**: no frame-to-frame step steeper than 15 dB/s (+1 dB), and >= 80% of steps do not fall.

M036 (music vs feedback; M033 saw the guard trip on mix-sim's synthetic song at 200 Hz-1 kHz within 20-40 s), all
must also hold:

- the band is within `relative_floor_db` (30 dB) of the frame's loudest band;
- the band's **own level** rose by at least `level_rise_share` (0.6) of the prominence rise, with >= 80 % of its
  steps not falling (music gains prominence when the neighbouring bands fall away; feedback grows);
- it is **still rising**: +`recent_rise_db` (1.5 dB) or more over the last `recent_s` (1 s) (a swelled note
  plateaus; feedback keeps building);
- **no harmonic partner** (`reject_harmonic`): no other band at 1/3, 1/2, 2, 3 or 4 times its frequency
  (third-octave offsets -5, -3, +3, +5, +6) is itself narrowband (>= the suspect prominence) within 25 dB of its
  level. A note has partials; incipient feedback is a sine.

A note onset is a step (rejected), a mix swelling overall has no prominence, a held tone has no rise. A slow
single-note swell of a pure tone can look like feedback: that error is on the safe side (STOP + revert, `resume` to
continue). At half the thresholds the state is **suspect**: coupled boosts are held, nothing else changes.
Validation (`crates/mix-engine/tests/feedback_music.rs`): 0 trips on the synthetic song (reference and degraded,
40 s each) and on all 35 played sections of the three matched songs as recorded at FOH (785 s; the M041 detector
tripped 5 times there and on the synthetic song); the loop harness runs with the guard on; every M041 trip test
(`sim_feedback`, `guards`, unit tests) passes unchanged. M039 unit test `bass_note_change_next_to_a_silent_band_does_not_trip`
replays the frames of the holy-forever trip (and checks the same rise as a pure tone still trips).

On a trip the actuator: emits `ActuatorEvent::Safety{trip, reverted}`, STOPs (queue dropped, running move ends in
place), and if the last completed or running move finished within `revert_window_s` (20 s) ramps it back at
`restore_speed` (undo; params the engineer moved since are left alone). The latch (and the spectrum history) clear on
`resume`. A hold of every boost stays in force until then.

## Planning within the limits (M036)

The planner (`mix_engine::plan`, `PlanConfig::safety`, default `SafetyConfig::default()`) plans every gain increase
within these limits instead of proposing moves the gate refuses: a param's total planned value stays under
`SafetyConfig::session_raise_to` (session baseline + per-session limit, fader ceiling; the controller tracks the
baseline as the value before its first change), a move whose boost exceeds `move_limit_db` is split into
consecutive moves labelled `(k/n)`, remit envelopes are clamped from the session baseline exactly as the gate
measures them (max boost/cut on every dB param, octaves on Hz/Q/ms/s/ratio params), and no move has more changes
than the actuator's `max_batch`. When a source is too quiet and its faders are at the limit, the planner offers
cutting the loudest other strips instead. What the plan cannot know stays a runtime refusal: holds (feedback,
mains ceiling, stall) and discrete changes on an audible strip (model switches wait for live meters).

## Connection loss

`SurfaceEvent::Disconnected` (or a write failing with `Disconnected`): the running move ends in place
(`AbortReason::Disconnected`), the queue is dropped, new jobs are refused (`Refusal::Disconnected`), and `resume` does
nothing. **Stale ramps are never replayed.** An inspection's solo/RTA restore that could not be sent is retried on
reconnect. On `Reconnected` the actuator re-reads every param it wrote: values that differ from the last written value
were changed on the console meanwhile; they are yielded to the engineer (cool-down, journal note, listed in
`LinkRestored{drifted}`) and never overwritten. If the link loss was the only reason for the STOP it is lifted
(`Ready`); a deliberate STOP/PANIC stays.

## Bounded state

`write_log` keeps the last `write_log_cap` (10 000) records, dropping the oldest quarter at a time
(`Actuator::write_log_dropped()` counts them); the undo stack keeps `history_cap` (256) moves. The full audit trail
is the event stream and the journal (M050 persists and rotates them). The `Journal` itself is unbounded.

## RTA source save/restore

`InspectRequest::with_rta(RtaSource)` points the console RTA at a strip/tap for the inspection and restores the prior
source afterwards (done, STOP, abort, reconnect), exactly like the solo state, and leaves it alone if the engineer
selected another source meanwhile (`RtaRestored{exact}`). The remit `[inspection] rta` rule is checked first.

## Tests

`cargo test -p mix-actuator`: `tests/guards.rs` (FakeSurface: every guard above, link loss, RTA, bounded logs) and
`tests/sim_feedback.rs` (SimSurface + the real streaming analyser: a 2.5 kHz sine building at 6 dB/s trips the guard,
reverts the boost and stops the actuator; a held tone / note onset does not). Unit tests for the detector are in
`safety.rs`.

## Not covered (follow-ups)

- Which tap an inspection measures (M021/M033). The feedback guard is only as good as the taps that are fed.
- A model change followed by PANIC reports the old model's params as conflicts (M040 follow-up 3).
- Thresholds are conservative defaults, not tuned on real rooms; tune against recordings (M060).

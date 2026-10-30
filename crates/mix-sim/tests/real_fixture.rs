//! The user's real multitrack (local only) patched into the simulator. Skips when the fixture folder is absent.

use mix_sim::real::{flat_scene, load_excerpt, load_window};
use mix_sim::scenes::console_with;
use mix_sim::{SimConfig, TapPoint};
use mix_surface::Surface;
use std::time::Instant;

/// Fast: 2 s of the full-band window, all 28 channels patched to their WING inputs, rendered to main 1.
#[test]
fn real_excerpt_renders_through_its_wing_channels() {
    let Some(ex) = load_window("full-band", 2.0) else {
        eprintln!("skipped: multitrack fixture not present");
        return;
    };
    let ex = ex.unwrap();
    assert_eq!(ex.tracks.len(), 28);
    assert!(ex.tracks.iter().any(|t| t.channel == 3 && t.label == "Kick"));
    let scene = flat_scene(&ex);
    let sim = console_with(&ex.multitrack, &scene, SimConfig::default()).unwrap();
    assert_eq!(sim.get_wing("ch/38/in/conn/grp").as_deref(), Some("USB"));
    let r = sim.render(2.0, &[TapPoint::Main(1), TapPoint::InputPost(3)]);
    let main = r.tap(TapPoint::Main(1)).unwrap();
    assert!(main.rms_db() > -50.0, "main {} dBFS", main.rms_db());
    assert!(r.tap(TapPoint::InputPost(3)).unwrap().peak() > 0.0, "kick channel silent");
    let m = sim.meters().unwrap();
    let kick = m.strips.iter().find(|s| s.strip.as_str() == "input.3").unwrap();
    assert!(kick.input_db[0] > -60.0, "kick input meter {:?}", kick.input_db);
    // a muted channel (ch 30 is muted in the project) produces no output
    let tx = m.strips.iter().find(|s| s.strip.as_str() == "input.30").unwrap();
    assert!(tx.output_db[0] <= -120.0, "muted ch 30 output {:?}", tx.output_db);
}

#[test]
#[ignore = "slow: run with make check-full"]
fn real_window_30s() {
    let Some(ex) = load_excerpt(3900.0, 30.0) else {
        eprintln!("skipped: multitrack fixture not present");
        return;
    };
    let ex = ex.unwrap();
    let sim = console_with(&ex.multitrack, &flat_scene(&ex), SimConfig::default()).unwrap();
    let t = Instant::now();
    let r = sim.render(30.0, &[TapPoint::Main(1)]);
    eprintln!("30 s x 28 ch rendered in {:.2} s", t.elapsed().as_secs_f64());
    assert!(r.tap(TapPoint::Main(1)).unwrap().rms_db() > -40.0);
}

//! probe
use std::sync::Arc;
fn _assert_send<T: Send>() {}
pub fn probe() {
    _assert_send::<wing_sim::audio::Console>();
    _assert_send::<wing_sim::audio::ConsoleControl>();
    _assert_send::<wing_model::ParamStore>();
    let _ = Arc::new(wing_engine::meters::LevelBank::new());
}

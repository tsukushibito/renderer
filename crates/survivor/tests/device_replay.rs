use sashiko::{app::Replay, input::PadState, math::Vec2};
use survivor::{Game, Input, Scenario, TICK_HZ, controls};

#[test]
fn recorded_analog_movement_replays_without_a_device() {
    let mut devices = controls::controls();
    devices.gamepad(Some(PadState {
        left: Vec2::new(0.59, 0.),
        ..Default::default()
    }));
    let action = controls::sample(&mut devices);
    let rate = TICK_HZ.try_into().unwrap();
    let bytes = serde_json::to_vec(&Replay::new(506_u64, rate, vec![action])).unwrap();
    let recording = Replay::<u64, Input>::read(bytes.as_slice(), || unreachable!(), rate).unwrap();
    let mut live = Game::new(Default::default(), Scenario::Early, 506);
    let mut replay = Game::new(Default::default(), Scenario::Early, recording.initial);
    let x = live.player.x();
    live.step(action);
    replay.step(recording.inputs[0]);
    assert_eq!(live.snapshot(), replay.snapshot());
    assert!((live.player.x() - x - 0.0375).abs() < 0.0001);
    let legacy: Input = serde_json::from_str(r#"{"movement":[0.5,0]}"#).unwrap();
    replay.step(legacy);
    assert!((replay.player.x() - live.player.x() - 0.075).abs() < 0.0001);
}

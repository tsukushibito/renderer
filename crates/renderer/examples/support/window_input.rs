//! Window-independent application controls. Native events and smoke replay use
//! these same operations; replay never sends input to Windows or another app.

use super::window_camera::{Camera, Motion};
use std::time::Duration;

#[derive(Default)]
pub struct CameraInput {
    camera: Camera,
    looking: bool,
}
impl CameraInput {
    pub fn motion(&mut self, motion: Motion, pressed: bool) {
        self.camera.set_motion(motion, pressed);
    }
    pub fn fast(&mut self, pressed: bool) {
        self.camera.set_fast(pressed);
    }
    pub fn capture(&mut self, enabled: bool) {
        // Releasing RMB ends mouse look, while independently held keyboard
        // movement continues. Only focus/mode loss clears those held keys.
        self.looking = enabled;
    }
    pub fn focus_lost(&mut self) {
        self.looking = false;
        self.clear_motion();
    }
    pub fn clear_motion(&mut self) {
        self.camera.clear_input();
    }
    pub fn relative_motion(&mut self, dx: f64, dy: f64, free_camera: bool) {
        if self.looking && free_camera {
            self.camera.look(dx, dy);
        }
    }
    pub fn step(&mut self, elapsed: Duration) {
        self.camera.step(elapsed);
    }
    pub fn tour(&mut self, ordinal: u64) {
        self.camera.tour(ordinal);
    }
    pub fn position(&self) -> [f32; 3] {
        self.camera.position()
    }
    #[allow(dead_code, reason = "available to other example scenes")]
    pub fn set_pose(&mut self, position: [f32; 3], target: [f32; 3]) {
        self.looking = false;
        self.camera.set_pose(position, target);
    }
    #[allow(dead_code, reason = "available to other example scenes")]
    pub fn target(&self) -> [f32; 3] {
        self.camera.target()
    }
    pub fn clip_from_world(&self, width: u32, height: u32) -> [[f32; 4]; 4] {
        self.camera.clip_from_world(width, height)
    }
}

pub const REPLAY_STEPS: usize = 5;
pub const REPLAY_DT: Duration = Duration::from_nanos(33_333_333);

fn forward_step(before: [f32; 3], after: [f32; 3]) -> bool {
    let distance_squared: f32 = before
        .into_iter()
        .zip(after)
        .map(|(before, after)| (after - before).powi(2))
        .sum();
    (distance_squared.sqrt() - 0.1).abs() < 0.00001 && after[2] > before[2] && after[1] < before[1]
}

/// One transition per accepted Graph ordinal, independent of event-loop wakeups
/// and backpressure retries. Each pose is rendered before advancing to the next.
#[derive(Default)]
pub struct InputReplay {
    pub steps: usize,
    pub movement: bool,
    pub mouse_look: bool,
    pub release_preserves_movement: bool,
    pub focus_stops_movement: bool,
}
impl InputReplay {
    pub fn advance(&mut self, input: &mut CameraInput, ordinal: u64) -> Result<(), &'static str> {
        if ordinal < self.steps as u64 {
            return Ok(());
        }
        if ordinal != self.steps as u64 || self.steps == REPLAY_STEPS {
            return Err("input replay Graph ordinals are not consecutive");
        }
        let position = input.position();
        match self.steps {
            0 => {} // First actual GPU Graph records the initial free-camera pose.
            1 => {
                input.motion(Motion::Forward, true);
                input.step(REPLAY_DT);
                self.movement = forward_step(position, input.position());
            }
            2 => {
                input.capture(true);
                let before = input.clip_from_world(640, 480);
                input.relative_motion(24.0, -8.0, true);
                self.mouse_look = input.clip_from_world(640, 480) != before;
                input.step(REPLAY_DT);
            }
            3 => {
                input.capture(false);
                let before = input.clip_from_world(640, 480);
                input.relative_motion(100.0, 100.0, true);
                let mouse_stopped = input.clip_from_world(640, 480) == before;
                input.step(REPLAY_DT);
                self.release_preserves_movement =
                    mouse_stopped && forward_step(position, input.position());
            }
            4 => {
                input.focus_lost();
                let before = input.clip_from_world(640, 480);
                input.relative_motion(100.0, 100.0, true);
                input.step(REPLAY_DT);
                self.focus_stops_movement =
                    input.position() == position && input.clip_from_world(640, 480) == before;
            }
            _ => unreachable!("bounded replay step checked above"),
        }
        self.steps += 1;
        Ok(())
    }
    pub fn complete(&self) -> bool {
        self.steps == REPLAY_STEPS
            && self.movement
            && self.mouse_look
            && self.release_preserves_movement
            && self.focus_stops_movement
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_release_preserves_held_movement_and_focus_loss_stops_both() {
        let mut input = CameraInput::default();
        input.motion(Motion::Forward, true);
        input.capture(true);
        input.relative_motion(24.0, -8.0, true);
        input.step(REPLAY_DT);
        input.capture(false);
        let stopped_look = input.clip_from_world(640, 480);
        input.relative_motion(200.0, 100.0, true);
        assert_eq!(input.clip_from_world(640, 480), stopped_look);
        let still_moving = input.position();
        input.step(REPLAY_DT);
        assert_ne!(input.position(), still_moving);
        input.focus_lost();
        let stopped = input.clip_from_world(640, 480);
        input.relative_motion(200.0, 100.0, true);
        input.step(REPLAY_DT);
        assert_eq!(input.clip_from_world(640, 480), stopped);
    }

    #[test]
    fn replay_changes_each_rendered_pose_once_and_retains_the_focus_stop_pose() {
        let mut input = CameraInput::default();
        let mut replay = InputReplay::default();
        let mut matrices = [[[0.0; 4]; 4]; REPLAY_STEPS];
        for (ordinal, matrix) in matrices.iter_mut().enumerate() {
            replay.advance(&mut input, ordinal as u64).unwrap();
            *matrix = input.clip_from_world(640, 480);
            replay.advance(&mut input, ordinal as u64).unwrap();
            assert_eq!(
                input.clip_from_world(640, 480),
                *matrix,
                "retrying a Graph ordinal must not advance keyboard motion twice"
            );
        }
        assert!(matrices[..4].windows(2).all(|pair| pair[0] != pair[1]));
        assert_eq!(matrices[3], matrices[4]);
        assert!(replay.complete());
    }
}

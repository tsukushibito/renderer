//! App-owned free camera and deterministic tour shared by both native APIs.
//!
//! Matrices contain four columns and use column vectors. World +Y is up and
//! the camera looks along local +Z. Clip depth is 0..1; projection Y is inverted
//! for Renderer's positive-height Vulkan viewport. The generated DXIL vertex
//! shader performs the corresponding API conversion.

use std::time::Duration;

type Matrix = [[f32; 4]; 4];

#[derive(Clone, Copy, Debug)]
pub enum Motion {
    Forward,
    Backward,
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Debug)]
pub struct Camera {
    position: [f32; 3],
    yaw: f32,
    pitch: f32,
    motion: [bool; 6],
    fast: bool,
}

impl Default for Camera {
    fn default() -> Self {
        Self::new()
    }
}

impl Camera {
    pub fn new() -> Self {
        Self {
            position: [0.0, 1.8, -5.0],
            yaw: 0.0,
            pitch: -0.25,
            motion: [false; 6],
            fast: false,
        }
    }

    pub fn set_motion(&mut self, motion: Motion, pressed: bool) {
        self.motion[motion as usize] = pressed;
    }

    pub fn set_fast(&mut self, pressed: bool) {
        self.fast = pressed;
    }

    /// Release every held key on focus/capture loss.
    pub fn clear_input(&mut self) {
        self.motion.fill(false);
        self.fast = false;
    }

    /// Relative mouse motion, in physical pixels. Ignore non-finite device
    /// input; clamp each event to avoid a discontinuity after capture changes.
    pub fn look(&mut self, dx: f64, dy: f64) {
        if !dx.is_finite() || !dy.is_finite() {
            return;
        }
        self.yaw = (self.yaw + dx.clamp(-1000.0, 1000.0) as f32 * 0.0025)
            .rem_euclid(std::f32::consts::TAU);
        self.pitch = (self.pitch - dy.clamp(-1000.0, 1000.0) as f32 * 0.0025).clamp(-1.55, 1.55);
    }

    /// Advance from real elapsed time. A long pause does not teleport the
    /// camera; diagonal movement has the same speed as one held direction.
    pub fn step(&mut self, elapsed: Duration) {
        let axis = |positive: usize, negative: usize| {
            u8::from(self.motion[positive]) as f32 - u8::from(self.motion[negative]) as f32
        };
        let [forward, right, up] = self.basis();
        let directions = [axis(0, 1), axis(3, 2), axis(4, 5)];
        let movement = std::array::from_fn::<_, 3, _>(|component| {
            forward[component] * directions[0]
                + right[component] * directions[1]
                + up[component] * directions[2]
        });
        let length = dot(movement, movement).sqrt();
        if length > 0.0 {
            let distance = elapsed.as_secs_f32().min(0.1) * if self.fast { 12.0 } else { 3.0 };
            for (position, movement) in self.position.iter_mut().zip(movement) {
                *position += movement / length * distance;
            }
        }
    }

    /// A fixed 960-frame orbit. Pose depends only on the frame input, making
    /// VSync-on/off and Vulkan/D3D12 runs use the same camera sequence.
    pub fn tour(&mut self, frame: u64) {
        let angle = (frame % 960) as f32 * (std::f32::consts::TAU / 960.0);
        self.position = [angle.sin() * 6.0, 2.2, -angle.cos() * 6.0];
        let direction = [-self.position[0], 0.6 - self.position[1], -self.position[2]];
        self.yaw = direction[0].atan2(direction[2]);
        self.pitch = (direction[1] / dot(direction, direction).sqrt()).asin();
        self.clear_input();
    }

    pub fn position(&self) -> [f32; 3] {
        self.position
    }

    #[allow(dead_code, reason = "available to other example scenes")]
    pub fn set_pose(&mut self, position: [f32; 3], target: [f32; 3]) {
        let delta: [f32; 3] = std::array::from_fn(|axis| target[axis] - position[axis]);
        self.position = position;
        self.yaw = delta[0].atan2(delta[2]);
        self.pitch = delta[1].atan2(delta[0].hypot(delta[2]));
        self.clear_input();
    }

    #[allow(dead_code, reason = "available to other example scenes")]
    pub fn target(&self) -> [f32; 3] {
        let forward = self.basis()[0];
        std::array::from_fn(|axis| self.position[axis] + forward[axis])
    }

    pub fn clip_from_world(&self, width: u32, height: u32) -> Matrix {
        let [forward, right, up] = self.basis();
        let view = [
            [right[0], up[0], forward[0], 0.0],
            [right[1], up[1], forward[1], 0.0],
            [right[2], up[2], forward[2], 0.0],
            [
                -dot(right, self.position),
                -dot(up, self.position),
                -dot(forward, self.position),
                1.0,
            ],
        ];
        let near = 0.05;
        let far = 100.0;
        let scale = 1.0 / (60.0_f32.to_radians() * 0.5).tan();
        // A minimized Window never submits a Graph. Keeping the matrix finite
        // nevertheless makes camera state independent of resize event order.
        let aspect = width.max(1) as f32 / height.max(1) as f32;
        let projection = [
            [scale / aspect, 0.0, 0.0, 0.0],
            [0.0, -scale, 0.0, 0.0],
            [0.0, 0.0, far / (far - near), 1.0],
            [0.0, 0.0, -near * far / (far - near), 0.0],
        ];
        multiply(projection, view)
    }

    fn basis(&self) -> [[f32; 3]; 3] {
        let (yaw_sin, yaw_cos) = self.yaw.sin_cos();
        let (pitch_sin, pitch_cos) = self.pitch.sin_cos();
        let forward = [yaw_sin * pitch_cos, pitch_sin, yaw_cos * pitch_cos];
        let right = [yaw_cos, 0.0, -yaw_sin];
        let up = [-yaw_sin * pitch_sin, pitch_cos, -yaw_cos * pitch_sin];
        [forward, right, up]
    }
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| a * b).sum()
}

fn multiply(a: Matrix, b: Matrix) -> Matrix {
    std::array::from_fn(|column| {
        std::array::from_fn(|row| (0..4).map(|k| a[k][row] * b[column][k]).sum())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn distance(a: [f32; 3], b: [f32; 3]) -> f32 {
        dot(
            std::array::from_fn(|i| a[i] - b[i]),
            std::array::from_fn(|i| a[i] - b[i]),
        )
        .sqrt()
    }

    fn transform(matrix: Matrix, point: [f32; 3]) -> [f32; 4] {
        let point = [point[0], point[1], point[2], 1.0];
        std::array::from_fn(|row| {
            (0..4)
                .map(|column| matrix[column][row] * point[column])
                .sum()
        })
    }

    #[test]
    fn focus_loss_releases_movement_and_fast_mode() {
        let mut camera = Camera::new();
        camera.set_motion(Motion::Forward, true);
        camera.set_fast(true);
        camera.step(Duration::from_millis(10));
        camera.clear_input();
        let stopped = camera.position();
        camera.step(Duration::from_secs(1));
        assert_eq!(camera.position(), stopped);
        camera.set_motion(Motion::Forward, true);
        camera.step(Duration::from_millis(10));
        assert!((distance(camera.position(), stopped) - 0.03).abs() < 1e-6);
    }

    #[test]
    fn diagonal_and_opposing_keys_have_predictable_speed() {
        let mut camera = Camera::new();
        let start = camera.position();
        camera.set_motion(Motion::Forward, true);
        camera.set_motion(Motion::Right, true);
        camera.step(Duration::from_millis(100));
        assert!((distance(start, camera.position()) - 0.3).abs() < 1e-6);
        camera.set_motion(Motion::Backward, true);
        camera.set_motion(Motion::Left, true);
        let stopped = camera.position();
        camera.step(Duration::from_millis(100));
        assert_eq!(camera.position(), stopped);
        camera.clear_input();
        camera.set_motion(Motion::Up, true);
        camera.set_motion(Motion::Down, true);
        camera.step(Duration::from_millis(100));
        assert_eq!(camera.position(), stopped);
    }

    #[test]
    fn projection_maps_forward_near_far_and_up_to_renderer_clip_space() {
        let camera = Camera::new();
        let [forward, _, up] = camera.basis();
        let matrix = camera.clip_from_world(640, 480);
        let at_distance =
            |distance| std::array::from_fn(|i| camera.position[i] + forward[i] * distance);
        for (distance, expected_depth) in [(0.05, 0.0), (100.0, 1.0)] {
            let clip = transform(matrix, at_distance(distance));
            assert!((clip[2] / clip[3] - expected_depth).abs() < 2e-5);
            assert!(clip[0].abs() < 1e-4 && clip[1].abs() < 1e-4);
            assert!(clip[3] > 0.0);
        }
        let point = std::array::from_fn(|i| camera.position[i] + forward[i] * 3.0 + up[i]);
        assert!(transform(matrix, point)[1] < 0.0);
    }

    #[test]
    fn mouse_and_pause_bounds_keep_projection_finite() {
        let mut camera = Camera::new();
        camera.look(100_000.0, -100_000.0);
        let expected = camera.clip_from_world(0, 0);
        assert!(expected.into_iter().flatten().all(f32::is_finite));
        camera.look(f64::NAN, 1.0);
        assert_eq!(camera.clip_from_world(0, 0), expected);
        let start = camera.position();
        camera.set_motion(Motion::Forward, true);
        camera.step(Duration::from_secs(60));
        assert!((distance(start, camera.position()) - 0.3).abs() < 1e-6);
    }

    #[test]
    fn tour_depends_only_on_frame_and_wraps_without_clock_drift() {
        let mut first = Camera::new();
        let mut second = Camera::new();
        first.look(20.0, 30.0);
        first.set_motion(Motion::Forward, true);
        first.tour(120);
        second.tour(1080);
        assert_eq!(first.position(), second.position());
        assert_eq!(
            first.clip_from_world(800, 600),
            second.clip_from_world(800, 600)
        );
        let stopped = first.position();
        first.step(Duration::from_secs(1));
        assert_eq!(first.position(), stopped);
        let target = transform(first.clip_from_world(800, 600), [0.0, 0.6, 0.0]);
        assert!(target[0].abs() < 1e-5 && target[1].abs() < 1e-5);
    }
}

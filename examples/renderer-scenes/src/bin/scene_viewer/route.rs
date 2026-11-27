use super::{backend, finish};
use sashiko_asset::scene::{DisplayScene, PerspectiveView};
use sashiko_renderer::pipeline::PipelineResult;
use std::{
    fs,
    io::{self, Write},
    path::Path,
    time::Duration,
};

#[derive(serde::Deserialize)]
pub(super) struct Route {
    fov: f32,
    // Time in seconds, world position in metres, unwrapped yaw and pitch in degrees.
    keys: Vec<[f32; 6]>,
}

impl Route {
    pub fn load(path: &Path) -> PipelineResult<Self> {
        let route: Self = serde_json::from_slice(&fs::read(path)?)?;
        if route.keys.len() < 2
            || route.keys[0][0] != 0.
            || !route.fov.is_finite()
            || !(1.0..179.).contains(&route.fov)
            || route
                .keys
                .iter()
                .any(|k| k.iter().any(|v| !v.is_finite()) || k[5].abs() >= 90.)
            || route.keys.windows(2).any(|k| k[1][0] <= k[0][0])
        {
            return Err("invalid camera route".into());
        }
        Ok(route)
    }

    pub fn duration(&self) -> f32 {
        self.keys.last().unwrap()[0]
    }

    pub fn pose(&self, seconds: f32) -> PerspectiveView {
        let index = self
            .keys
            .partition_point(|k| k[0] <= seconds)
            .clamp(1, self.keys.len() - 1);
        let (a, b) = (self.keys[index - 1], self.keys[index]);
        let t = ((seconds - a[0]) / (b[0] - a[0])).clamp(0., 1.);
        let at = |i: usize| a[i] + t * (b[i] - a[i]);
        let (yaw, pitch) = (at(4).to_radians(), at(5).to_radians());
        PerspectiveView {
            position: [at(1), at(2), at(3)],
            forward: [
                yaw.sin() * pitch.cos(),
                pitch.sin(),
                yaw.cos() * pitch.cos(),
            ],
            up: [0., 1., 0.],
            vertical_fov_degrees: self.fov,
            mirror_x: false,
        }
    }
}

pub(super) fn record(package: &Path, name: &str, path: &Path, fps: u32) -> PipelineResult<()> {
    if !(1..=120).contains(&fps) {
        return Err("capture rate outside 1..120".into());
    }
    let route = Route::load(path)?;
    let extent = [1920, 1080];
    let (mut scene, mut renderer) = DisplayScene::headless(package, name, extent, backend())?;
    let mut rgba = vec![0; 1920 * 1080 * 4];
    let mut stdout = io::stdout().lock();
    for frame_index in 0..(route.duration() * fps as f32).ceil() as u32 {
        let pose = route.pose(frame_index as f32 / fps as f32);
        scene.select_lods(&pose, extent[1]);
        let view = pose.camera(extent, [0.02, 1500.])?;
        renderer.set_camera(view.camera);
        let mut frame = renderer.begin_frame()?;
        scene.draw(&mut frame)?;
        let ticket = frame.capture()?;
        if !renderer.read_capture(&ticket, &mut rgba, Duration::from_secs(120))? {
            return Err("route capture timed out".into());
        }
        stdout.write_all(&rgba)?;
    }
    finish(scene, renderer.into_runtime()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walking_route_turns_in_place_and_returns_to_the_start() {
        let route: Route =
            serde_json::from_str(include_str!("../../../../../assets/namaqualand/walk.json"))
                .unwrap();
        let start = route.pose(0.);
        let before_turn = route.pose(18.);
        let after_turn = route.pose(22.);
        let end = route.pose(route.duration());
        for (first, last) in start
            .position
            .into_iter()
            .chain(start.forward)
            .zip(end.position.into_iter().chain(end.forward))
        {
            assert!((first - last).abs() < 0.00001);
        }
        assert_eq!(before_turn.position, after_turn.position);
        let horizontal_dot = before_turn.forward[0] * after_turn.forward[0]
            + before_turn.forward[2] * after_turn.forward[2];
        assert!(horizontal_dot < -0.98);
    }
}

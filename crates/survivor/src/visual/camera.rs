use renderer::{
    pbr::{Projection, ViewInput},
    pipeline::{CameraView, PipelineResult, ViewId},
};

// Twenty-two metres of vertical clip space, viewed from the front of the room.
// Positive game Z projects down the screen, positive X projects right.
pub(super) fn tracking(target: [f32; 2], extent: [u32; 2]) -> PipelineResult<CameraView> {
    let [width, height] = extent;
    if width == 0 || height == 0 {
        return Err("camera requires a nonempty viewport".into());
    }
    let [x, z] = target;
    let half_height = 11.;
    let half_width = half_height * width as f32 / height as f32;
    let distance = 34.;
    let (s, c) = 54.0f32.to_radians().sin_cos();
    let eye = [x, distance * s, z + distance * c];
    let near = 0.5;
    let far = 65.;
    let depth = far - near;
    let matrix = [
        [1. / half_width, 0., 0., 0.],
        [0., -c / half_height, -s / depth, 0.],
        [0., s / half_height, -c / depth, 0.],
        [
            -x / half_width,
            -s * z / half_height,
            (distance + c * z - near) / depth,
            1.,
        ],
    ];
    Ok(CameraView {
        id: ViewId(1),
        extent,
        camera: ViewInput::new(
            matrix,
            eye,
            [0., -f64::from(s), -f64::from(c)],
            [near, far],
            Projection::Orthographic,
        )?,
    })
}

pub(super) fn on_screen(camera: &CameraView, pos: [f32; 2]) -> bool {
    let m = camera.camera.clip_from_world();
    let x = m[0][0] * pos[0] + m[3][0];
    let feet = m[2][1] * pos[1] + m[3][1];
    let head = feet + m[1][1] * 1.9;
    let margin_x = m[0][0].abs() * 0.65;
    let margin_y = m[2][1].abs() * 0.65;
    x.abs() <= 1. + margin_x && feet >= -1. - margin_y && head <= 1. + margin_y
}

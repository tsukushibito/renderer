//! Shared inputs for the standard scene and direct/indirect visibility comparisons.
pub const TRIANGLE: [[f32; 3]; 3] = [[-1., -1., 0.], [1., -1., 0.], [0., 1., 0.]];
pub const INDICES: [u32; 3] = [0, 2, 1];
pub const SEED: u32 = 71;

pub fn camera_x(frame: usize) -> f32 {
    // Integer phase makes the replay identical across processes and warmup lengths.
    ((frame % 64) as f32 - 32.).abs() / 1024. - 0.015625
}

/// World-space center and half-size. A coprime stride visits every cell once.
pub fn placement(index: usize, count: usize, visible_percent: u32, seed: u32) -> [f32; 4] {
    let stride = if count.is_multiple_of(7919) {
        count - 1
    } else {
        7919
    };
    let cell = (index * stride + seed as usize) % count;
    let width = (count - 1).isqrt() + 1;
    let pitch = 1.84 / width as f32;
    let outside = cell >= count * visible_percent as usize / 100;
    let x = -0.92 + (cell % width) as f32 * pitch + pitch * 0.5;
    let y = -0.92 + (cell / width) as f32 * pitch + pitch * 0.5;
    let z = 0.15 + 0.7 * ((index + seed as usize) % 127) as f32 / 126.;
    [if outside { x + 3. } else { x }, y, z, pitch * 0.40]
}

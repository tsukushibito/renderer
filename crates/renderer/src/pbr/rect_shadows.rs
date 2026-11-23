//! Five 90-degree projections cover a rectangle's emitting hemisphere.
use super::{Bounds, RectLight};
use renderer_core::graph::{GraphError, ImageDesc, RasterRect};

pub(crate) const FACE_SIZE: u32 = 512;
pub(crate) const FACES: usize = 5;

pub(crate) fn atlas_desc(lights: usize) -> Result<ImageDesc, GraphError> {
    let faces = u32::try_from(lights)
        .ok()
        .and_then(|n| n.checked_mul(FACES as u32))
        .filter(|&n| n > 0)
        .ok_or_else(|| GraphError::invalid_input("invalid rectangle shadow capacity"))?;
    let cols = (faces - 1).isqrt() + 1;
    Ok(ImageDesc::depth32(
        cols * FACE_SIZE,
        faces.div_ceil(cols) * FACE_SIZE,
    ))
}

pub(crate) fn tile(desc: ImageDesc, index: usize) -> RasterRect {
    let cols = desc.width / FACE_SIZE;
    RasterRect::new(
        index as u32 % cols * FACE_SIZE,
        index as u32 / cols * FACE_SIZE,
        FACE_SIZE,
        FACE_SIZE,
    )
}

pub(crate) fn affects(light: RectLight, bounds: Bounds) -> bool {
    let center = light.center();
    (0..3).all(|i| {
        center[i] + light.bound_radius >= bounds.min()[i]
            && center[i] - light.bound_radius <= bounds.max()[i]
    })
}

pub(crate) type Matrix = [[f32; 4]; 4];

pub(crate) fn matrix(light: RectLight, face: usize) -> Matrix {
    fn unit(a: [f32; 3]) -> [f32; 3] {
        let len = a[0].hypot(a[1]).hypot(a[2]);
        a.map(|x| x / len)
    }
    let neg = |a: [f32; 3]| a.map(|x| -x);
    let [u, v] = light.half_edges().map(unit);
    let n = light.normal();
    let (x, y, z) = match face {
        0 => (u, v, n),
        1 => (neg(n), v, u),
        2 => (n, v, neg(u)),
        3 => (u, neg(n), v),
        4 => (u, n, neg(v)),
        _ => unreachable!(),
    };
    let far = light.bound_radius;
    let near = (far * 0.0001).min(0.01);
    let scale = far / (far - near);
    let c = light.center();
    let offset = |a: [f32; 3]| -(a[0] * c[0] + a[1] * c[1] + a[2] * c[2]);
    let w = offset(z);
    [
        [x[0], y[0], z[0] * scale, z[0]],
        [x[1], y[1], z[1] * scale, z[1]],
        [x[2], y[2], z[2] * scale, z[2]],
        [offset(x), offset(y), (w - near) * scale, w],
    ]
}

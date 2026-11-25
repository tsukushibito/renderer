use renderer::{mesh::MeshData, pbr::PbrVertex, pipeline::PipelineResult};
use sashiko::math::Vec3;
use std::f32::consts::TAU;

#[derive(Default)]
pub(super) struct Geometry {
    vertices: Vec<PbrVertex>,
    indices: Vec<u32>,
}

impl Geometry {
    /// A capped circular fitting between arbitrary points. Vary the radii for a
    /// vessel shoulder or an objective lens barrel. Axial normals at either end
    /// keep the metal rims sharp while the body shades continuously.
    pub fn tube(&mut self, a: [f32; 3], b: [f32; 3], ra: f32, rb: f32) -> PipelineResult<()> {
        const N: u32 = 18;
        let a = Vec3::from_array(a);
        let b = Vec3::from_array(b);
        let delta = b - a;
        let length = delta.length();
        let axis = delta / length;
        let basis = axis.cross(if axis.x().abs() < 0.8 {
            Vec3::X
        } else {
            Vec3::Z
        });
        let u = basis / basis.length();
        let v = axis.cross(u);
        let start = u32::try_from(self.vertices.len())?;
        for k in 0..=N {
            let (s, c) = (TAU * k as f32 / N as f32).sin_cos();
            let dir = u * c + v * s;
            let tangent = u * -s + v * c;
            let slope = (ra - rb) / length;
            let normal = (dir + axis * slope) / (1. + slope * slope).sqrt();
            for (center, r, height) in [(a, ra, 0.), (b, rb, length)] {
                self.vertices.push(PbrVertex::new(
                    (center + dir * r).to_array(),
                    normal.to_array(),
                    [tangent[0], tangent[1], tangent[2], 1.],
                    [k as f32 / N as f32, height],
                )?);
            }
            if k < N {
                let i = start + 2 * k;
                self.indices.extend([i, i + 2, i + 3, i, i + 3, i + 1]);
            }
        }
        for (center, r, sign) in [(a, ra, -1.), (b, rb, 1.)] {
            let base = u32::try_from(self.vertices.len())?;
            let n = (axis * sign).to_array();
            let t = [u[0], u[1], u[2], sign];
            self.vertices
                .push(PbrVertex::new(center.to_array(), n, t, [0.5, 0.5])?);
            for k in 0..=N {
                let (s, c) = (sign * TAU * k as f32 / N as f32).sin_cos();
                self.vertices.push(PbrVertex::new(
                    (center + (u * c + v * s) * r).to_array(),
                    n,
                    t,
                    [(1. + c) / 2., (1. + s) / 2.],
                )?);
                if k < N {
                    self.indices.extend([base, base + k + 1, base + k + 2]);
                }
            }
        }
        Ok(())
    }
    // Axes extend from the centre to each edge. UV scale stays in metres so decals and
    // acquisition textures remain the same size on the floor, cabinet doors and trims.
    fn face(
        &mut self,
        center: [f32; 3],
        u: [f32; 3],
        v: [f32; 3],
        repeat: f32,
    ) -> PipelineResult<()> {
        let center = Vec3::from_array(center);
        let u = Vec3::from_array(u);
        let v = Vec3::from_array(v);
        let u_length = u.length();
        let v_length = v.length();
        let un = u / u_length;
        let vn = v / v_length;
        let normal = un.cross(vn).to_array();
        let base = u32::try_from(self.vertices.len())?;
        for (s, t) in [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)] {
            let p = (center + u * s + v * t).to_array();
            self.vertices.push(PbrVertex::new(
                p,
                normal,
                [un[0], un[1], un[2], 1.],
                [(1. + s) * u_length / repeat, (1. + t) * v_length / repeat],
            )?);
        }
        self.indices
            .extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        Ok(())
    }

    pub fn slab(&mut self, [x, y, z]: [f32; 3], [w, h, d]: [f32; 3]) -> PipelineResult<()> {
        self.face([x, y + h / 2., z], [w / 2., 0., 0.], [0., 0., -d / 2.], 2.)?;
        self.face([x, y - h / 2., z], [w / 2., 0., 0.], [0., 0., d / 2.], 2.)?;
        self.face([x, y, z + d / 2.], [w / 2., 0., 0.], [0., h / 2., 0.], 1.)?;
        self.face([x, y, z - d / 2.], [-w / 2., 0., 0.], [0., h / 2., 0.], 1.)?;
        self.face([x + w / 2., y, z], [0., 0., -d / 2.], [0., h / 2., 0.], 1.)?;
        self.face([x - w / 2., y, z], [0., 0., d / 2.], [0., h / 2., 0.], 1.)
    }

    pub fn data(self) -> PipelineResult<MeshData> {
        Ok(MeshData::pbr(&self.vertices, &self.indices)?)
    }
}

// Flat torus section, optionally filled. One mesh can be instanced for a contact
// shadow, a marker, a shock wave or the player's locator.
pub(super) fn disk(inner: f32) -> PipelineResult<MeshData> {
    let mut g = Geometry::default();
    for i in 0..=48 {
        let (s, c) = (TAU * i as f32 / 48.).sin_cos();
        for r in [inner, 1.] {
            g.vertices.push(PbrVertex::new(
                [s * r, 0., c * r],
                [0., 1., 0.],
                [1., 0., 0., 1.],
                [(s * r + 1.) / 2., (c * r + 1.) / 2.],
            )?);
        }
        if i < 48 {
            let k = i * 2;
            g.indices.extend([k, k + 1, k + 3, k, k + 3, k + 2]);
        }
    }
    g.data()
}

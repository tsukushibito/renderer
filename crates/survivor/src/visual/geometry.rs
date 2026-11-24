use renderer::{mesh::MeshData, pbr::PbrVertex, pipeline::PipelineResult};
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
        let delta: [f32; 3] = std::array::from_fn(|i| b[i] - a[i]);
        let length = delta[0].hypot(delta[1]).hypot(delta[2]);
        let axis = delta.map(|v| v / length);
        let cross = |u: [f32; 3], v: [f32; 3]| {
            [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ]
        };
        let basis = cross(
            axis,
            if axis[0].abs() < 0.8 {
                [1., 0., 0.]
            } else {
                [0., 0., 1.]
            },
        );
        let norm = basis[0].hypot(basis[1]).hypot(basis[2]);
        let u = basis.map(|v| v / norm);
        let v = cross(axis, u);
        let start = u32::try_from(self.vertices.len())?;
        for k in 0..=N {
            let (s, c) = (TAU * k as f32 / N as f32).sin_cos();
            let dir: [f32; 3] = std::array::from_fn(|j| c * u[j] + s * v[j]);
            let tangent: [f32; 3] = std::array::from_fn(|j| -s * u[j] + c * v[j]);
            let slope = (ra - rb) / length;
            let normal =
                std::array::from_fn(|j| (dir[j] + axis[j] * slope) / (1. + slope * slope).sqrt());
            for (center, r, height) in [(a, ra, 0.), (b, rb, length)] {
                self.vertices.push(PbrVertex::new(
                    std::array::from_fn(|j| center[j] + r * dir[j]),
                    normal,
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
            let n = axis.map(|v| v * sign);
            let t = [u[0], u[1], u[2], sign];
            self.vertices
                .push(PbrVertex::new(center, n, t, [0.5, 0.5])?);
            for k in 0..=N {
                let (s, c) = (sign * TAU * k as f32 / N as f32).sin_cos();
                self.vertices.push(PbrVertex::new(
                    std::array::from_fn(|j| center[j] + r * (c * u[j] + s * v[j])),
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
        let length = |p: [f32; 3]| p.iter().map(|c| c * c).sum::<f32>().sqrt();
        let un = u.map(|x| x / length(u));
        let vn = v.map(|x| x / length(v));
        let normal = [
            un[1] * vn[2] - un[2] * vn[1],
            un[2] * vn[0] - un[0] * vn[2],
            un[0] * vn[1] - un[1] * vn[0],
        ];
        let base = u32::try_from(self.vertices.len())?;
        for (s, t) in [(-1., -1.), (1., -1.), (1., 1.), (-1., 1.)] {
            let p = std::array::from_fn(|i| center[i] + s * u[i] + t * v[i]);
            self.vertices.push(PbrVertex::new(
                p,
                normal,
                [un[0], un[1], un[2], 1.],
                [(1. + s) * length(u) / repeat, (1. + t) * length(v) / repeat],
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

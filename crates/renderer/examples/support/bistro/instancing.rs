//! One resident opaque mesh, placed on a fixed grid in front of the initial view.

use super::{Error, asset::Scene, camera::Camera, program, shadow::Bounds};
use renderer::graph::{RasterConstants, RasterDraw};

const COLUMNS: usize = 6;
const ROWS: usize = 4;
pub(super) const COUNT: usize = COLUMNS * ROWS;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Mode {
    #[default]
    Off,
    Individual,
    Instanced,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Individual => "individual",
            Self::Instanced => "instanced",
        }
    }

    pub fn enabled(self) -> bool {
        self != Self::Off
    }

    pub fn draws(self) -> usize {
        match self {
            Self::Off => 0,
            Self::Individual => COUNT,
            Self::Instanced => 1,
        }
    }
}

pub(super) struct Placements {
    pub primitive: usize,
    pub material: usize,
    layout: [u32; (program::INSTANCE_INLINE_BYTES / 4) as usize],
}

impl Placements {
    pub fn new(
        scene: &Scene,
        bounds: &[Bounds],
        camera: Camera,
        aspect: f32,
        near: f32,
        far: f32,
    ) -> Result<Self, Error> {
        let half_angle = (camera.fov.to_radians() * 0.5).tan();
        let matrix = camera.matrix(aspect, near, far)?;
        let right: [f32; 3] = std::array::from_fn(|i| matrix[i][0] * aspect * half_angle);
        let up: [f32; 3] = std::array::from_fn(|i| -matrix[i][1] * half_angle);
        let forward: [f32; 3] = std::array::from_fn(|i| matrix[i][3]);
        let distance = (near * 16.0).max(0.5).min(near + (far - near) * 0.25);
        let half_height = distance * half_angle;
        let spacing = 1.6 * half_height * (aspect / COLUMNS as f32).min(1.0 / ROWS as f32);
        let (primitive, material, source, diameter) = scene
            .primitives
            .iter()
            .zip(bounds)
            .enumerate()
            .filter_map(|(index, (mesh, bound))| {
                let material = &scene.materials[mesh.material];
                if material.uniform[48..52] != [0; 4] {
                    return None;
                }
                let diameter = bound
                    .min
                    .into_iter()
                    .zip(bound.max)
                    .map(|(min, max)| (max - min).powi(2))
                    .sum::<f32>()
                    .sqrt();
                (diameter.is_finite() && diameter > 0.01 && front_facing(scene, mesh, forward))
                    .then_some((index, mesh.material, *bound, diameter))
            })
            .min_by(|(_, _, _, a), (_, _, _, b)| a.total_cmp(b))
            .ok_or("instance comparison needs a visible opaque triangle")?;
        let center: [f32; 3] = std::array::from_fn(|i| source.min[i] * 0.5 + source.max[i] * 0.5);
        let origin: [f32; 3] = std::array::from_fn(|i| {
            camera.position[i] + distance * forward[i]
                - spacing * (COLUMNS - 1) as f32 * 0.5 * right[i]
                - spacing * (ROWS - 1) as f32 * 0.5 * up[i]
        });
        let mut layout = [0; (program::INSTANCE_INLINE_BYTES / 4) as usize];
        for (word, value) in layout.iter_mut().zip(
            center
                .into_iter()
                .chain([spacing * 0.72 / diameter])
                .chain(origin)
                .chain([0.0])
                .chain(right.map(|value| value * spacing))
                .chain([0.0])
                .chain(up.map(|value| value * spacing))
                .chain([0.0]),
        ) {
            if !value.is_finite() {
                return Err("non-finite instance comparison placement".into());
            }
            *word = value.to_bits();
        }
        Ok(Self {
            primitive,
            material,
            layout,
        })
    }

    pub fn draws(&self, mode: Mode, draw: RasterDraw) -> impl Iterator<Item = RasterDraw> + '_ {
        (0..mode.draws()).map(move |offset| {
            let mut words = self.layout;
            words[16] = offset as u32;
            RasterDraw {
                parameters: RasterConstants::new(&words)
                    .expect("fixed instance layout fits inline storage"),
                instance_count: if mode == Mode::Instanced {
                    COUNT as u32
                } else {
                    1
                },
                ..draw
            }
        })
    }
}

fn front_facing(scene: &Scene, primitive: &super::asset::Primitive, forward: [f32; 3]) -> bool {
    let position = |bytes: [u8; 4]| {
        let index = u32::from_le_bytes(bytes) as usize * program::VERTEX_BYTES;
        let start = primitive.vertices.start + index;
        let words = scene.vertices[start..start + 12].as_chunks::<4>().0;
        [words[0], words[1], words[2]].map(f32::from_le_bytes)
    };
    scene.indices[primitive.indices.clone()]
        .as_chunks::<12>()
        .0
        .iter()
        .any(|triangle| {
            let points = triangle.as_chunks::<4>().0;
            let [a, b, c] = [points[0], points[1], points[2]].map(position);
            let ab: [f32; 3] = std::array::from_fn(|i| b[i] - a[i]);
            let ac: [f32; 3] = std::array::from_fn(|i| c[i] - a[i]);
            let normal = [
                ab[1] * ac[2] - ab[2] * ac[1],
                ab[2] * ac[0] - ab[0] * ac[2],
                ab[0] * ac[1] - ab[1] * ac[0],
            ];
            normal
                .into_iter()
                .zip(forward)
                .map(|(a, b)| a * b)
                .sum::<f32>()
                < -1e-8
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::graph::{BufferDesc, GraphCapacity, GraphPool};

    #[test]
    fn draws_select_the_same_placement_records_and_resident_mesh() {
        let scene = Scene::fixture(false);
        let bounds = scene
            .primitives
            .iter()
            .map(|p| {
                let [x, y, z] = p.center;
                Bounds::new([x - 0.35, y - 0.4, z], [x + 0.35, y + 0.4, z]).unwrap()
            })
            .collect::<Vec<_>>();
        let placements =
            Placements::new(&scene, &bounds, Camera::fixture(), 16.0 / 9.0, 0.02, 100.0).unwrap();
        assert_eq!(scene.materials[placements.material].uniform[48..52], [0; 4]);
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let draw = RasterDraw {
            vertices: graph
                .buffer("resident mesh", BufferDesc { size: 192 })
                .unwrap(),
            indices: graph
                .buffer("resident indices", BufferDesc { size: 24 })
                .unwrap(),
            first_index: 0,
            index_count: 6,
            instance_count: 1,
            parameters: RasterConstants::default(),
            bindings: None,
        };
        let mut instanced = placements.draws(Mode::Instanced, draw);
        let single = instanced.next().unwrap();
        assert_eq!(single.instance_count, COUNT as u32);
        assert_eq!(single.parameters.words()[16], 0);
        assert!(instanced.next().is_none());
        let mut draws = placements.draws(Mode::Individual, draw);
        for i in 0..COUNT {
            let individual = draws.next().unwrap();
            assert_eq!(individual.instance_count, 1);
            assert_eq!(individual.vertices, single.vertices);
            assert_eq!(individual.indices, single.indices);
            assert_eq!(
                individual.parameters.words()[..16],
                single.parameters.words()[..16]
            );
            assert_eq!(individual.parameters.words()[16], i as u32);
        }
        assert!(draws.next().is_none());
    }
}

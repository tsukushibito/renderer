//! Deterministic walking mannequin and three moving props, shared by native and Canvas.
use super::{Error, asset::Scene, camera::Camera, ibl};
use sashiko_renderer::pbr::{BakedLighting, LightmapAddress, ShadowCapsule, Transform};

pub(super) fn camera() -> Camera {
    Camera {
        position: [0., 1.85, 3.4],
        target: [0., 0.59, 0.],
        fov: 42.,
    }
}

pub(super) fn volume() -> Result<BakedLighting, Error> {
    let mut cells = BakedLighting::uniform([[1.; 3]])?.into_rgba32f();
    let mut put = |index: usize, values: [f32; 4]| {
        for (dst, v) in cells[index * 16..]
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(values)
        {
            dst.copy_from_slice(&v.to_le_bytes());
        }
    };
    put(0, [-4., -4., -4., 8.]);
    put(7, [10., 10., 10., 1.]);
    put(8, [10., 10., 10., 0.]);
    // Cosine-convolved, broad window from the upper left.
    put(10, [-0.68, -0.68, -0.68, 0.]);
    put(11, [0.51, 0.51, 0.51, 0.]);
    Ok(BakedLighting::from_texels(cells)?)
}

pub(super) fn populate(scene: &mut Scene, env: &mut ibl::Environment) -> Result<(), Error> {
    scene.source["indirect_shadow_demo"] = true.into();
    scene.primitives.truncate(2);
    scene.mesh_count = 2;
    for (i, p) in scene.primitives.iter_mut().enumerate() {
        p.surface = Some(LightmapAddress { page: 0, slot: 1 });
        p.material = 0;
        let floor = i == 0;
        p.center = if floor { [0.; 3] } else { [0., 1., -1.5] };
        for (vertex, [x, y]) in scene.vertices[p.vertices.clone()]
            .as_chunks_mut::<48>()
            .0
            .iter_mut()
            .zip([[-2.5, 0.], [2.5, 0.], [2.5, 3.], [-2.5, 3.]])
        {
            let data = if floor {
                [x, 0., 2.5 - y * (4.0 / 3.0), 0., 1., 0.]
            } else {
                [x, y, -1.5, 0., 0., 1.]
            };
            for (dst, v) in vertex.as_chunks_mut::<4>().0.iter_mut().zip(data) {
                dst.copy_from_slice(&f32::to_le_bytes(v));
            }
        }
    }
    // The wall uses MASK and the floor carries real emission, so the
    // separated-lighting captures exercise both material contracts.
    let floor = sashiko_renderer::pbr::MaterialFactors {
        base_color: [0.61, 0.68, 0.72, 1.],
        emissive_nits: [0.02, 0.01, 0.005],
        ..Default::default()
    };
    scene.materials[0].uniform = floor.encode()?;
    scene.materials[1].uniform = sashiko_renderer::pbr::MaterialFactors {
        alpha_mode: sashiko_renderer::pbr::AlphaMode::Mask,
        base_color: [0.38, 0.46, 0.52, 1.],
        ..Default::default()
    }
    .encode()?;
    scene.primitives[1].material = 1;
    scene.add_inspection_sphere([0.; 3], 0.8, 0.);
    scene.materials.last_mut().unwrap().uniform = sashiko_renderer::pbr::MaterialFactors {
        base_color: [0.31, 0.15, 0.045, 1.],
        roughness: 0.8,
        ..Default::default()
    }
    .encode()?;
    let mut page = vec![1., 1., 3., 0., 5., 4., 5., 4., 2., 2., 0., 0.];
    page.resize(80, 1.);
    let bytes: Vec<_> = page.into_iter().flat_map(f32::to_le_bytes).collect();
    env.lightmaps.push(ibl::Image {
        format: sashiko_renderer_core::ImageFormat::Rgba32Float,
        width: 5,
        height: 4,
        mips: std::iter::once(0..bytes.len()).collect(),
        bytes,
    });
    let probes = volume()?;
    let [width, height] = probes.extent();
    let bytes = probes.into_rgba32f();
    env.diffuse = ibl::Image {
        format: sashiko_renderer_core::ImageFormat::Rgba32Float,
        width,
        height,
        mips: std::iter::once(0..bytes.len()).collect(),
        bytes,
    };
    Ok(())
}

pub(super) struct Pose {
    pub capsules: [ShadowCapsule; 16],
    pub transforms: [Transform; 16],
}

impl Pose {
    pub fn at(frame: usize) -> Result<Self, Error> {
        let phase = (frame % 240) as f32 * std::f32::consts::TAU / 240.;
        let x = 0.65 * phase.sin();
        let stride = (4. * phase).sin();
        let mut parts = [([0.; 3], [0.; 3], 0.1); 16];
        parts[0] = ([x, 0.8, 0.], [x, 1.18, 0.], 0.17);
        parts[1] = ([x, 1.48, 0.], [x, 1.48, 0.], 0.15);
        parts[2] = ([x - 0.1, 0.77, 0.], [x + 0.1, 0.77, 0.], 0.11);
        for (side, range) in [(-1., 3), (1., 8)] {
            let z = side * stride;
            let hip = [x + side * 0.095, 0.73, 0.];
            let knee = [hip[0], 0.41, 0.12 * z + 0.07];
            let ankle = [hip[0], 0.10 + 0.07 * z.max(0.), 0.27 * z];
            let shoulder = [x + side * 0.19, 1.20, 0.];
            let elbow = [x + side * 0.22, 0.97, -0.13 * z];
            let hand = [elbow[0], 0.78, 0.05 - 0.22 * z];
            parts[range..range + 5].copy_from_slice(&[
                (hip, knee, 0.085),
                (knee, ankle, 0.065),
                (ankle, [ankle[0], ankle[1] - 0.03, ankle[2] + 0.1], 0.065),
                (shoulder, elbow, 0.06),
                (elbow, hand, 0.05),
            ]);
        }
        for (i, part) in parts[13..].iter_mut().enumerate() {
            let r = 0.27 + 0.075 * i as f32;
            let p = [
                -1.0 + i as f32,
                r * (1.0 + 0.42 * i as f32),
                0.62 + 0.16 * (phase + i as f32).sin(),
            ];
            *part = (p, p, r);
        }
        let mut capsules = [ShadowCapsule::new([0.; 3], [0.; 3], 1.)?; 16];
        let mut transforms = [Transform::IDENTITY; 16];
        for (i, (a, b, r)) in parts.into_iter().enumerate() {
            capsules[i] = ShadowCapsule::new(a, b, r)?;
            let v: [f32; 3] = std::array::from_fn(|j| b[j] - a[j]);
            let length = v[0].hypot(v[1]).hypot(v[2]);
            let axis = if length > 0. {
                v.map(|v| v / length)
            } else {
                [0., 1., 0.]
            };
            // Stretch an inspection sphere along the exact same animated bone.
            let rows = std::array::from_fn(|j| {
                std::array::from_fn(|k| {
                    if k == 3 {
                        (a[j] + b[j]) * 0.5
                    } else {
                        (if j == k { r } else { 0. } + 0.5 * length * axis[j] * axis[k]) / 0.4
                    }
                })
            });
            transforms[i] = Transform::new(rows)?;
        }
        Ok(Self {
            capsules,
            transforms,
        })
    }
}

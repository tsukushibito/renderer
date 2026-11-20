//! Adapt the Bistro assets, visibility and history to the public Forward recipe.

use renderer_pipeline::pbr_forward::{self, Draw, Frame, Material, ShadowSource};

use super::{
    Bloom, BufferUpdates, Built, CASCADES, Camera, Error, Frustum, GraphPool, Options, Resident,
    Scene, SceneImages, ShadowKey, ShadowUpdate, Workspace, finish_frame, import_environment,
    import_frame, select_draws, shadow, shadow_casters,
};

pub(super) struct Inputs {
    materials: Vec<Material>,
    opaque: Vec<Draw>,
    transparent: Vec<Draw>,
    casters: [Vec<Draw>; shadow::CASCADE_COUNT],
}

impl Inputs {
    pub(super) fn new(scene: &Scene) -> Self {
        let primitives = scene.primitives.len();
        Self {
            materials: Vec::with_capacity(scene.materials.len()),
            opaque: Vec::with_capacity(primitives),
            transparent: Vec::with_capacity(primitives),
            casters: std::array::from_fn(|_| Vec::with_capacity(primitives)),
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "construct a frame from the same application inputs as the comparison modes"
)]
pub(super) fn build(
    pool: &mut GraphPool,
    workspace: &mut Workspace,
    resident: &Resident,
    scene: &Scene,
    options: &Options,
    camera: Camera,
    camera_bytes: &[u8; 128],
    capture: bool,
) -> Result<Built, Error> {
    let mut graph = pool.begin()?;
    let camera_target = import_frame(&mut graph, workspace, resident)?;
    let [irradiance, reflection, brdf_lut] = import_environment(&mut graph, resident)?;
    let [strength, yaw_radians, max_prefilter_lod] = resident.environment_parameters;
    let composition = workspace.prepared.select(options);
    let bloom = composition.bloom();
    let recipe = composition.standard_forward();
    let word = |index: usize| {
        f32::from_le_bytes(camera_bytes[index * 4..index * 4 + 4].try_into().unwrap())
    };
    let view = recipe.prepare_view(
        pbr_forward::Camera {
            position: camera.position,
            target: camera.target,
            fov_y_degrees: camera.fov,
            near: word(19),
            far: word(23),
        },
        pbr_forward::Sun::from_normalized(
            [word(20), word(21), word(22)],
            [word(24), word(25), word(26)],
        )?,
        resident.standard_bounds,
        pbr_forward::Environment {
            irradiance,
            reflection,
            brdf_lut,
            strength,
            yaw_radians,
            max_prefilter_lod: max_prefilter_lod as u32,
        },
    )?;
    let frustum = Frustum::new(*view.matrix(), resident.scene_bounds);
    select_draws(
        [&mut workspace.opaque, &mut workspace.blended],
        scene,
        resident,
        options,
        camera,
        &frustum,
    );

    let inputs = workspace
        .standard_inputs
        .as_mut()
        .expect("workspace prepared for the standard recipe");
    inputs.materials.clear();
    for (material, &factors) in scene.materials.iter().zip(&workspace.materials) {
        let [base, normal, orm, emissive] = material.textures;
        let texture = |index: usize| {
            graph.sampled_image_view(
                workspace.images[index],
                0,
                scene.images[index].mips.len() as u32,
            )
        };
        inputs.materials.push(Material {
            factors,
            factor_offset: 0,
            textures: [
                texture(base)?,
                texture(normal)?,
                texture(orm)?,
                texture(emissive)?,
            ],
        });
    }
    let draw = |index: usize| Draw {
        vertices: workspace.vertices[index],
        indices: workspace.indices[index],
        first_index: 0,
        index_count: scene.primitives[index].index_count,
        material: scene.primitives[index].material,
    };
    inputs.opaque.clear();
    inputs
        .opaque
        .extend(workspace.opaque.iter().copied().map(draw));
    inputs.transparent.clear();
    inputs
        .transparent
        .extend(workspace.blended.iter().copied().map(draw));

    let matrices = view.shadow_matrices();
    let keys = matrices.map(|matrix| ShadowKey::new(matrix, options.alpha_only, options.shadows));
    let mut targets = [None; shadow::CASCADE_COUNT];
    let mut reused = [false; shadow::CASCADE_COUNT];
    for (cascade, casters) in inputs.casters.iter_mut().enumerate() {
        casters.clear();
        if let Some(old) = workspace
            .shadow_cache
            .as_ref()
            .and_then(|cache| cache.entries[cascade].as_ref())
        {
            targets[cascade] = Some(graph.import_image(CASCADES[cascade], &old.image)?);
            reused[cascade] = old.key == keys[cascade];
        }
        if !reused[cascade] {
            let clip = shadow::CascadeClip::new(&matrices[cascade], resident.scene_bounds);
            casters.extend(shadow_casters(scene, resident, options, clip).map(draw));
        }
    }

    let frame = Frame {
        view: &view,
        materials: &inputs.materials,
        opaque: &inputs.opaque,
        transparent: &inputs.transparent,
        shadows: std::array::from_fn(|cascade| {
            if reused[cascade] {
                ShadowSource::Reuse(targets[cascade].unwrap())
            } else {
                ShadowSource::Render {
                    target: targets[cascade],
                    casters: &inputs.casters[cascade],
                }
            }
        }),
        camera_target: Some(camera_target),
        exposure: 2.0,
    };
    let (images, camera) = if bloom == Bloom::Off {
        let frame = recipe.add_passes(&mut graph, frame)?;
        (
            SceneImages {
                hdr: frame.hdr,
                depth: frame.depth,
                output: frame.sdr,
                bloom: None,
                shadows: frame.shadows,
            },
            frame.camera,
        )
    } else {
        let scene = recipe.add_scene(&mut graph, frame)?;
        let enhanced = bloom.append(&mut graph, scene.hdr)?;
        let output = recipe.add_output(&mut graph, enhanced, 2.0)?;
        (
            SceneImages {
                hdr: scene.hdr,
                depth: scene.depth,
                output: output.sdr,
                bloom: Some(enhanced),
                shadows: scene.shadows,
            },
            scene.camera,
        )
    };
    let mut updates = workspace
        .shadow_cache
        .as_ref()
        .map(|_| [None; shadow::CASCADE_COUNT]);
    if let Some(updates) = &mut updates {
        for (cascade, image) in images.shadows.into_iter().enumerate() {
            if !reused[cascade] {
                graph.export_image(image)?;
                updates[cascade] = Some(ShadowUpdate {
                    key: keys[cascade],
                    handle: image,
                });
            }
        }
    }
    graph.export_buffer(camera)?;
    let draws =
        (capture && !options.windowed).then_some(inputs.opaque.len() + inputs.transparent.len());
    finish_frame(
        graph,
        images,
        BufferUpdates {
            camera,
            tiles: None,
        },
        updates,
        draws,
        options,
        capture,
    )
}

//! Connect Bistro's materials, draws and history to the public PBR recipes.

use super::{Scene, shadow};
use renderer_pipeline::pbr::{Draw, Material};

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::{
    Built, Camera, Error, GraphPool, Options, Resident, SceneImages, SubmissionMode, Workspace,
    caster_revision, finish_frame, import_environment, import_frame, material_visible,
    select_draws, shadow_casters,
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer_pipeline::pbr::{self, Frame, IndexedDraw, ShadowSource};

pub(super) struct Inputs {
    materials: Vec<Material>,
    opaque: Vec<Draw>,
    transparent: Vec<Draw>,
    casters: [Vec<Draw>; shadow::CASCADE_COUNT],
}

impl Inputs {
    pub(super) fn new(scene: &Scene, instance_draws: usize) -> Self {
        let primitives = scene.primitives.len();
        Self {
            materials: Vec::with_capacity(scene.materials.len()),
            opaque: Vec::with_capacity(primitives + instance_draws),
            transparent: Vec::with_capacity(primitives),
            casters: std::array::from_fn(|_| Vec::with_capacity(primitives + instance_draws)),
        }
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "construct a frame from the same application inputs as the comparison modes"
)]
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
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
    let (recipe, bloom) = workspace.prepared.select(options).standard();
    let word = |index: usize| {
        f32::from_le_bytes(camera_bytes[index * 4..index * 4 + 4].try_into().unwrap())
    };
    let view = recipe.prepare_view(
        pbr::Camera {
            position: camera.position,
            target: camera.target,
            fov_y_degrees: camera.fov,
            near: word(19),
            far: word(23),
        },
        pbr::Sun::from_normalized(
            [word(20), word(21), word(22)],
            [word(24), word(25), word(26)],
        )?,
        workspace
            .placements
            .as_ref()
            .map_or(resident.standard_bounds, |placements| {
                placements.shadow_bounds
            }),
        pbr::Environment {
            irradiance,
            reflection,
            brdf_lut,
            strength,
            yaw_radians,
            max_prefilter_lod: max_prefilter_lod as u32,
        },
    )?;
    // GPU culling reads the current camera before the recipe adds its passes.
    if options.submission == SubmissionMode::Indirect {
        graph.upload_buffer(
            "camera update boundary",
            camera_target,
            0,
            view.uniform_bytes(),
        )?;
    }
    let include_material = |material| material_visible(options, scene, material);
    let visible = |index: usize| view.visible(resident.standard_primitive_bounds[index]);
    let packed = resident
        .packed
        .as_ref()
        .map(|packed| {
            packed.prepare_frame(
                &mut graph,
                camera_target,
                &mut workspace.indirect,
                include_material,
            )
        })
        .transpose()?;
    select_draws(
        [&mut workspace.opaque, &mut workspace.blended],
        scene,
        resident,
        options,
        camera,
        visible,
    );

    let inputs = workspace
        .standard_inputs
        .as_mut()
        .expect("workspace prepared for the public recipes");
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
    let geometry = |index: usize| IndexedDraw {
        vertices: workspace.vertices[index],
        indices: workspace.indices[index],
        first_index: 0,
        index_count: scene.primitives[index].index_count,
        material: scene.primitives[index].material,
    };
    let draw = |index| Draw::Indexed(geometry(index));

    inputs.opaque.clear();
    let indirect = if let Some(packed) = &packed {
        packed.append_pbr_draws(&mut inputs.opaque, visible, include_material);
        packed.is_indirect()
    } else {
        inputs
            .opaque
            .extend(workspace.opaque.iter().copied().map(draw));
        false
    };
    if let Some(placements) = &workspace.placements {
        placements.append_draws(
            options.instances,
            geometry(placements.fixture.primitive),
            &mut inputs.opaque,
        );
    }
    inputs.transparent.clear();
    inputs
        .transparent
        .extend(workspace.blended.iter().copied().map(draw));

    let history_frame = workspace
        .shadow_reuse
        .as_ref()
        .map(|reuse| {
            // This workspace retains the same immutable scene and instance grid.
            reuse.history.begin_view(
                &mut graph,
                &view,
                [caster_revision(options); pbr::CASCADE_COUNT],
            )
        })
        .transpose()?;
    for (cascade, casters) in inputs.casters.iter_mut().enumerate() {
        casters.clear();
        if !history_frame
            .as_ref()
            .is_some_and(|frame| frame.is_reused(cascade))
        {
            casters.extend(
                shadow_casters(scene, options, |index| {
                    view.cascade_overlaps(cascade, resident.standard_primitive_bounds[index])
                        .unwrap_or(true)
                })
                .map(draw),
            );
            // Both fixture modes submit the same caster geometry. The grid is
            // small enough to submit to each cascade without filtering batches.
            if options.shadows
                && let Some(placements) = &workspace.placements
            {
                placements.append_draws(
                    options.instances,
                    geometry(placements.fixture.primitive),
                    casters,
                );
            }
        }
    }
    let frame = Frame {
        view: &view,
        materials: &inputs.materials,
        opaque: &inputs.opaque,
        transparent: &inputs.transparent,
        shadows: std::array::from_fn(|cascade| match &history_frame {
            Some(frame) => frame.source(cascade, &inputs.casters[cascade]),
            None => ShadowSource::Render {
                target: None,
                casters: &inputs.casters[cascade],
            },
        }),
        camera_target: Some(camera_target),
        transforms: workspace
            .placements
            .as_ref()
            .map_or(&[], |placements| &placements.transforms[..]),
        exposure: 2.0,
    };
    let lights = resident
        .local_lights
        .as_ref()
        .map_or(&[][..], |preset| &preset[..]);
    let images = recipe.add_scene(&mut graph, frame, lights)?;
    let processed = bloom.add_passes(&mut graph, images.hdr)?;
    let output = recipe.add_output(&mut graph, processed, 2.0)?;
    let updates = history_frame
        .map(|frame| frame.export(&mut graph))
        .transpose()?;
    graph.export_buffer(images.camera)?;

    let draws = (capture && !options.windowed && !indirect)
        .then_some(inputs.opaque.len() + inputs.transparent.len());
    finish_frame(
        graph,
        SceneImages {
            hdr: images.hdr,
            depth: images.depth,
            output: output.sdr,
            bloom: (processed != images.hdr).then_some(processed),
            shadows: images.shadows,
        },
        images.camera,
        updates,
        draws,
        options,
        capture,
    )
}

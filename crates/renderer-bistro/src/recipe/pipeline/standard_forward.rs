//! Connect Bistro's materials, draws and history to the public PBR recipes.

use super::{Scene, shadow};
use renderer::pbr::{Draw, GraphMaterial};

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::{
    Built, Error, GraphPool, Options, Resident, SceneImages, SubmissionMode, Workspace,
    caster_revision, finish_frame, import_environment, import_frame, material_visible,
    select_draws, shadow_casters,
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::pbr::{self, FrameShadows, IndexedDraw, PbrDrawInput, ShadowSource};

use renderer::material::{
    MaterialHandle, MaterialInstance, MaterialTextures, Materials, PbrMaterial,
};
use renderer::pbr::ResidentMaterialUniform;
use renderer_core::graph::{Graph, GraphError};
use renderer_core::runtime::ManagedImage;

/// Cooked tables and GPU culling use dense asset indices. Keep their mapping to
/// shareable pipeline handles at the asset boundary.
pub(super) struct SurfaceTable {
    materials: Materials,
    handles: Vec<MaterialHandle>,
    maps: Vec<[usize; 4]>,
}
impl SurfaceTable {
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub(super) fn new(
        scene: &Scene,
        uniforms: &[ResidentMaterialUniform],
        images: &[ManagedImage],
    ) -> Result<Self, GraphError> {
        let mut table = Self {
            materials: Materials::default(),
            handles: Vec::with_capacity(uniforms.len()),
            maps: scene.materials.iter().map(|m| m.textures).collect(),
        };
        table.refresh(uniforms, images)?;
        Ok(table)
    }
    pub(super) fn refresh(
        &mut self,
        uniforms: &[ResidentMaterialUniform],
        images: &[ManagedImage],
    ) -> Result<(), GraphError> {
        for (index, (maps, uniform)) in self.maps.iter().zip(uniforms).enumerate() {
            let [base_color, normal, orm, emissive] = maps.map(|i| images[i].snapshot().clone());
            let material = PbrMaterial::resident(
                uniform,
                MaterialTextures {
                    base_color,
                    normal,
                    orm,
                    emissive,
                },
            )?;
            if let Some(&handle) = self.handles.get(index) {
                self.materials.update_resident_pbr(handle, material)?
            } else {
                self.handles
                    .push(self.materials.create_resident_pbr(material))
            }
        }
        Ok(())
    }
    fn resolve(&self, graph: &mut Graph, out: &mut Vec<GraphMaterial>) -> Result<(), GraphError> {
        for &handle in &self.handles {
            let MaterialInstance::Pbr(p) = self.materials.get(handle)? else {
                unreachable!("Bistro cooks PBR materials")
            };
            out.push(p.resolve(graph, "material factors", None)?)
        }
        Ok(())
    }
}
pub(super) struct Inputs {
    materials: Vec<GraphMaterial>,
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
    camera_input: &crate::shared::camera::CameraInput,
    capture: bool,
) -> Result<Built, Error> {
    let camera = camera_input.camera;
    let mut graph = pool.begin()?;
    let camera_target = import_frame(&mut graph, workspace, resident)?;
    let [irradiance, reflection, brdf_lut] = import_environment(&mut graph, resident)?;
    let [strength, yaw_radians, max_prefilter_lod] = resident.environment_parameters;
    let (recipe, bloom) = workspace.prepared.select(options).standard();
    let view = recipe.prepare_view(
        pbr::ViewInput::new(
            camera_input.matrix,
            camera.position,
            std::array::from_fn(|axis| {
                f64::from(camera.target[axis]) - f64::from(camera.position[axis])
            }),
            [camera_input.near, camera_input.far],
            pbr::Projection::SymmetricPerspective {
                vertical_fov_degrees: camera.fov,
                aspect: options.width as f32 / options.height as f32,
            },
        )?,
        pbr::Sun::from_normalized(camera_input.direction_to_light, camera_input.irradiance)?,
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
    resident
        .surfaces
        .resolve(&mut graph, &mut inputs.materials)?;
    let geometry = |index: usize| IndexedDraw {
        vertices: workspace.vertices[index],
        indices: workspace.indices[index],
        first_index: 0,
        index_count: scene.primitives[index].index_count,
        material: scene.primitives[index].material,
    };
    let draw = |index| Draw::WorldSpaceIndexed(geometry(index));

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

    let history_frame = if options.shadows {
        let history_frame = workspace
            .shadow_reuse
            .as_ref()
            .map(|reuse| {
                // The workspace retains the same scene and instance grid.
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
                // Both fixture modes submit the same caster geometry.
                if let Some(placements) = &workspace.placements {
                    placements.append_draws(
                        options.instances,
                        geometry(placements.fixture.primitive),
                        casters,
                    );
                }
            }
        }
        history_frame
    } else {
        None
    };
    let shadows = if options.shadows {
        FrameShadows::Cascades(std::array::from_fn(|cascade| match &history_frame {
            Some(frame) => frame.source(cascade, &inputs.casters[cascade]),
            None => ShadowSource::Render {
                target: None,
                casters: &inputs.casters[cascade],
            },
        }))
    } else {
        FrameShadows::Disabled(
            resident
                .defaults
                .as_ref()
                .ok_or("unshadowed Bistro needs its prepared defaults")?
                .import(&mut graph)?
                .unshadowed()?,
        )
    };
    let mut frame = PbrDrawInput::with_shadows(
        &view,
        &inputs.materials,
        &inputs.opaque,
        &inputs.transparent,
        shadows,
    );
    frame.camera_target = Some(camera_target);
    frame.transforms = workspace
        .placements
        .as_ref()
        .map_or(&[], |p| &p.transforms[..]);
    let lights = resident
        .local_lights
        .as_ref()
        .map_or(&[][..], |preset| &preset[..]);
    let images = recipe.draw_hdr(&mut graph, frame.with_lights(lights))?;
    let processed = bloom.add_passes(&mut graph, images.hdr)?;
    let output = recipe.write_output(&mut graph, processed, 2.0)?;
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

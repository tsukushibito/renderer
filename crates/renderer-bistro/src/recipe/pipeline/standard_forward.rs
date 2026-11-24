//! Connect Bistro's materials, draws and history to the public PBR recipes.

use super::{Scene, shadow};
use renderer::pbr::{Draw, GraphMaterial};

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::{
    Built, Error, GraphPool, Options, Resident, SceneImages, SubmissionMode, Workspace,
    caster_revision, finish_frame, import_environment, import_frame, material_visible,
    select_draws, shadow_casters, sun_caster_revision,
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::pbr::{self, IndexedDraw, PbrDrawInput, ShadowSource, SunShadows};

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
    rect_casters: Vec<Draw>,
    casters: [Vec<Draw>; shadow::CASCADE_COUNT],
}

impl Inputs {
    pub(super) fn new(scene: &Scene, instance_draws: usize, rect_shadows: bool) -> Self {
        let primitives = scene.primitives.len();
        Self {
            materials: Vec::with_capacity(scene.materials.len()),
            opaque: Vec::with_capacity(primitives + instance_draws),
            transparent: Vec::with_capacity(primitives),
            rect_casters: Vec::with_capacity(if rect_shadows {
                primitives + instance_draws
            } else {
                0
            }),
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
    frame_index: usize,
    capture: bool,
) -> Result<Built, Error> {
    let camera = camera_input.camera;
    let mut graph = pool.begin()?;
    let camera_target = import_frame(&mut graph, workspace, resident)?;
    let [irradiance, reflection, brdf_lut] = import_environment(&mut graph, resident)?;
    let [nits_per_unit, yaw_radians, max_prefilter_lod] = resident.environment_parameters;
    let (recipe, bloom) = workspace.prepared.select(options).standard();
    let mut scene_bounds = workspace
        .placements
        .as_ref()
        .map_or(resident.standard_bounds, |p| p.shadow_bounds);
    let progress = (frame_index as f32 / options.frames.saturating_sub(1).max(1) as f32).min(1.);
    let displacement = options
        .reflection_spheres
        .zip(options.reflection_spheres_to)
        .map(|(start, end)| std::array::from_fn::<_, 3, _>(|i| (end[i] - start[i]) * progress));
    // The two inspection spheres are appended after the cooked primitives.
    let sphere_start = scene.primitives.len().saturating_sub(2);
    let mut moving_bounds = [scene_bounds; 2];
    let mut motion = None;
    if let Some([x, y, z]) = displacement {
        motion = Some(pbr::Transform::new([
            [1., 0., 0., x],
            [0., 1., 0., y],
            [0., 0., 1., z],
        ])?);
        for (index, bounds) in moving_bounds.iter_mut().enumerate() {
            let source = resident.standard_primitive_bounds[sphere_start + index];
            let translated = |point: [f32; 3]| [point[0] + x, point[1] + y, point[2] + z];
            *bounds = pbr::Bounds::new(translated(source.min()), translated(source.max()))?;
            scene_bounds = pbr::Bounds::new(
                std::array::from_fn(|i| scene_bounds.min()[i].min(bounds.min()[i])),
                std::array::from_fn(|i| scene_bounds.max()[i].max(bounds.max()[i])),
            )?;
        }
    }
    let bounds = |i: usize| {
        if motion.is_some() && i >= sphere_start {
            moving_bounds[i - sphere_start]
        } else {
            resident.standard_primitive_bounds[i]
        }
    };
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
        pbr::Sun::from_normalized_rgb_lux(
            camera_input.direction_to_light,
            camera_input.irradiance,
        )?,
        scene_bounds,
        pbr::Environment {
            reflection_metadata: graph
                .import_buffer("reflection metadata", &resident.reflection_metadata)?,
            local_reflections: super::import_reflections(&mut graph, resident)?.map(|atlas| {
                pbr::LocalReflections {
                    atlas,
                    blend: camera_input.scenario_blend,
                    view: options.reflection_debug,
                }
            }),
            diffuse_lighting: options.diffuse_lighting(camera_input.scenario_blend),
            irradiance,
            reflection,
            brdf_lut,
            nits_per_unit,
            yaw_radians,
            max_prefilter_lod: max_prefilter_lod as u32,
        },
    )?;
    let view = if let Some(clear) = options.sweep_clear(camera_input.scenario_blend) {
        view.with_clear_color(clear)?
    } else {
        view
    };
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
    let visible = |index: usize| view.visible(bounds(index));
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
    let draw = |index| {
        if motion.is_some() && index >= sphere_start {
            Draw::Instanced {
                geometry: geometry(index),
                first_transform: 0,
                instance_count: 1,
            }
        } else if let Some(address) = scene.primitives[index].surface {
            Draw::Lightmapped {
                geometry: geometry(index),
                surface: pbr::DrawLightmap::new(
                    workspace.lightmap_uv[index].unwrap(),
                    workspace.lightmap_pages[address.page],
                    address,
                ),
            }
        } else {
            Draw::WorldSpaceIndexed(geometry(index))
        }
    };

    inputs.opaque.clear();
    let indirect = if let Some(packed) = &packed {
        packed.append_pbr_draws(
            &mut inputs.opaque,
            visible,
            include_material,
            &workspace.lightmap_pages,
        );
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
                let pose_revision = motion.map_or(0, |_| u64::from(progress.to_bits()) << 32);
                let revision = sun_caster_revision(options) | pose_revision;
                reuse
                    .history
                    .begin_view(&mut graph, &view, [revision; pbr::CASCADE_COUNT])
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
                        view.cascade_overlaps(cascade, bounds(index))
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
        SunShadows::Cascades(std::array::from_fn(|cascade| match &history_frame {
            Some(frame) => frame.source(cascade, &inputs.casters[cascade]),
            None => ShadowSource::Render {
                target: None,
                casters: &inputs.casters[cascade],
            },
        }))
    } else {
        SunShadows::Disabled(
            resident
                .defaults
                .as_ref()
                .ok_or("unshadowed Bistro needs its prepared defaults")?
                .import(&mut graph)?
                .unshadowed()?,
        )
    };
    let lights = resident
        .local_lights
        .as_ref()
        .map_or(&[][..], |preset| &preset[..]);
    let has_shadowed_rect = lights
        .iter()
        .any(|l| matches!(l, pbr::LocalLight::ShadowedRect(_)));
    let rect_frame = if has_shadowed_rect {
        let revision = caster_revision(options);
        workspace
            .shadow_reuse
            .as_mut()
            .map(|h| {
                h.rect_history.begin(&mut graph, lights, |face, _| {
                    let moving =
                        motion.is_some() && moving_bounds.iter().any(|&b| face.overlaps(b));
                    revision
                        | if moving {
                            u64::from(progress.to_bits()) << 32 | 8
                        } else {
                            0
                        }
                })
            })
            .transpose()?
            .flatten()
    } else {
        None
    };
    inputs.rect_casters.clear();
    if has_shadowed_rect
        && !rect_frame
            .as_ref()
            .is_some_and(|h| h.faces_to_render() == 0)
    {
        inputs
            .rect_casters
            .extend(shadow_casters(scene, options, |_| true).map(draw));
        if let Some(placements) = &workspace.placements {
            placements.append_draws(
                options.instances,
                geometry(placements.fixture.primitive),
                &mut inputs.rect_casters,
            );
        }
    }
    let mut frame = PbrDrawInput::with_sun_shadows(
        &view,
        &inputs.materials,
        &inputs.opaque,
        &inputs.transparent,
        &inputs.rect_casters,
        shadows,
    );
    frame.camera_target = Some(camera_target);
    frame.transforms = workspace
        .placements
        .as_ref()
        .map_or(motion.as_slice(), |p| &p.transforms[..]);
    if let Some(history) = &rect_frame {
        frame.use_rect_shadow_history(history);
    }
    let images = recipe.draw_hdr(&mut graph, frame.with_lights(lights))?;
    let processed = bloom.add_passes(&mut graph, images.hdr)?;
    let output = recipe.write_output(&mut graph, processed, 2.0)?;
    let updates = history_frame
        .map(|frame| frame.export(&mut graph))
        .transpose()?;
    graph.export_buffer(images.camera)?;

    let draws = (capture && !options.windowed && !indirect)
        .then_some(inputs.opaque.len() + inputs.transparent.len());
    let rect_updates = rect_frame.map(|f| f.export(&mut graph)).transpose()?;
    let mut built = finish_frame(
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
    )?;
    built.rect_updates = rect_updates;
    Ok(built)
}

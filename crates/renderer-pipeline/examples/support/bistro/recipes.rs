//! Prepared Bistro scene, lighting and output passes.

use std::num::NonZeroUsize;

use renderer::{
    capacity::{GraphRequirements, Preparation, ResourceClass, WorkingSet},
    graph::{
        Attachment, Binding, BufferDesc, BufferHandle, Graph, GraphError, ImageDesc, ImageHandle,
        ImageView, LoadOp, RasterAttachments, RasterPass, SampleCount, StoreOp,
    },
    program::{MAX_RASTER_BINDINGS, ManagedProgram, ManagedRasterProgram, RasterPipelineRequest},
};
use renderer_pipeline::{passes::output::PreparedOutput, pbr_forward};

use super::{
    Error, Options, PipelineMode, SubmissionMode, asset::Scene, bloom::Bloom, deferred,
    pipeline::indirect::Layout, program, shadow,
};

pub(super) struct Prepared {
    preparation: Preparation,
    recipe: Recipe,
    hdr: ImageDesc,
    depth: ImageDesc,
    bloom: Bloom,
    clear: [f32; 4],
    mode: PipelineMode,
    pub opaque: ManagedRasterProgram,
    pub transparent: ManagedRasterProgram,
    pub instances: Option<ManagedRasterProgram>,
    pub shadow: ManagedRasterProgram,
}

enum Recipe {
    Forward(Box<pbr_forward::Prepared>),
    Comparison(PreparedOutput),
}

fn uses_standard_forward(options: &Options) -> bool {
    #[cfg(feature = "development-validation")]
    if options.offset_shadow_receiver {
        return false;
    }
    options.pipeline == PipelineMode::Forward
        && options.sample_count == SampleCount::X1
        && options.submission == SubmissionMode::Direct
        && !options.instances.enabled()
        && options.shader_assets.is_none()
}

impl Prepared {
    pub fn new(
        options: &Options,
        scene: &Scene,
        layout: Option<&Layout>,
        clear: [f32; 4],
    ) -> Result<Self, Error> {
        options.validate_lighting_extent()?;
        let concurrent = NonZeroUsize::new(options.inflight).ok_or("Bistro needs a frame slot")?;
        let mut preparation = Preparation::new(concurrent);
        let [width, height] = [options.width, options.height];
        let hdr = ImageDesc::rgba16_float(width, height);
        let depth = ImageDesc::depth32(width, height).with_sample_count(options.sample_count);
        let shadow_extent = shadow::Parameters::default().resolution;
        let is_deferred = options.pipeline == PipelineMode::Deferred;
        let is_msaa = options.sample_count == SampleCount::X4;
        let primitives = scene.primitives.len();
        let standard = if uses_standard_forward(options) {
            let forward = pbr_forward::prepare(
                pbr_forward::Config {
                    extent: [width, height],
                    clear_color: clear,
                    shadows: pbr_forward::ShadowSettings {
                        enabled: options.shadows,
                        ..pbr_forward::ShadowSettings::default()
                    },
                    output: options.sdr_output(),
                },
                pbr_forward::SceneLimits {
                    frames_in_flight: concurrent,
                    max_draws: primitives,
                    max_materials: scene.materials.len(),
                    max_casters_per_cascade: primitives,
                },
            )?;
            let requirements = forward.preparation();
            let working_set = requirements.working_set();
            let declaration = requirements.graph_requirements();
            // Bistro supplies its existing canonical camera target on each
            // frame, including frames whose shadow maps are reused.
            let images: Vec<_> = working_set
                .resources
                .iter()
                .copied()
                .filter(|class| matches!(class, ResourceClass::Image { .. }))
                .collect();
            preparation.add_requirements(
                GraphRequirements {
                    // The imported camera already counts among Bistro's
                    // resident buffers when sizing the complete Graph.
                    resources: declaration.resources - 1,
                    ..declaration
                },
                WorkingSet {
                    resources: &images,
                    ..working_set
                },
            )?;
            Some(forward)
        } else {
            preparation.reserve_image(
                ImageDesc::depth32(shadow_extent, shadow_extent),
                shadow::CASCADE_COUNT,
            )?;
            preparation.reserve_image(hdr, 1)?;
            preparation.reserve_image(depth, 1)?;
            None
        };
        let mut targets = if standard.is_some() {
            0
        } else {
            shadow::CASCADE_COUNT + 2
        };
        if is_msaa {
            preparation.reserve_image(hdr.with_sample_count(options.sample_count), 1)?;
            targets += 1;
        }
        if is_deferred {
            preparation.reserve_image(hdr, deferred::PLANES.len())?;
            targets += deferred::PLANES.len();
        }
        if let Some(images) = options.bloom.images(hdr) {
            for desc in images {
                preparation.reserve_image(desc, 1)?;
            }
            targets += images.len();
        }

        let (opaque, transparent, shadow) = if standard.is_some() {
            use renderer_pipeline::passes::pbr::{blend_program, opaque_program, shadow_program};
            (opaque_program(), blend_program(), shadow_program())
        } else {
            let opaque = if is_deferred {
                program::deferred::gbuffer_program()
            } else {
                options.forward_program(false).into_owned()
            };
            (
                opaque,
                options.forward_program(true).into_owned(),
                options.shadow_program().into_owned(),
            )
        };
        let instances = options
            .instances
            .enabled()
            .then(program::instancing_program);
        let mut raster = Vec::with_capacity(5);
        if standard.is_none() {
            raster.push(shadow.clone().into());
            for program in [&opaque, &transparent]
                .into_iter()
                .chain(instances.as_ref())
            {
                raster.push(RasterPipelineRequest {
                    program: program.clone(),
                    sample_count: options.sample_count,
                });
            }
            if is_deferred {
                raster.push(program::deferred::deferred_lighting_program().into());
            }
        }
        let mut compute = options.bloom.programs().to_vec();
        let mut dispatches = options.bloom.images(hdr).map_or(0, |images| images.len());
        if options.pipeline == PipelineMode::ForwardPlus {
            add_tile_program(&mut compute)?;
            dispatches += 1;
        }
        let mut groups = 0;
        if let Some(layout) = layout.filter(|_| options.submission == SubmissionMode::Indirect) {
            groups = layout.group_count();
            let resources: Vec<_> = layout
                .transient_buffers(options)
                .map(|(size, count)| ResourceClass::Buffer {
                    desc: BufferDesc { size },
                    count,
                })
                .collect();
            preparation.add_working_set(WorkingSet {
                resources: &resources,
                ..WorkingSet::default()
            })?;
            compute.extend([
                program::visibility::visibility_program(),
                ManagedProgram::indexed_indirect_encoder(),
            ]);
            dispatches += 1 + groups;
        }
        preparation.add_working_set(WorkingSet {
            resources: &[],
            raster: &raster,
            compute: &compute,
        })?;

        let recipe = if let Some(forward) = standard {
            preparation.add_graph_requirements(GraphRequirements {
                resources: targets,
                passes: dispatches,
                compute_dispatches: dispatches,
                bindings: 4 * dispatches,
                accesses: 4 * dispatches,
                name_bytes: (targets + dispatches) * 48,
                ..GraphRequirements::default()
            })?;
            Recipe::Forward(Box::new(forward))
        } else {
            let instance_draws = options.instances.draws();
            let shadow_draws = primitives
                .checked_mul(shadow::CASCADE_COUNT)
                .ok_or("Bistro shadow draw capacity overflow")?;
            let instance_words = instance_draws
                .checked_mul(program::INSTANCE_INLINE_BYTES as usize / size_of::<u32>())
                .ok_or("Bistro instance constant capacity overflow")?;
            // Budget every potential declaration, before Graph compression.
            let draws =
                primitives + shadow_draws + groups + instance_draws + usize::from(is_deferred);
            let extra_buffers = usize::from(groups != 0) + groups * 2;
            // Each encoder also uploads its validated command template.
            let passes = shadow::CASCADE_COUNT + 3 + dispatches + groups;
            preparation.add_graph_requirements(GraphRequirements {
                resources: targets + extra_buffers,
                passes,
                compute_dispatches: dispatches,
                draws,
                bindings: scene.materials.len() * (MAX_RASTER_BINDINGS + 4)
                    + usize::from(instance_draws != 0) * MAX_RASTER_BINDINGS
                    + dispatches * 4
                    + 16,
                accesses: draws * (MAX_RASTER_BINDINGS + 2) + 4 * dispatches + groups + 32,
                attachments: shadow::CASCADE_COUNT + 8,
                raster_constant_words: shadow_draws + instance_words,
                name_bytes: (targets + extra_buffers + passes) * 48,
            })?;
            Recipe::Comparison(PreparedOutput::prepare(
                &mut preparation,
                [width, height],
                options.sdr_output(),
            )?)
        };
        Ok(Self {
            preparation,
            recipe,
            hdr,
            depth,
            bloom: options.bloom,
            clear,
            mode: options.pipeline,
            opaque,
            transparent,
            instances,
            shadow,
        })
    }

    pub fn requirements(&self) -> GraphRequirements {
        self.preparation.graph_requirements()
    }

    pub fn working_set(&self) -> WorkingSet<'_> {
        self.preparation.working_set()
    }

    pub fn uses_standard_forward(&self) -> bool {
        matches!(self.recipe, Recipe::Forward(_))
    }

    pub fn standard_forward(&mut self) -> &mut pbr_forward::Prepared {
        match &mut self.recipe {
            Recipe::Forward(forward) => forward,
            Recipe::Comparison(_) => unreachable!("not the standard Bistro path"),
        }
    }

    pub fn bloom(&self) -> Bloom {
        self.bloom
    }

    pub fn scene(
        &self,
        graph: &mut Graph,
        frame: SceneFrame,
        opaque_draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
        transparent_draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
    ) -> Result<(ImageHandle, ImageHandle), Error> {
        if self.mode != PipelineMode::Deferred {
            return Ok(pbr_forward::add_scene_passes(
                graph,
                [self.hdr.width, self.hdr.height],
                self.clear,
                self.depth.sample_count,
                frame.has_transparency,
                opaque_draws,
                transparent_draws,
            )?);
        }

        let hdr = graph.image("hdr", self.hdr)?;
        let depth = graph.image("depth", self.depth)?;
        let depth_view = graph.image_view(depth, 0, 0)?;
        let hdr_view = graph.image_view(hdr, 0, 0)?;
        deferred::append(
            graph,
            deferred::Inputs {
                hdr: hdr_view,
                depth: depth_view,
                clear: self.clear,
                camera: frame.camera,
                view_projection: frame.view_projection,
                lighting: frame.lighting,
            },
            |raster, _| opaque_draws(raster),
        )?;
        if frame.has_transparency {
            graph.raster(
                "scene_transparent",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[Attachment {
                        view: hdr_view,
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    }],
                    depth: Some(Attachment {
                        view: depth_view,
                        load: LoadOp::Load,
                        store: StoreOp::Store,
                    }),
                },
                transparent_draws,
            )?;
        }
        Ok((hdr, depth))
    }

    pub fn post_process(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
    ) -> Result<(ImageHandle, Option<ImageHandle>), GraphError> {
        let color = self.bloom.append(graph, hdr)?;
        let source: ImageView = graph.image_view(color, 0, 0)?;
        let Recipe::Comparison(output) = &self.recipe else {
            unreachable!("the standard recipe owns its output")
        };
        let output = output.add_passes(graph, source, 2.0)?;
        Ok((output.sdr, (color != hdr).then_some(color)))
    }
}

pub(super) struct SceneFrame {
    pub camera: BufferHandle,
    pub view_projection: [[f32; 4]; 4],
    pub lighting: [Binding; 9],
    pub has_transparency: bool,
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn add_tile_program(compute: &mut Vec<ManagedProgram>) -> Result<(), GraphError> {
    compute.push(program::forward_plus::compute_program());
    Ok(())
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn add_tile_program(_: &mut Vec<ManagedProgram>) -> Result<(), GraphError> {
    Err(GraphError::invalid_input(
        "Bistro Forward+ requires a native Renderer backend feature",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::graph::{GraphCapacity, GraphPool};

    #[test]
    fn prepared_recipes_cover_supported_bistro_profiles() {
        let scene = Scene::fixture(false);
        let layout = Layout::new(&scene).unwrap();
        let mut options = Options::parse([String::from("--fixture")]).unwrap();
        let width = options.width;
        let height = options.height;
        for mode in [
            PipelineMode::Forward,
            #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
            PipelineMode::ForwardPlus,
            PipelineMode::ForwardReference,
            PipelineMode::Deferred,
        ] {
            options.pipeline = mode;
            for samples in [SampleCount::X1, SampleCount::X4] {
                if mode == PipelineMode::Deferred && samples == SampleCount::X4 {
                    continue;
                }
                options.sample_count = samples;
                for submission in [
                    SubmissionMode::Direct,
                    SubmissionMode::Packed,
                    SubmissionMode::Indirect,
                ] {
                    options.submission = submission;
                    for bloom in [Bloom::Off, Bloom::Tent, Bloom::Box] {
                        options.bloom = bloom;
                        for aa in [false, true] {
                            options.aa = aa;
                            let prepared = Prepared::new(
                                &options,
                                &scene,
                                layout.as_ref(),
                                [0.18, 0.18, 0.18, 1.0],
                            )
                            .unwrap_or_else(|error| {
                                panic!(
                                    "{mode:?}/{samples:?}/{submission:?}/{bloom:?}/{aa}: {error}"
                                )
                            });
                            assert_eq!(
                                prepared.uses_standard_forward(),
                                mode == PipelineMode::Forward
                                    && samples == SampleCount::X1
                                    && submission == SubmissionMode::Direct
                            );
                            let set = prepared.working_set();
                            let reserves = |expected| {
                                set.resources.iter().any(|class| {
                                matches!(class, ResourceClass::Image { desc, count } if *desc == expected && *count >= options.inflight)
                            })
                            };
                            assert!(reserves(ImageDesc::rgba8(width, height)));
                            assert!(reserves(ImageDesc::rgba16_float(width, height)));
                            assert!(reserves(
                                ImageDesc::depth32(width, height).with_sample_count(samples)
                            ));
                            assert!(set.raster.iter().any(|request| {
                                request.program.same(&prepared.opaque)
                                    && request.sample_count == samples
                            }));
                            assert!(set.raster.iter().any(|request| {
                                request
                                    .program
                                    .same(&options.sdr_output().pipeline().program)
                            }));
                            if bloom != Bloom::Off {
                                assert_eq!(prepared.bloom, bloom);
                                assert!(reserves(ImageDesc::rgba16_float(
                                    width.div_ceil(4),
                                    height.div_ceil(4)
                                )));
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn forward_instance_batches_prepare_for_both_sample_counts() {
        let scene = Scene::fixture(false);
        let mut options =
            Options::parse(["--fixture", "--instances", "individual"].map(str::to_owned)).unwrap();
        for samples in [SampleCount::X1, SampleCount::X4] {
            options.sample_count = samples;
            let recipe = Prepared::new(&options, &scene, None, [0.18, 0.18, 0.18, 1.0]).unwrap();
            assert!(recipe.working_set().raster.iter().any(|request| {
                request.sample_count == samples
                    && request.program.parameter_size() == program::INSTANCE_INLINE_BYTES
            }));
        }
    }

    #[test]
    fn empty_opaque_batch_resolves_in_both_msaa_branches() {
        let scene = Scene::fixture(false);
        let mut options = Options::parse(["--fixture", "--msaa", "4"].map(str::to_owned)).unwrap();
        options.width = 9;
        options.height = 5;
        let prepared = Prepared::new(&options, &scene, None, [0.18, 0.18, 0.18, 1.0]).unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        for has_transparency in [false, true] {
            let mut graph = pool.begin().unwrap();
            let camera = graph
                .buffer("unused camera", BufferDesc { size: 464 })
                .unwrap();
            let (hdr, _) = prepared
                .scene(
                    &mut graph,
                    SceneFrame {
                        camera,
                        view_projection: [[0.0; 4]; 4],
                        lighting: [Binding {
                            group: 0,
                            binding: 0,
                            resource: renderer::graph::BindingResource::Sampler(
                                renderer::program::SamplerDescriptor::NearestClamp,
                            ),
                        }; 9],
                        has_transparency,
                    },
                    |_| Ok(()),
                    |_| Ok(()),
                )
                .unwrap();
            assert_eq!(
                graph.image_desc(hdr).unwrap(),
                ImageDesc::rgba16_float(9, 5)
            );
            graph.export_image(hdr).unwrap();
            assert_eq!(
                graph.compile().unwrap().pass_count(),
                if has_transparency { 2 } else { 1 }
            );
        }
    }
}

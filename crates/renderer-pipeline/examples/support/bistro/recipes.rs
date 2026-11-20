//! Prepared Bistro scene, lighting and output passes.

use std::num::NonZeroUsize;

use renderer::{
    capacity::{ResourceClass, WorkingSet},
    graph::{
        Attachment, Binding, BufferDesc, BufferHandle, Graph, GraphError, ImageDesc, ImageHandle,
        ImageView, LoadOp, RasterAttachments, RasterPass, SampleCount, StoreOp,
    },
    program::{MAX_RASTER_BINDINGS, ManagedProgram, ManagedRasterProgram, RasterPipelineRequest},
};
use renderer_pipeline::{GraphRequirements, Preparation, standard::graphs::PreparedOutput};

use super::{
    Error, Options, PipelineMode, SubmissionMode, asset::Scene, bloom::Bloom, deferred,
    pipeline::indirect::Layout, program, shadow,
};

pub(super) struct Prepared {
    preparation: Preparation,
    output: PreparedOutput,
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
        let mut targets = shadow::CASCADE_COUNT + 2;
        preparation.reserve_image(
            ImageDesc::depth32(shadow_extent, shadow_extent),
            shadow::CASCADE_COUNT,
        )?;
        preparation.reserve_image(hdr, 1)?;
        preparation.reserve_image(depth, 1)?;
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

        let opaque = if is_deferred {
            program::deferred::gbuffer_program()
        } else {
            options.forward_program(false).into_owned()
        };
        let transparent = options.forward_program(true).into_owned();
        let instances = options
            .instances
            .enabled()
            .then(program::instancing_program);
        let shadow = options.shadow_program().into_owned();
        let mut raster = Vec::with_capacity(5);
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

        let primitives = scene.primitives.len();
        let instance_draws = options.instances.draws();
        let shadow_draws = primitives
            .checked_mul(shadow::CASCADE_COUNT)
            .ok_or("Bistro shadow draw capacity overflow")?;
        let instance_words = instance_draws
            .checked_mul(program::INSTANCE_INLINE_BYTES as usize / size_of::<u32>())
            .ok_or("Bistro instance constant capacity overflow")?;
        // In addition to one main draw per primitive, budget every shadow
        // cascade and every potentially enabled indirect group. The access
        // arena also needs space for duplicate draw accesses before compression.
        let draws = primitives + shadow_draws + groups + instance_draws + usize::from(is_deferred);
        let extra_buffers = usize::from(groups != 0) + groups * 2;
        // Each encoder dispatch also uploads its validated command template.
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
        let output =
            PreparedOutput::prepare(&mut preparation, [width, height], options.sdr_output())?;
        Ok(Self {
            preparation,
            output,
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

    pub fn scene(
        &self,
        graph: &mut Graph,
        frame: SceneFrame,
        opaque_draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
        transparent_draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
    ) -> Result<(ImageHandle, ImageHandle), Error> {
        let hdr = graph.image("hdr", self.hdr)?;
        let depth = graph.image("depth", self.depth)?;
        let depth_view = graph.image_view(depth, 0, 0)?;
        let hdr_view = graph.image_view(hdr, 0, 0)?;
        let samples = self.depth.sample_count;
        let (color_view, resolve) = if samples == SampleCount::X4 {
            let msaa = graph.image("hdr_msaa", self.hdr.with_sample_count(samples))?;
            (graph.image_view(msaa, 0, 0)?, Some(hdr_view))
        } else {
            (hdr_view, None)
        };
        let clear_color = Attachment {
            view: color_view,
            load: LoadOp::Clear(self.clear),
            store: StoreOp::Store,
        };
        let clear_depth = Attachment {
            view: depth_view,
            load: LoadOp::Clear(1.0),
            store: StoreOp::Store,
        };
        let load_color = Attachment {
            load: LoadOp::Load,
            ..clear_color
        };
        let load_depth = Attachment {
            load: LoadOp::Load,
            ..clear_depth
        };

        if self.mode == PipelineMode::Deferred {
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
        } else if let Some(resolve) = resolve {
            let final_opaque = !frame.has_transparency;
            let store = if final_opaque {
                StoreOp::Discard
            } else {
                StoreOp::Store
            };
            let resolves = [Some(resolve)];
            graph.raster(
                if final_opaque {
                    "scene_opaque_resolve"
                } else {
                    "scene_opaque"
                },
                RasterAttachments {
                    color_resolves: if final_opaque { &resolves } else { &[] },
                    colors: &[Attachment {
                        store,
                        ..clear_color
                    }],
                    depth: Some(Attachment {
                        store,
                        ..clear_depth
                    }),
                },
                opaque_draws,
            )?;
        } else {
            graph.attachment_pass(
                "scene_clears",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[clear_color],
                    depth: Some(clear_depth),
                },
            )?;
            graph.raster(
                "scene_opaque",
                RasterAttachments {
                    color_resolves: &[],
                    colors: &[load_color],
                    depth: Some(load_depth),
                },
                opaque_draws,
            )?;
        }
        if frame.has_transparency {
            let store = if resolve.is_some() {
                StoreOp::Discard
            } else {
                StoreOp::Store
            };
            let resolves = resolve.map(Some);
            graph.raster(
                if resolve.is_some() {
                    "scene_transparent_resolve"
                } else {
                    "scene_transparent"
                },
                RasterAttachments {
                    color_resolves: resolves.as_slice(),
                    colors: &[Attachment {
                        store,
                        ..load_color
                    }],
                    depth: Some(Attachment {
                        store,
                        ..load_depth
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
        let output = self.output.add_passes(graph, source, 2.0)?;
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

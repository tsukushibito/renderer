//! Prepared recipe choices for Bistro and its diagnostic views.

use std::num::NonZeroUsize;

use renderer::{
    capacity::{GraphRequirements, Preparation, WorkingSet},
    graph::{
        Attachment, Binding, BufferDesc, BufferHandle, Graph, GraphError, ImageDesc, ImageHandle,
        ImageView, ImportBuffer, LoadOp, RasterAttachments, RasterPass, SampleCount, StoreOp,
    },
    program::{MAX_RASTER_BINDINGS, ManagedRasterProgram, RasterPipelineRequest},
};
use renderer_pipeline::{
    passes::{
        bloom::PreparedBloom,
        output::{OutputImages, PreparedOutput, SdrImage},
    },
    pbr, pbr_deferred, pbr_forward, pbr_forward_plus,
};

use super::{
    Error, Options, PipelineMode, SubmissionMode, asset::Scene, deferred,
    pipeline::indirect::Layout, program, shadow,
};

pub(super) struct Prepared {
    preparation: Preparation,
    recipe: Recipe,
    bloom: PreparedBloom,
}

enum Recipe {
    Standard(Standard),
    Reference(Box<Reference>),
}

pub(super) enum Standard {
    Forward(Box<pbr_forward::Prepared>),
    ForwardPlus(Box<pbr_forward_plus::Prepared>),
    Deferred(Box<pbr_deferred::Prepared>),
}

impl Standard {
    fn preparation(&self) -> &Preparation {
        match self {
            Self::Forward(recipe) => recipe.preparation(),
            Self::ForwardPlus(recipe) => recipe.preparation(),
            Self::Deferred(recipe) => recipe.preparation(),
        }
    }

    pub fn prepare_view(
        &self,
        camera: pbr::Camera,
        sun: pbr::Sun,
        bounds: pbr::Bounds,
        environment: pbr::Environment,
    ) -> Result<pbr::PreparedView, GraphError> {
        match self {
            Self::Forward(recipe) => recipe.prepare_view(camera, sun, bounds, environment),
            Self::ForwardPlus(recipe) => recipe.prepare_view(camera, sun, bounds, environment),
            Self::Deferred(recipe) => recipe.prepare_view(camera, sun, bounds, environment),
        }
    }

    pub fn add_scene(
        &mut self,
        graph: &mut Graph,
        frame: pbr::Frame<'_>,
        lights: &[pbr_forward_plus::PointLight],
        tiles: Option<&ImportBuffer>,
    ) -> Result<pbr::SceneImages, Error> {
        Ok(match self {
            Self::Forward(recipe) => recipe.add_scene(graph, frame)?,
            Self::ForwardPlus(recipe) => recipe.add_scene(
                graph,
                frame,
                lights,
                tiles.ok_or("Forward+ needs initialized tile storage")?,
            )?,
            Self::Deferred(recipe) => recipe.add_scene(graph, frame)?.scene,
        })
    }

    pub fn add_output(
        &self,
        graph: &mut Graph,
        hdr: ImageHandle,
        exposure: f32,
    ) -> Result<OutputImages, GraphError> {
        match self {
            Self::Forward(recipe) => recipe.add_output(graph, hdr, exposure),
            Self::ForwardPlus(recipe) => recipe.add_output(graph, hdr, exposure),
            Self::Deferred(recipe) => recipe.add_output(graph, hdr, exposure),
        }
    }
}

impl Prepared {
    pub fn new(
        options: &Options,
        scene: &Scene,
        layout: Option<&Layout>,
        clear_color: [f32; 4],
    ) -> Result<Self, Error> {
        let concurrent = NonZeroUsize::new(options.inflight).ok_or("Bistro needs a frame slot")?;
        let mut preparation = Preparation::new(concurrent);
        let hdr = ImageDesc::rgba16_float(options.width, options.height);
        let recipe = if standard_pipeline(options) {
            let camera_storage = if options.submission == SubmissionMode::Indirect {
                pbr::CameraStorage::PreUploaded
            } else {
                pbr::CameraStorage::External
            };
            let config = pbr_forward::Config {
                extent: [options.width, options.height],
                clear_color,
                samples: options.sample_count,
                camera_storage,
                shadows: pbr::ShadowSettings {
                    enabled: options.shadows,
                    ..pbr::ShadowSettings::default()
                },
                output: Some(options.sdr_output()),
            };
            let primitives = scene.primitives.len();
            let instance_draws = options.instances.draws();
            let max_draws = primitives
                .checked_add(instance_draws)
                .ok_or("Bistro scene draw capacity overflow")?;
            let limits = pbr::SceneLimits {
                frames_in_flight: concurrent,
                max_draws,
                max_materials: scene.materials.len(),
                max_casters_per_cascade: max_draws,
            };
            let standard = match options.pipeline {
                PipelineMode::Forward => {
                    let recipe = if let Some(programs) = options
                        .shader_assets
                        .as_ref()
                        .and_then(|assets| assets.programs.as_ref())
                    {
                        pbr_forward::prepare_with_programs(
                            config,
                            limits,
                            pbr_forward::Programs {
                                opaque: programs.forward.clone(),
                                blend: programs.blend.clone(),
                                shadow: programs.shadow.clone(),
                            },
                        )?
                    } else if options.instances.enabled() {
                        pbr_forward::prepare_with_transforms(config, limits)?
                    } else {
                        pbr_forward::prepare(config, limits)?
                    };
                    Standard::Forward(Box::new(recipe))
                }
                PipelineMode::ForwardPlus => {
                    Standard::ForwardPlus(Box::new(pbr_forward_plus::prepare(config, limits)?))
                }
                PipelineMode::Deferred => {
                    Standard::Deferred(Box::new(pbr_deferred::prepare(config, limits)?))
                }
                PipelineMode::ForwardReference => unreachable!("the comparison has its own recipe"),
            };
            preparation.add_requirements(
                standard.preparation().graph_requirements(),
                standard.preparation().working_set(),
            )?;
            if camera_storage == pbr::CameraStorage::PreUploaded {
                preparation.add_graph_requirements(GraphRequirements {
                    passes: 1,
                    accesses: 1,
                    name_bytes: "camera update boundary".len(),
                    upload_bytes: pbr::VIEW_BYTES,
                    ..Default::default()
                })?;
            }
            Recipe::Standard(standard)
        } else {
            Recipe::Reference(Box::new(Reference::prepare(
                options,
                scene,
                layout,
                clear_color,
                hdr,
                &mut preparation,
            )?))
        };
        if let Some(layout) = layout.filter(|_| options.submission == SubmissionMode::Indirect) {
            layout.visibility().reserve(&mut preparation)?;
        }
        let bloom = options.bloom.prepare(&mut preparation, hdr)?;
        Ok(Self {
            preparation,
            recipe,
            bloom,
        })
    }

    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    pub fn uses_standard(&self) -> bool {
        matches!(self.recipe, Recipe::Standard(_))
    }

    pub fn standard(&mut self) -> (&mut Standard, &PreparedBloom) {
        match &mut self.recipe {
            Recipe::Standard(recipe) => (recipe, &self.bloom),
            Recipe::Reference(_) => unreachable!("expected a public recipe"),
        }
    }

    pub fn reference(&self) -> (&Reference, &PreparedBloom) {
        match &self.recipe {
            Recipe::Reference(recipe) => (recipe, &self.bloom),
            Recipe::Standard(_) => unreachable!("expected the comparison recipe"),
        }
    }
}

fn standard_pipeline(options: &Options) -> bool {
    #[cfg(feature = "development-validation")]
    if options.offset_shadow_receiver {
        return false;
    }
    options.pipeline != PipelineMode::ForwardReference
}

/// The all-light control and the development receiver-offset probe capture
/// directly assembled passes for comparison with the prepared recipes.
pub(super) struct Reference {
    hdr: ImageDesc,
    depth: ImageDesc,
    clear: [f32; 4],
    mode: PipelineMode,
    output: PreparedOutput,
    pub opaque: ManagedRasterProgram,
    pub transparent: ManagedRasterProgram,
    pub instances: Option<ManagedRasterProgram>,
    pub shadow: ManagedRasterProgram,
}

impl Reference {
    fn prepare(
        options: &Options,
        scene: &Scene,
        layout: Option<&Layout>,
        clear: [f32; 4],
        hdr: ImageDesc,
        preparation: &mut Preparation,
    ) -> Result<Self, Error> {
        let depth = ImageDesc::depth32(options.width, options.height)
            .with_sample_count(options.sample_count);
        let shadow_extent = shadow::Parameters::default().resolution;
        let mut targets = shadow::CASCADE_COUNT + 2;
        preparation.reserve_image(
            ImageDesc::depth32(shadow_extent, shadow_extent),
            shadow::CASCADE_COUNT,
        )?;
        preparation.reserve_image(hdr, 1)?;
        preparation.reserve_image(depth, 1)?;
        if options.sample_count == SampleCount::X4 {
            preparation.reserve_image(hdr.with_sample_count(options.sample_count), 1)?;
            targets += 1;
        }
        let is_deferred = options.pipeline == PipelineMode::Deferred;
        if is_deferred {
            preparation.reserve_image(hdr, deferred::PLANES.len())?;
            targets += deferred::PLANES.len();
        }

        let opaque = if is_deferred {
            program::deferred::gbuffer_program()
        } else {
            options.forward_program(false).into_owned()
        };
        let transparent = options.forward_program(true).into_owned();
        let shadow = options.shadow_program().into_owned();
        let instances = options
            .instances
            .enabled()
            .then(program::instancing_program);
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

        let tiled = options.pipeline == PipelineMode::ForwardPlus;
        let compute = tiled
            .then(pbr_forward_plus::compute_program)
            .into_iter()
            .collect::<Vec<_>>();
        if tiled {
            preparation.add_working_set(WorkingSet {
                resources: &[renderer::capacity::ResourceClass::Buffer {
                    desc: BufferDesc {
                        size: pbr_forward_plus::TILE_BUFFER_BYTES,
                    },
                    count: 1,
                }],
                ..Default::default()
            })?;
            targets += 1;
        }
        let local_lights = options.pipeline.uses_local_lights();
        let mut upload_bytes = pbr::VIEW_BYTES;
        if local_lights {
            preparation.reserve_buffer(
                BufferDesc {
                    size: pbr_forward_plus::LIGHT_BUFFER_BYTES,
                },
                1,
            )?;
            targets += 1;
            upload_bytes += pbr_forward_plus::LIGHT_BUFFER_BYTES as usize;
        }
        if is_deferred {
            let size = program::deferred::DEFERRED_RECONSTRUCTION_BYTES;
            preparation.reserve_buffer(BufferDesc { size: size as u64 }, 1)?;
            targets += 1;
            upload_bytes += size;
        }
        let groups = layout
            .filter(|_| options.submission == SubmissionMode::Indirect)
            .map_or(0, Layout::group_count);
        let primitives = scene.primitives.len();
        let instance_draws = options.instances.draws();
        let shadow_draws = primitives
            .checked_mul(shadow::CASCADE_COUNT)
            .ok_or("Bistro shadow draw capacity overflow")?;
        let instance_words = instance_draws
            .checked_mul(program::INSTANCE_INLINE_BYTES as usize / size_of::<u32>())
            .ok_or("Bistro instance constant capacity overflow")?;
        let draws = primitives + shadow_draws + groups + instance_draws + usize::from(is_deferred);
        let passes = shadow::CASCADE_COUNT + 8;
        preparation.add_requirements(
            GraphRequirements {
                resources: targets,
                passes,
                compute_dispatches: usize::from(tiled),
                draws,
                bindings: scene.materials.len() * (MAX_RASTER_BINDINGS + 4)
                    + usize::from(instance_draws != 0) * MAX_RASTER_BINDINGS
                    + 20,
                accesses: draws * (MAX_RASTER_BINDINGS + 2) + 36,
                attachments: shadow::CASCADE_COUNT + 8,
                raster_constant_words: shadow_draws + instance_words,
                name_bytes: (targets + passes) * 48,
                upload_bytes,
            },
            WorkingSet {
                raster: &raster,
                compute: &compute,
                ..Default::default()
            },
        )?;
        let output = PreparedOutput::prepare(
            preparation,
            [options.width, options.height],
            options.sdr_output(),
        )?;
        Ok(Self {
            hdr,
            depth,
            clear,
            mode: options.pipeline,
            output,
            opaque,
            transparent,
            instances,
            shadow,
        })
    }

    pub fn scene(
        &self,
        graph: &mut Graph,
        frame: SceneFrame,
        opaque_draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
        transparent_draws: impl FnOnce(&mut RasterPass<'_, '_>) -> Result<(), GraphError>,
    ) -> Result<(ImageHandle, Option<ImageHandle>), Error> {
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
        Ok((hdr, Some(depth)))
    }

    pub fn add_output(&self, graph: &mut Graph, hdr: ImageHandle) -> Result<SdrImage, GraphError> {
        let source: ImageView = graph.image_view(hdr, 0, 0)?;
        Ok(self.output.add_passes(graph, source, 2.0)?.sdr)
    }
}

pub(super) struct SceneFrame {
    pub camera: BufferHandle,
    pub view_projection: [[f32; 4]; 4],
    pub lighting: [Binding; 9],
    pub has_transparency: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::capacity::ResourceClass;
    use renderer_pipeline::passes::bloom::Bloom;

    #[test]
    fn prepared_recipes_cover_supported_bistro_profiles() {
        let scene = Scene::fixture(false);
        let layout = Layout::new(&scene).unwrap();
        let mut options = Options::parse([String::from("--fixture")]).unwrap();
        let [width, height] = [options.width, options.height];
        for mode in [
            PipelineMode::Forward,
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
                                prepared.uses_standard(),
                                mode != PipelineMode::ForwardReference
                            );
                            let set = prepared.preparation().working_set();
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
                            assert!(
                                set.raster
                                    .iter()
                                    .any(|request| request.sample_count == samples
                                        && request.program.color_outputs()
                                            == [renderer::ImageFormat::Rgba16Float])
                            );
                            assert!(set.raster.iter().any(|request| {
                                request
                                    .program
                                    .same(&options.sdr_output().pipeline().program)
                            }));
                            if bloom != Bloom::Off {
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
            assert!(recipe.uses_standard());
            assert!(
                recipe
                    .preparation()
                    .working_set()
                    .raster
                    .iter()
                    .any(|request| {
                        request.sample_count == samples
                            && request.program.bindings().iter().any(|binding| {
                                binding.kind == renderer::program::BindingType::StorageRead
                            })
                    })
            );
        }
    }
}

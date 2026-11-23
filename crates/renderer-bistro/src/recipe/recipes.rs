//! Prepared recipe choices for Bistro and its diagnostic views.

use renderer::pipeline;
use std::num::NonZeroUsize;

use renderer::{
    passes::{
        bloom::PreparedBloom,
        output::{PreparedOutput, SdrImage},
    },
    pbr, pbr_forward, pbr_forward_plus,
};
use renderer_core::{
    capacity::{GraphRequirements, Preparation, WorkingSet},
    graph::{
        Attachment, Binding, BufferDesc, BufferHandle, Graph, GraphError, ImageDesc, ImageHandle,
        ImageView, LoadOp, RasterAttachments, RasterPass, SampleCount, StoreOp,
    },
    program::{ManagedRasterProgram, RasterPipelineRequest},
};

use super::{
    Error, Options, PipelineMode, SubmissionMode, asset::Scene, deferred,
    pipeline::indirect::Layout, program, shadow,
};

pub(super) struct Prepared {
    preparation: Preparation,
    recipe: Recipe,
    bloom: PreparedBloom,
    defaults: Option<pbr::PreparedDefaults>,
}

enum Recipe {
    Standard(Box<pipeline::GeometryPipeline>),
    Reference(Box<Reference>),
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
        let mut defaults = None;
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
                shadows: options.shadows.then(pbr::ShadowSettings::default),
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
                max_shadowed_rect_lights: if options.look == crate::shared::camera::Look::O3deAtrium
                    && options.rect_lights == super::RectLights::Shadowed
                {
                    2
                } else {
                    0
                },
                max_local_lights: if options.look == crate::shared::camera::Look::O3deAtrium {
                    2
                } else if options.pipeline.uses_local_lights() {
                    super::lighting::PRESET_LEN
                } else {
                    0
                },
                max_casters_per_cascade: max_draws,
                max_transforms: if options.instances.enabled() {
                    crate::shared::instancing::COUNT
                } else {
                    usize::from(options.reflection_spheres_to.is_some())
                },
            };
            let standard = match options.pipeline {
                PipelineMode::Forward => {
                    if let Some(programs) = options
                        .shader_assets
                        .as_ref()
                        .and_then(|assets| assets.programs.as_ref())
                    {
                        pipeline::GeometryPipeline::Forward(pbr_forward::prepare_with_programs(
                            config,
                            limits,
                            pbr_forward::Programs {
                                opaque: programs.forward.clone(),
                                blend: programs.blend.clone(),
                                shadow: programs.shadow.clone(),
                            },
                        )?)
                    } else {
                        pipeline::GeometryPipeline::new(pbr::Technique::Forward, config, limits)?
                    }
                }
                PipelineMode::ForwardPlus => {
                    pipeline::GeometryPipeline::new(pbr::Technique::ForwardPlus, config, limits)?
                }
                PipelineMode::Deferred => {
                    pipeline::GeometryPipeline::new(pbr::Technique::Deferred, config, limits)?
                }
                PipelineMode::ForwardReference => unreachable!("the comparison has its own recipe"),
            };
            preparation.add_requirements(
                standard.preparation().graph_requirements(),
                standard.preparation().working_set(),
            )?;
            if !options.shadows {
                defaults = Some(pbr::Defaults::Unshadowed.prepare(&mut preparation)?);
                if options.capture_shadows && !options.windowed {
                    let resolution = shadow::Parameters::default().resolution;
                    preparation.reserve_image(
                        ImageDesc::depth32(resolution, resolution),
                        pbr::CASCADE_COUNT,
                    )?;
                    preparation.add_graph_requirements(GraphRequirements {
                        resources: pbr::CASCADE_COUNT,
                        passes: pbr::CASCADE_COUNT,
                        accesses: pbr::CASCADE_COUNT,
                        attachments: pbr::CASCADE_COUNT,
                        name_bytes: 128,
                        ..GraphRequirements::default()
                    })?;
                }
            }
            if camera_storage == pbr::CameraStorage::PreUploaded {
                preparation.add_graph_requirements(GraphRequirements {
                    passes: 1,
                    accesses: 1,
                    name_bytes: "camera update boundary".len(),
                    upload_bytes: pbr::VIEW_BYTES,
                    ..Default::default()
                })?;
            }
            Recipe::Standard(Box::new(standard))
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
            defaults,
        })
    }

    pub fn preparation(&self) -> &Preparation {
        &self.preparation
    }

    pub fn uses_standard(&self) -> bool {
        matches!(self.recipe, Recipe::Standard(_))
    }

    pub fn defaults(&self) -> Option<pbr::PreparedDefaults> {
        self.defaults
    }

    pub fn standard(&mut self) -> (&mut pipeline::GeometryPipeline, &PreparedBloom) {
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
    pub lights: Option<super::lighting::LightTable>,
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
            preparation.reserve_image(
                super::lighting::layout().tile_mask_desc(options.width, options.height)?,
                1,
            )?;
            targets += 1;
        }
        let local_lights = options
            .pipeline
            .uses_local_lights()
            .then(|| {
                super::lighting::LightTable::new(
                    options.fixture,
                    [options.width, options.height],
                    tiled,
                )
            })
            .transpose()?;
        let mut upload_bytes = pbr::VIEW_BYTES;
        if local_lights.is_some() {
            preparation.reserve_image(super::lighting::layout().image_desc(), 1)?;
            targets += 1;
            upload_bytes += super::lighting::layout().byte_len();
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
        let max_bindings = raster
            .iter()
            .map(|request| request.program.bindings().len())
            .max()
            .expect("the reference opaque Program is always prepared");
        preparation.add_requirements(
            GraphRequirements {
                resources: targets,
                passes,
                compute_dispatches: usize::from(tiled),
                draws,
                bindings: scene.materials.len() * (max_bindings + 4)
                    + usize::from(instance_draws != 0) * max_bindings
                    + 20,
                accesses: draws * (max_bindings + 2) + 36,
                attachments: shadow::CASCADE_COUNT + 8,
                raster_constant_words: shadow_draws + instance_words,
                name_bytes: (targets + passes) * 48,
                upload_bytes,
                ..Default::default()
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
            lights: local_lights,
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
                frame.clear.unwrap_or(self.clear),
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
                local_lights: None,
                hdr: hdr_view,
                depth: depth_view,
                clear: frame.clear.unwrap_or(self.clear),
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
    pub clear: Option<[f32; 4]>,
    pub camera: BufferHandle,
    pub view_projection: [[f32; 4]; 4],
    pub lighting: [Binding; 10],
    pub has_transparency: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::passes::bloom::Bloom;
    use renderer_core::capacity::ResourceClass;

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
                                            == [renderer_core::ImageFormat::Rgba16Float])
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
                                binding.kind == renderer_core::program::BindingType::StorageRead
                            })
                    })
            );
        }
    }
}

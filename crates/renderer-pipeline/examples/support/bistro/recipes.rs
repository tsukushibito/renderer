//! Prepare the complete Bistro frame from embedded YAML fragments.

use renderer::{
    capacity::WorkingSet,
    graph::{Graph, SampleCount},
};
use renderer_pipeline::{
    FrameInputs, FrameOutput, PipelineRequirements, PrepareInputs, PrepareOptions,
    PrepareParameter, PrepareValue, PreparedPipeline, Program, ShaderRequest,
    standard::passes::tone_map_neutral_pipeline,
};

use super::{
    Error, Options, PipelineMode, SubmissionMode,
    asset::Scene,
    bloom::{self, Bloom},
    digest,
    pipeline::indirect::Layout,
    program,
};

const MATERIAL: &str = "bistro/pbr";

pub(super) struct Prepared {
    pipeline: PreparedPipeline,
    sha256: String,
}

impl Prepared {
    pub fn new(
        options: &Options,
        scene: &Scene,
        layout: Option<&Layout>,
        background: [f32; 4],
    ) -> Result<Self, Error> {
        let indirect = layout.filter(|_| options.submission == SubmissionMode::Indirect);
        let fragments = fragments(options, indirect.is_some());
        let mut parameters = vec![PrepareParameter {
            name: "background",
            value: PrepareValue::Color(background),
        }];
        if options.pipeline == PipelineMode::ForwardPlus {
            prepare_tiles(options, &mut parameters)?;
        }
        let tables = if let Some(layout) = indirect {
            parameters.extend([
                PrepareParameter {
                    name: "bounds_bytes",
                    value: PrepareValue::Size(layout.aabb_bytes() as u64),
                },
                PrepareParameter {
                    name: "visibility_bytes",
                    value: PrepareValue::Size(layout.mask_bytes()),
                },
                PrepareParameter {
                    name: "visibility_grid",
                    value: PrepareValue::Grid([layout.dispatch_groups(), 1]),
                },
            ]);
            layout.indirect_tables().collect()
        } else {
            Vec::new()
        };
        let indirect_lists = [renderer_pipeline::PrepareIndirectList {
            name: "packed_opaque",
            tables: &tables,
        }];
        let pipeline = PreparedPipeline::from_yaml_fragments(
            &fragments,
            PrepareOptions {
                frame_extent: Some([options.width, options.height]),
                frames_in_flight: options.inflight,
                max_draws_per_list: scene.primitives.len().max(options.instances.draws()),
                max_materials: scene.materials.len(),
            },
            PrepareInputs {
                values: &parameters,
                indirect_lists: if indirect.is_some() {
                    &indirect_lists
                } else {
                    &[]
                },
            },
            |request| resolve(options, request),
        )?;
        let sha256 = digest(fragments.concat().as_bytes());
        Ok(Self { pipeline, sha256 })
    }

    pub fn build(
        &mut self,
        graph: Graph,
        inputs: FrameInputs<'_>,
    ) -> Result<(Graph, FrameOutput<'_>), Error> {
        Ok(self.pipeline.build(graph, inputs)?)
    }

    pub fn requirements(&self) -> PipelineRequirements {
        self.pipeline.requirements()
    }

    pub fn working_set(&self) -> WorkingSet<'_> {
        self.pipeline.working_set()
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

struct DrawRecipes {
    indexed: &'static str,
    indirect: &'static str,
    transparent: &'static str,
}

fn fragments(options: &Options, indirect: bool) -> Vec<&'static str> {
    let mut fragments = vec![include_str!("recipes/shadows.yaml")];
    let draw_recipes = match options.pipeline {
        PipelineMode::Forward => DrawRecipes {
            indexed: include_str!("recipes/draw-forward-indexed.yaml"),
            indirect: include_str!("recipes/draw-forward-indirect.yaml"),
            transparent: include_str!("recipes/draw-forward-transparent.yaml"),
        },
        PipelineMode::ForwardPlus => {
            fragments.push(include_str!("recipes/lighting-tiled.yaml"));
            DrawRecipes {
                indexed: include_str!("recipes/draw-forward-plus-indexed.yaml"),
                indirect: include_str!("recipes/draw-forward-plus-indirect.yaml"),
                transparent: include_str!("recipes/draw-forward-plus-transparent.yaml"),
            }
        }
        PipelineMode::ForwardReference => {
            fragments.push(include_str!("recipes/lighting-reference.yaml"));
            DrawRecipes {
                indexed: include_str!("recipes/draw-forward-reference-indexed.yaml"),
                indirect: include_str!("recipes/draw-forward-reference-indirect.yaml"),
                transparent: include_str!("recipes/draw-forward-reference-transparent.yaml"),
            }
        }
        PipelineMode::Deferred => DrawRecipes {
            indexed: include_str!("recipes/draw-deferred-indexed.yaml"),
            indirect: include_str!("recipes/draw-deferred-indirect.yaml"),
            transparent: include_str!("recipes/draw-forward-transparent.yaml"),
        },
    };
    let opaque = if indirect {
        fragments.push(include_str!("recipes/visibility.yaml"));
        draw_recipes.indirect
    } else if options.instances.enabled() {
        include_str!("recipes/draw-forward-instances.yaml")
    } else {
        draw_recipes.indexed
    };
    let scene = match (options.pipeline, options.sample_count) {
        (PipelineMode::Deferred, _) => include_str!("recipes/scene-deferred.yaml"),
        (_, SampleCount::X1) => include_str!("recipes/scene-forward.yaml"),
        (_, SampleCount::X4) => include_str!("recipes/scene-forward-msaa.yaml"),
    };
    fragments.extend([opaque, draw_recipes.transparent, scene]);
    let blur = match options.bloom {
        Bloom::Off => None,
        Bloom::Tent => Some(include_str!("recipes/bloom-tent.yaml")),
        Bloom::Box => Some(include_str!("recipes/bloom-box.yaml")),
    };
    if let Some(blur) = blur {
        fragments.extend([
            include_str!("recipes/bloom-prefilter.yaml"),
            blur,
            include_str!("recipes/tone-map-bloom.yaml"),
        ]);
    } else {
        fragments.push(include_str!("recipes/tone-map.yaml"));
    }
    fragments.push(if options.aa {
        include_str!("recipes/output-fxaa.yaml")
    } else {
        include_str!("recipes/output.yaml")
    });
    fragments
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn prepare_tiles(
    options: &Options,
    parameters: &mut Vec<PrepareParameter<'static>>,
) -> Result<(), Error> {
    let compute = program::forward_plus::compute_program();
    let tiles = program::forward_plus::active_tile_count(options.width, options.height)?;
    let groups = tiles.div_ceil(compute.workgroup_size()[0]);
    let bytes_per_group = compute.dispatch_contract().bytes[2];
    parameters.extend([
        PrepareParameter {
            name: "tile_grid",
            value: PrepareValue::Grid([groups, 1]),
        },
        PrepareParameter {
            name: "tile_write_bytes",
            value: PrepareValue::Size(u64::from(groups) * bytes_per_group),
        },
    ]);
    Ok(())
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn prepare_tiles(_: &Options, _: &mut Vec<PrepareParameter<'static>>) -> Result<(), Error> {
    Err("Bistro Forward+ requires a native Renderer backend feature".into())
}

fn resolve(
    options: &Options,
    request: ShaderRequest<'_>,
) -> Result<Program, Box<dyn std::error::Error + Send + Sync>> {
    use ShaderRequest::{Program as Named, Template};

    Ok(match request {
        Template {
            template: "bistro/shadow",
            material_type: MATERIAL,
        } => Program::Raster(options.shadow_program().into_owned()),
        Template {
            template: "bistro/forward",
            material_type: MATERIAL,
        } => Program::Raster(options.forward_program(false).into_owned()),
        Named("bistro/forward-blend") => {
            Program::Raster(options.forward_program(true).into_owned())
        }
        Named("bistro/forward-plus") => {
            Program::Raster(program::forward_plus::raster_program(true, false))
        }
        Named("bistro/forward-plus-blend") => {
            Program::Raster(program::forward_plus::raster_program(true, true))
        }
        Named("bistro/forward-reference") => {
            Program::Raster(program::forward_plus::raster_program(false, false))
        }
        Named("bistro/forward-reference-blend") => {
            Program::Raster(program::forward_plus::raster_program(false, true))
        }
        Named("bistro/instancing") => Program::Raster(program::instancing_program()),
        Named("bistro/gbuffer") => Program::Raster(program::deferred::gbuffer_program()),
        Named("bistro/deferred-lighting") => {
            Program::Raster(program::deferred::deferred_lighting_program())
        }
        #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
        Named("bistro/cull-local-lights") => {
            Program::Compute(program::forward_plus::compute_program())
        }
        Named("bistro/frustum-visibility") => {
            Program::Compute(program::visibility::visibility_program())
        }
        Named("bloom/extract") => Program::Compute(bloom::EXTRACT),
        Named("bloom/downsample") => Program::Compute(bloom::DOWNSAMPLE),
        Named("bloom/blur-tent") => Program::Compute(bloom::BLUR_TENT),
        Named("bloom/blur-box") => Program::Compute(bloom::BLUR_BOX),
        Named("bloom/combine") => Program::Compute(bloom::COMBINE),
        Named("post/neutral") => Program::Raster(tone_map_neutral_pipeline().program),
        Named("post/sdr" | "post/fxaa") => {
            let mut output = options.sdr_output();
            output.fxaa = matches!(request, Named("post/fxaa"));
            Program::Raster(output.pipeline().program)
        }
        _ => return Err(format!("unknown Bistro shader {request:?}").into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer::graph::ImageDesc;

    #[test]
    fn declared_recipes_cover_the_supported_bistro_profiles() {
        let scene = Scene::fixture(false);
        let layout = Layout::new(&scene).unwrap();
        let mut options = Options::parse([String::from("--fixture")]).unwrap();
        let extent = [options.width, options.height];
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
                            let recipe = Prepared::new(
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
                            let [width, height] = extent;
                            assert_eq!(
                                recipe.pipeline.output_resource(),
                                ("sdr", ImageDesc::rgba8(width, height))
                            );
                            assert_eq!(
                                recipe.pipeline.image_resource("hdr"),
                                Some(ImageDesc::rgba16_float(width, height))
                            );
                            assert_eq!(
                                recipe.pipeline.image_resource("depth"),
                                Some(ImageDesc::depth32(width, height).with_sample_count(samples))
                            );
                            assert_eq!(
                                recipe.pipeline.image_resource("bloom_hdr").is_some(),
                                bloom != Bloom::Off
                            );
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
            assert!(
                recipe
                    .pipeline
                    .working_set()
                    .raster
                    .iter()
                    .any(|request| request.program.parameter_size()
                        == program::INSTANCE_INLINE_BYTES)
            );
        }
    }
}

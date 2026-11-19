//! Configured fullscreen processing for Bistro's post-Bloom HDR image.

use std::{fs, path::Path, sync::Arc};

use renderer::{
    capacity::{ResourceClass, WorkingSet},
    graph::{
        Attachment, Graph, GraphCapacity, GraphPool, ImageDesc, ImageHandle, LoadOp,
        RasterAttachments, StoreOp,
    },
    standard::passes::tone_map_neutral_pipeline,
};
use renderer_pipeline::{
    FrameInputs, FrameResource, PrepareOptions, PreparedPipeline, Program, ResourceDesc,
    ShaderRequest,
};

use super::{Error, Options, digest};

struct Source {
    yaml: String,
    sha256: String,
}

pub(super) struct Prepared {
    source: Arc<Source>,
    pipeline: PreparedPipeline,
    graph: GraphCapacity,
    aa: bool,
}

impl Prepared {
    pub fn load(options: &Options) -> Result<Option<Self>, Error> {
        options
            .post_pipeline
            .as_deref()
            .map(|path| {
                let yaml = fs::read_to_string(path).map_err(|error| path_error(path, error))?;
                let source = Arc::new(Source {
                    sha256: digest(yaml.as_bytes()),
                    yaml,
                });
                Self::prepare(source, options).map_err(|error| path_error(path, error))
            })
            .transpose()
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn resize(&self, options: &Options) -> Result<Self, Error> {
        Self::prepare(Arc::clone(&self.source), options)
    }

    fn prepare(source: Arc<Source>, options: &Options) -> Result<Self, Error> {
        let extent = [options.width, options.height];
        let hdr = ImageDesc::rgba16_float(options.width, options.height);
        let sdr = ImageDesc::rgba8(options.width, options.height);
        let mut pipeline = PreparedPipeline::from_yaml(
            &source.yaml,
            PrepareOptions {
                frames_in_flight: options.inflight,
                max_draws_per_list: 0,
                max_materials: 0,
                frame_extent: Some(extent),
            },
            |request| match request {
                ShaderRequest::Program("post/neutral") => {
                    Ok(Program::Raster(tone_map_neutral_pipeline().program))
                }
                ShaderRequest::Program("post/sdr") => {
                    Ok(Program::Raster(options.sdr_output().pipeline().program))
                }
                _ => Err(format!("unknown Bistro post Program {request:?}").into()),
            },
        )?;
        if !pipeline
            .external_resources()
            .eq([("hdr", ResourceDesc::Image(hdr))])
        {
            return Err(
                "Bistro post YAML requires one external frame-size RGBA16F image named 'hdr'"
                    .into(),
            );
        }
        if pipeline.output_resource().1 != sdr {
            return Err("Bistro post YAML output must be a frame-size RGBA8 UNORM image".into());
        }
        let requirements = pipeline
            .fullscreen_requirements()
            .ok_or("Bistro post YAML requires fullscreen Raster passes")?;
        if requirements.writes_external_attachments {
            return Err("Bistro post YAML must treat external HDR as read-only".into());
        }
        if pipeline
            .working_set()
            .resources
            .iter()
            .any(|class| matches!(class, ResourceClass::Buffer { .. }))
        {
            return Err("Bistro post YAML resources must be images".into());
        }

        let accesses = requirements.bindings + 2 * requirements.attachments;
        let graph = GraphCapacity {
            resources: requirements.resources,
            passes: requirements.passes,
            accesses,
            draws: requirements.passes,
            raster_constant_words: Some(requirements.raster_constant_words),
            bindings: requirements.bindings,
            attachments: requirements.attachments,
            dependencies: 0,
            initial_ranges: 0,
            name_bytes: requirements.name_bytes,
            upload_bytes: 0,
            edges: accesses * 4,
            segments: accesses * 2,
            readers: accesses * 4,
        };
        validate_graph(&mut pipeline, graph, hdr)?;
        Ok(Self {
            source,
            pipeline,
            graph,
            aa: options.aa,
        })
    }

    pub fn sha256(&self) -> &str {
        &self.source.sha256
    }

    pub fn uses_options(&self, options: &Options) -> bool {
        // The automatic AA-off captures use the builtin diagnostic for the
        // other AA mode. An explicit --aa-off run can select its own YAML.
        self.aa == options.aa
    }

    pub fn graph_capacity(&self) -> GraphCapacity {
        self.graph
    }

    pub fn working_set(&self) -> WorkingSet<'_> {
        self.pipeline.working_set()
    }

    pub fn append(
        &mut self,
        graph: Graph,
        hdr: ImageHandle,
    ) -> Result<(Graph, ImageHandle), Error> {
        Ok(append(&mut self.pipeline, graph, hdr)?)
    }
}

fn path_error(path: &Path, error: impl std::fmt::Display) -> Error {
    format!("Bistro post pipeline {}: {error}", path.display()).into()
}

fn append(
    pipeline: &mut PreparedPipeline,
    graph: Graph,
    hdr: ImageHandle,
) -> Result<(Graph, ImageHandle), renderer_pipeline::PipelineError> {
    pipeline.build(
        graph,
        FrameInputs {
            resources: &[FrameResource {
                name: "hdr",
                resource: hdr.into(),
            }],
            ..Default::default()
        },
    )
}

/// Check framebuffer/binding compatibility and output initialization once using
/// a stand-in for the scene's HDR producer. No GPU objects are needed here.
fn validate_graph(
    pipeline: &mut PreparedPipeline,
    mut capacity: GraphCapacity,
    hdr_desc: ImageDesc,
) -> Result<(), Error> {
    const INPUT: &str = "Bistro post validation HDR";
    capacity.resources += 1;
    capacity.passes += 1;
    capacity.accesses += 2;
    capacity.attachments += 1;
    capacity.name_bytes += 2 * INPUT.len();
    capacity.edges += 8;
    capacity.segments += 4;
    capacity.readers += 8;
    let mut pool = GraphPool::new(capacity, 1)?;
    let mut graph = pool.begin()?;
    let hdr = graph.image(INPUT, hdr_desc)?;
    let view = graph.image_view(hdr, 0, 0)?;
    graph.attachment_pass(
        INPUT,
        RasterAttachments {
            color_resolves: &[],
            colors: &[Attachment {
                view,
                load: LoadOp::Clear([0.0; 4]),
                store: StoreOp::Store,
            }],
            depth: None,
        },
    )?;
    let (mut graph, output) = append(pipeline, graph, hdr)?;
    graph.export_image(output)?;
    graph.compile()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bistro::pipeline::Budget;

    const FXAA: &str = include_str!("post.yaml");
    const NO_FXAA: &str = include_str!("post-aa-off.yaml");

    fn source(yaml: &str) -> Arc<Source> {
        Arc::new(Source {
            yaml: yaml.to_owned(),
            sha256: digest(yaml.as_bytes()),
        })
    }

    fn fixture_options() -> Options {
        Options::parse(["--fixture".into()]).unwrap()
    }

    #[test]
    fn shipped_graphs_prepare_at_frame_extent_and_keep_the_stock_image_budget() {
        for (yaml, aa) in [(FXAA, true), (NO_FXAA, false)] {
            let mut options = fixture_options();
            options.aa = aa;
            let source = source(yaml);
            for (width, height, windowed) in [(256, 144, false), (321, 199, true)] {
                options.width = width;
                options.height = height;
                options.windowed = windowed;
                let prepared = Prepared::prepare(Arc::clone(&source), &options).unwrap();
                assert_eq!(
                    prepared.pipeline.output_resource().1,
                    ImageDesc::rgba8(width, height)
                );
                let builtin = Budget::frame_images(&options, None);
                let configured = Budget::frame_images(&options, Some(&prepared));
                assert_eq!(builtin.len(), configured.len());
                for class in &builtin {
                    assert!(configured.contains(class));
                }
            }
        }
    }

    #[test]
    fn recipe_cannot_attach_the_external_hdr_input() {
        let writes_hdr = FXAA.replace("image: neutral, load:", "image: hdr, load:");
        let result = Prepared::prepare(source(&writes_hdr), &fixture_options());
        assert_eq!(
            result.err().unwrap().to_string(),
            "Bistro post YAML must treat external HDR as read-only"
        );
    }
}

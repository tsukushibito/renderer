//! Application inputs and owned depth targets for a YAML shadow graph.

use std::fs;

use renderer::{
    capacity::ResourceClass,
    graph::{BufferDesc, BufferHandle, Graph, GraphCapacity, GraphPool, ImageDesc, ImageHandle},
};
use renderer_pipeline::{
    DrawCandidate, FrameInputs, FrameResource, FrameView, PipelineRequirements, PrepareOptions,
    PreparedPipeline, Program, ResourceDesc, ShaderRequest,
};

use super::{Error, Options, asset::Scene, digest, program, scene_pipeline::Materials, shadow};

const CASCADE_IMAGES: [&str; shadow::CASCADE_COUNT] =
    ["cascade_0", "cascade_1", "cascade_2", "cascade_3"];
const CASCADE_VIEWS: [&str; shadow::CASCADE_COUNT] = ["sun_0", "sun_1", "sun_2", "sun_3"];

pub(super) struct Prepared {
    sha256: String,
    pipeline: PreparedPipeline,
    pub(super) candidates: [Vec<DrawCandidate<'static>>; shadow::CASCADE_COUNT],
}

impl Prepared {
    pub fn load(options: &Options, scene: &Scene) -> Result<Option<Self>, Error> {
        let Some(path) = &options.shadow_pipeline else {
            return Ok(None);
        };
        let prepare = || -> Result<Self, Error> {
            Self::prepare(
                &fs::read_to_string(path)?,
                options,
                scene.primitives.len(),
                scene.materials.len(),
            )
        };
        prepare()
            .map(Some)
            .map_err(|error| format!("Bistro shadow pipeline {}: {error}", path.display()).into())
    }

    fn prepare(
        yaml: &str,
        options: &Options,
        max_draws: usize,
        max_materials: usize,
    ) -> Result<Self, Error> {
        let resolution = shadow::Parameters::default().resolution;
        let pipeline = PreparedPipeline::from_yaml(
            yaml,
            PrepareOptions {
                frame_extent: Some([resolution, resolution]),
                frames_in_flight: options.inflight,
                max_draws_per_list: max_draws,
                max_materials,
            },
            |request| match request {
                ShaderRequest::Program("bistro/shadow")
                | ShaderRequest::Template {
                    template: "bistro/shadow",
                    material_type: "bistro/pbr",
                } => Ok(Program::Raster(options.shadow_program().into_owned())),
                _ => Err(format!("unknown Bistro shadow Program {request:?}").into()),
            },
        )?;
        let camera = ResourceDesc::Buffer(BufferDesc {
            size: program::CAMERA_BYTES as u64,
        });
        if !pipeline.external_resources().eq([("camera", camera)])
            || pipeline.requirements().writes_external_resources
            || pipeline.sampled_view_names().next().is_some()
        {
            return Err(
                "Bistro shadow YAML requires only the read-only external camera buffer".into(),
            );
        }
        if pipeline
            .working_set()
            .resources
            .iter()
            .any(|class| matches!(class, ResourceClass::Buffer { .. }))
        {
            return Err("Bistro shadow YAML transient resources must be images".into());
        }
        let depth = ImageDesc::depth32(resolution, resolution);
        for name in CASCADE_IMAGES {
            if pipeline.image_resource(name) != Some(depth) {
                return Err(format!(
                    "Bistro shadow YAML requires {name}, a {resolution}×{resolution} D32 image"
                )
                .into());
            }
        }
        if !CASCADE_IMAGES.contains(&pipeline.output_resource().0) {
            return Err("Bistro shadow YAML output must be one of the four cascade images".into());
        }

        let mut prepared = Self {
            sha256: digest(yaml.as_bytes()),
            pipeline,
            candidates: std::array::from_fn(|_| Vec::with_capacity(max_draws)),
        };
        prepared.validate_outputs()?;
        Ok(prepared)
    }

    fn validate_outputs(&mut self) -> Result<(), Error> {
        const CAMERA: &str = "shadow camera input";
        let requirements = self.requirements();
        let accesses = requirements.attachments * 2;
        let mut pool = GraphPool::new(
            GraphCapacity {
                resources: requirements.resources + 1,
                passes: requirements.passes,
                accesses,
                draws: 0,
                raster_constant_words: Some(0),
                bindings: 0,
                attachments: requirements.attachments,
                dependencies: 0,
                initial_ranges: 0,
                name_bytes: requirements.name_bytes + CAMERA.len(),
                upload_bytes: 0,
                edges: accesses * 4,
                segments: accesses * 2,
                readers: accesses * 4,
            },
            1,
        )?;
        let mut graph = pool.begin()?;
        let camera = graph.buffer(
            CAMERA,
            BufferDesc {
                size: program::CAMERA_BYTES as u64,
            },
        )?;
        // A camera can have no visible casters. All four outputs still have
        // to be initialized in this draw-free graph, before native submission.
        let (mut graph, cascades) = self.append(graph, camera, &Materials::new(0))?;
        for cascade in cascades {
            graph.export_image(cascade)?;
        }
        graph.compile()?;
        Ok(())
    }

    pub fn sha256(&self) -> &str {
        &self.sha256
    }

    pub fn requirements(&self) -> PipelineRequirements {
        self.pipeline.requirements()
    }

    pub fn resources(&self) -> &[ResourceClass] {
        self.pipeline.working_set().resources
    }

    pub fn append(
        &mut self,
        graph: Graph,
        camera: BufferHandle,
        materials: &Materials,
    ) -> Result<(Graph, [ImageHandle; shadow::CASCADE_COUNT]), Error> {
        let (materials, material_bindings) = materials.inputs();
        let views: [FrameView<'_>; shadow::CASCADE_COUNT] =
            std::array::from_fn(|index| FrameView {
                name: CASCADE_VIEWS[index],
                candidates: &self.candidates[index],
            });
        let (graph, output) = self.pipeline.build(
            graph,
            FrameInputs {
                resources: &[FrameResource {
                    name: "camera",
                    resource: camera.into(),
                }],
                views: &views,
                materials,
                material_bindings,
                ..Default::default()
            },
        )?;
        Ok((
            graph,
            CASCADE_IMAGES.map(|name| output.named_image(name).expect("prepared shadow image")),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cascade_requires_a_writer_even_without_casters() {
        let yaml = include_str!("scene-shadows.yaml");
        let options = Options::parse([String::from("--fixture")]).unwrap();
        Prepared::prepare(yaml, &options, 1, 1).unwrap();
        let unused = yaml.replace("depth: { image: cascade_2,", "depth: { image: cascade_1,");
        assert_ne!(unused, yaml);
        let error = Prepared::prepare(&unused, &options, 1, 1)
            .err()
            .expect("unwritten depth cascade");
        assert!(error.to_string().contains("cascade_2"), "{error}");
    }
}

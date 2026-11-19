//! Application inputs for a YAML Forward graph.

use std::{fs, sync::Arc};

use renderer::{
    capacity::ResourceClass,
    graph::{
        BindingResource, BufferDesc, BufferHandle, Graph, GraphCapacity, GraphPool, ImageDesc,
        ImageHandle, ImageView,
    },
};
use renderer_pipeline::{
    DrawCandidate, FrameInputs, FrameMaterial, FrameResource, FrameSampledView, FrameView,
    MaterialBinding, PipelineRequirements, PrepareOptions, PreparedPipeline, Program, ResourceDesc,
    ShaderRequest,
};

use super::{Error, Options, asset::Scene, digest, program};

const MATERIAL_TYPE: &str = "bistro/pbr";
const COLOR_TEXTURES: [&str; 4] = ["base_color", "normal", "orm", "emissive"];
const SAMPLED_VIEWS: [&str; 7] = [
    "irradiance",
    "reflection",
    "brdf",
    "cascade_0",
    "cascade_1",
    "cascade_2",
    "cascade_3",
];

struct Source {
    yaml: String,
    sha256: String,
}

pub(super) struct Prepared {
    source: Arc<Source>,
    pipeline: PreparedPipeline,
}

impl Prepared {
    pub fn load(options: &Options, scene: &Scene) -> Result<Option<Self>, Error> {
        let Some(path) = &options.scene_pipeline else {
            return Ok(None);
        };
        let prepare = || -> Result<Self, Error> {
            let yaml = fs::read_to_string(path)?;
            let source = Arc::new(Source {
                sha256: digest(yaml.as_bytes()),
                yaml,
            });
            Self::prepare(
                source,
                options,
                scene.primitives.len(),
                scene.materials.len(),
            )
        };
        prepare()
            .map(Some)
            .map_err(|error| format!("Bistro scene pipeline {}: {error}", path.display()).into())
    }

    #[cfg(all(windows, feature = "presentation"))]
    pub fn resize(&self, options: &Options, scene: &Scene) -> Result<Self, Error> {
        Self::prepare(
            Arc::clone(&self.source),
            options,
            scene.primitives.len(),
            scene.materials.len(),
        )
    }

    fn prepare(
        source: Arc<Source>,
        options: &Options,
        max_draws: usize,
        max_materials: usize,
    ) -> Result<Self, Error> {
        let pipeline = PreparedPipeline::from_yaml(
            &source.yaml,
            PrepareOptions {
                frame_extent: Some([options.width, options.height]),
                frames_in_flight: options.inflight,
                max_draws_per_list: max_draws,
                max_materials,
            },
            |request| {
                let blend = match request {
                    ShaderRequest::Program("bistro/forward")
                    | ShaderRequest::Template {
                        template: "bistro/forward",
                        material_type: MATERIAL_TYPE,
                    } => false,
                    ShaderRequest::Program("bistro/forward-blend") => true,
                    _ => return Err(format!("unknown Bistro Forward Program {request:?}").into()),
                };
                Ok(Program::Raster(options.forward_program(blend).into_owned()))
            },
        )?;
        let camera = ResourceDesc::Buffer(BufferDesc {
            size: program::CAMERA_BYTES as u64,
        });
        if !pipeline.external_resources().eq([("camera", camera)]) {
            return Err(
                "Bistro Forward YAML requires a camera buffer as its external resource".into(),
            );
        }
        if pipeline.requirements().writes_external_resources {
            return Err(
                "Bistro Forward YAML must treat its external camera buffer as read-only".into(),
            );
        }
        if let Some(name) = pipeline
            .sampled_view_names()
            .find(|name| !SAMPLED_VIEWS.contains(name))
        {
            return Err(
                format!("Bistro Forward YAML refers to unknown sampled view {name:?}").into(),
            );
        }
        let hdr = ImageDesc::rgba16_float(options.width, options.height);
        let depth = ImageDesc::depth32(options.width, options.height);
        if pipeline.output_resource() != ("hdr", hdr)
            || pipeline.image_resource("depth") != Some(depth)
        {
            return Err("Bistro Forward YAML requires frame-size hdr (RGBA16F output) and depth (D32) images".into());
        }
        if pipeline
            .working_set()
            .resources
            .iter()
            .any(|class| matches!(class, ResourceClass::Buffer { .. }))
        {
            return Err("Bistro Forward YAML transient resources must be images".into());
        }
        let mut prepared = Self { source, pipeline };
        prepared.validate_outputs()?;
        Ok(prepared)
    }

    pub fn sha256(&self) -> &str {
        &self.source.sha256
    }

    pub fn requirements(&self) -> PipelineRequirements {
        self.pipeline.requirements()
    }

    pub fn resources(&self) -> &[ResourceClass] {
        self.pipeline.working_set().resources
    }

    fn validate_outputs(&mut self) -> Result<(), Error> {
        const CAMERA: &str = "scene camera input";
        const LIGHTING: &str = "scene lighting input";
        const SHADOW: &str = "scene shadow input";
        let requirements = self.requirements();
        // Draw-free assembly validates declarations and initialization even for
        // a frame with no visible geometry. The dummy inputs are never submitted.
        let accesses = requirements.attachments * 2;
        let mut pool = GraphPool::new(
            GraphCapacity {
                resources: requirements.resources + 3,
                passes: requirements.passes,
                accesses,
                draws: 0,
                raster_constant_words: Some(0),
                bindings: 0,
                attachments: requirements.attachments,
                dependencies: 0,
                initial_ranges: 0,
                name_bytes: requirements.name_bytes + CAMERA.len() + LIGHTING.len() + SHADOW.len(),
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
        let lighting = graph.image(LIGHTING, ImageDesc::rgba16_float(1, 1))?;
        let shadow = graph.image(SHADOW, ImageDesc::depth32(1, 1))?;
        let lighting = graph.image_view(lighting, 0, 0)?;
        let shadow = graph.image_view(shadow, 0, 0)?;
        let (mut graph, hdr, depth) = self.append(
            graph,
            camera,
            [lighting; 3],
            [shadow; 4],
            &[],
            &Materials::new(0),
        )?;
        graph.export_image(hdr)?;
        graph.export_image(depth)?;
        graph.compile()?;
        Ok(())
    }

    pub fn append(
        &mut self,
        graph: Graph,
        camera: BufferHandle,
        lighting: [ImageView; 3],
        shadows: [ImageView; 4],
        candidates: &[DrawCandidate<'_>],
        materials: &Materials,
    ) -> Result<(Graph, ImageHandle, ImageHandle), Error> {
        let (materials, material_bindings) = materials.inputs();
        let [irradiance, reflection, brdf] = lighting;
        let [cascade_0, cascade_1, cascade_2, cascade_3] = shadows;
        let images = [
            irradiance, reflection, brdf, cascade_0, cascade_1, cascade_2, cascade_3,
        ];
        let sampled_views: [FrameSampledView<'_>; 7] =
            std::array::from_fn(|index| FrameSampledView {
                name: SAMPLED_VIEWS[index],
                view: images[index],
            });

        let (graph, output) = self.pipeline.build(
            graph,
            FrameInputs {
                resources: &[FrameResource {
                    name: "camera",
                    resource: camera.into(),
                }],
                sampled_views: &sampled_views,
                views: &[FrameView {
                    name: "main",
                    candidates,
                }],
                materials,
                material_bindings,
            },
        )?;
        let depth = output.named_image("depth").expect("prepared scene depth");
        Ok((graph, output.image(), depth))
    }
}

/// Fixed material rows and reserved binding storage shared by the scene passes.
pub(super) struct Materials {
    rows: Vec<FrameMaterial<'static>>,
    bindings: Vec<MaterialBinding<'static>>,
}

impl Materials {
    pub fn new(count: usize) -> Self {
        let width = 1 + COLOR_TEXTURES.len();
        Self {
            rows: (0..count)
                .map(|index| FrameMaterial {
                    material_type: MATERIAL_TYPE,
                    bindings: index * width..(index + 1) * width,
                })
                .collect(),
            bindings: Vec::with_capacity(count * width),
        }
    }

    pub fn populate(
        &mut self,
        graph: &Graph,
        scene: &Scene,
        factors: &[BufferHandle],
        images: &[ImageHandle],
    ) -> Result<(), Error> {
        self.bindings.clear();
        for (material, &buffer) in scene.materials.iter().zip(factors) {
            self.bindings.push(MaterialBinding {
                name: "factors",
                resource: BindingResource::Buffer {
                    buffer,
                    offset: 0,
                    size: program::MATERIAL_BYTES as u64,
                },
            });
            for (name, index) in COLOR_TEXTURES.into_iter().zip(material.textures) {
                let view = graph.sampled_image_view(
                    images[index],
                    0,
                    scene.images[index].mips.len() as u32,
                )?;
                self.bindings.push(MaterialBinding {
                    name,
                    resource: BindingResource::ImageView(view),
                });
            }
        }
        Ok(())
    }

    pub fn inputs(&self) -> (&[FrameMaterial<'static>], &[MaterialBinding<'static>]) {
        (&self.rows, &self.bindings)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prepare(yaml: String) -> Result<Prepared, Error> {
        let options = Options::parse([String::from("--fixture")])?;
        let source = Source {
            sha256: digest(yaml.as_bytes()),
            yaml,
        };
        Prepared::prepare(Arc::new(source), &options, 1, 1)
    }

    #[test]
    fn scene_requires_initialized_hdr_and_depth_even_with_no_draws() {
        let yaml = include_str!("scene-forward.yaml");
        prepare(yaml.to_owned()).unwrap();
        let unused_depth = yaml
            .replace("resources:", "resources:\n  - { id: other_depth, kind: image, extent: frame, format: depth32_float }")
            .replace("image: depth,", "image: other_depth,");
        let error = prepare(unused_depth).err().expect("unwritten scene depth");
        assert!(error.to_string().contains("depth"), "{error}");
    }
}

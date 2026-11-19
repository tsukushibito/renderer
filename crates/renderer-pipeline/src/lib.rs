//! Prepare YAML pass connections and build a public Renderer Graph each frame.
//!
//! The application resolves trusted Programs and supplies graph-local frame
//! inputs. Renderer owns native preparation, scheduling and execution.

#![forbid(unsafe_code)]

mod frame;
mod prepare;
mod schema;

use std::{error::Error, fmt};

use renderer::capacity::{ResourceClass, WorkingSet};
use renderer::graph::{BufferDesc, GraphError, ImageDesc, LoadOp, RasterConstants, StoreOp};
use renderer::program::{
    ManagedProgram, ManagedRasterProgram, RasterPipelineRequest, SamplerDescriptor,
};

pub use frame::{
    DrawCandidate, FrameInputs, FrameMaterial, FrameResource, FrameView, MaterialBinding,
};

use frame::FrameScratch;

/// Logical asset keys resolved once when a YAML configuration is prepared.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ShaderRequest<'a> {
    /// A complete Program with a logical asset name.
    Program(&'a str),
    /// A Program previously built from the named template and material type.
    Template {
        template: &'a str,
        material_type: &'a str,
    },
}

/// A trusted Renderer Program returned by the application's logical-name resolver.
pub enum Program {
    Raster(ManagedRasterProgram),
    Compute(ManagedProgram),
}

/// Limits determined at an application preparation boundary.
#[derive(Clone, Copy, Debug)]
pub struct PrepareOptions {
    /// Width and height for images declared with `extent: frame`.
    pub frame_extent: Option<[u32; 2]>,
    /// Maximum concurrent frames using the transient classes in this configuration.
    pub frames_in_flight: usize,
    /// Maximum selected draws stored for each configured draw list.
    pub max_draws_per_list: usize,
    /// Maximum frame material instances referenced by indexed draws.
    pub max_materials: usize,
}

/// Graph declarations added by a pipeline of fullscreen Raster passes.
///
/// Each pass adds one draw. Size Graph analysis and app-owned resources and
/// passes separately from these declaration counts.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FullscreenRequirements {
    /// Transient resource declarations.
    pub resources: usize,
    pub passes: usize,
    pub bindings: usize,
    pub attachments: usize,
    pub raster_constant_words: usize,
    /// UTF-8 bytes of pass names and transient resource names.
    pub name_bytes: usize,
    /// An external image is declared as a color or depth attachment.
    /// The Graph treats attachments as writes regardless of load/store ops
    /// or depth state. This includes passes that the Graph may cull.
    pub writes_external_attachments: bool,
}

/// The source of a pipeline preparation or per-frame construction error.
#[derive(Debug)]
#[non_exhaustive]
pub enum PipelineError {
    Yaml(Box<serde_saphyr::Error>),
    Invalid(String),
    Resolve {
        name: String,
        source: Box<dyn Error + Send + Sync>,
    },
    Graph(GraphError),
    PassGraph {
        id: String,
        source: GraphError,
    },
}

impl fmt::Display for PipelineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Yaml(source) => write!(f, "pipeline YAML: {source}"),
            Self::Invalid(message) => f.write_str(message),
            Self::Resolve { name, source } => write!(f, "shader '{name}': {source}"),
            Self::Graph(source) => source.fmt(f),
            Self::PassGraph { id, source } => write!(f, "pipeline pass '{id}': {source}"),
        }
    }
}

impl Error for PipelineError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Yaml(source) => Some(source.as_ref()),
            Self::Invalid(_) => None,
            Self::Resolve { source, .. } => Some(source.as_ref()),
            Self::Graph(source) | Self::PassGraph { source, .. } => Some(source),
        }
    }
}

impl From<GraphError> for PipelineError {
    fn from(source: GraphError) -> Self {
        Self::Graph(source)
    }
}

/// Resolved passes, resources, Program versions and reusable construction storage.
pub struct PreparedPipeline {
    resources: Vec<Resource>,
    passes: Vec<Pass>,
    draw_lists: Vec<schema::DrawListSpec>,
    output: usize,
    raster: Vec<RasterPipelineRequest>,
    compute: Vec<ManagedProgram>,
    classes: Vec<ResourceClass>,
    scratch: FrameScratch,
}

impl PreparedPipeline {
    /// External resource names and their required descriptors in declaration order.
    pub fn external_resources(&self) -> impl Iterator<Item = (&str, ResourceDesc)> {
        self.resources
            .iter()
            .filter(|resource| resource.external)
            .map(|resource| (resource.id.as_str(), resource.desc))
    }

    /// The declared output image name and its prepared descriptor.
    pub fn output_resource(&self) -> (&str, ImageDesc) {
        let output = &self.resources[self.output];
        let ResourceDesc::Image(desc) = output.desc else {
            unreachable!("pipeline preparation requires an output image")
        };
        (&output.id, desc)
    }

    /// Count Graph declarations for a pipeline containing only fullscreen
    /// Raster passes, with no draw list inputs.
    pub fn fullscreen_requirements(&self) -> Option<FullscreenRequirements> {
        if !self.draw_lists.is_empty() {
            return None;
        }

        let mut requirements = FullscreenRequirements {
            passes: self.passes.len(),
            ..FullscreenRequirements::default()
        };
        for resource in self.resources.iter().filter(|resource| !resource.external) {
            requirements.resources += 1;
            requirements.name_bytes += resource.id.len();
        }
        for pass in &self.passes {
            let PassKind::Raster {
                colors,
                depth,
                draw: RasterDrawKind::Fullscreen(constants),
                ..
            } = &pass.kind
            else {
                return None;
            };
            requirements.bindings += pass.bindings.len();
            requirements.attachments += colors.len() + usize::from(depth.is_some());
            requirements.raster_constant_words += constants.words().len();
            requirements.name_bytes += pass.id.len();
            requirements.writes_external_attachments |= colors
                .iter()
                .map(|color| color.resource)
                .chain(depth.iter().map(|depth| depth.resource))
                .any(|resource| self.resources[resource].external);
        }
        Some(requirements)
    }

    /// Look up a prepared Raster Program for application-owned Graph assembly.
    /// A pass with a directly named Program uses that Program for any material type.
    pub fn raster_program(
        &self,
        pass_id: &str,
        material_type: &str,
    ) -> Option<&ManagedRasterProgram> {
        let pass = self.passes.iter().find(|pass| pass.id == pass_id)?;
        let PassKind::Raster { program, .. } = &pass.kind else {
            return None;
        };
        Some(&self.raster[program.select(material_type)?].program)
    }

    /// The transient classes and Program versions required by this pipeline.
    /// Include application-owned frame setup requirements in the runtime's full
    /// working set. Apply a new set at Renderer’s explicit preparation boundary.
    pub fn working_set(&self) -> WorkingSet<'_> {
        WorkingSet {
            resources: &self.classes,
            raster: &self.raster,
            compute: &self.compute,
        }
    }
}

struct Resource {
    id: String,
    external: bool,
    desc: ResourceDesc,
}

/// Descriptor of a prepared external pipeline resource.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResourceDesc {
    Image(ImageDesc),
    Buffer(BufferDesc),
}

struct Pass {
    id: String,
    bindings: Vec<PreparedBinding>,
    kind: PassKind,
}

enum PassKind {
    Raster {
        program: RasterPrograms,
        colors: Vec<PreparedAttachment<[f32; 4]>>,
        depth: Option<PreparedAttachment<f32>>,
        draw: RasterDrawKind,
        uses_materials: bool,
    },
    Compute {
        program: usize,
        grid: Option<[u32; 2]>,
    },
}

enum RasterPrograms {
    Fixed(usize),
    Material(Vec<MaterialVariant>),
}

impl RasterPrograms {
    fn select(&self, material_type: &str) -> Option<usize> {
        match self {
            Self::Fixed(program) => Some(*program),
            Self::Material(variants) => variants
                .iter()
                .find(|variant| variant.name == material_type)
                .map(|variant| variant.program),
        }
    }
}

struct MaterialVariant {
    name: String,
    program: usize,
}

enum RasterDrawKind {
    Indexed(usize),
    Fullscreen(RasterConstants),
}

struct PreparedAttachment<T> {
    resource: usize,
    load: LoadOp<T>,
    store: StoreOp,
}

struct PreparedBinding {
    group: u32,
    binding: u32,
    source: BindingSource,
}

enum BindingSource {
    Image(usize),
    Buffer {
        resource: usize,
        offset: u64,
        size: u64,
    },
    Sampler(SamplerDescriptor),
    Material(String),
}

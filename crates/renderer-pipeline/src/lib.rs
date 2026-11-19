//! Prepare YAML pass connections and build a public Renderer Graph each frame.
//!
//! The application resolves trusted Programs and supplies graph-local frame
//! inputs. Renderer owns native preparation, scheduling and execution.

#![forbid(unsafe_code)]

mod frame;
mod prepare;
mod schema;

use std::{error::Error, fmt};

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
    /// Maximum concurrent frames using the transient classes in this configuration.
    pub frames_in_flight: usize,
    /// Maximum selected draws stored for each configured draw list.
    pub max_draws_per_list: usize,
    /// Maximum frame material instances referenced by indexed draws.
    pub max_materials: usize,
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
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    classes: Vec<renderer::runtime::ResourceClass>,
    scratch: FrameScratch,
}

impl PreparedPipeline {
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
    #[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
    pub fn working_set(&self) -> renderer::runtime::WorkingSet<'_> {
        renderer::runtime::WorkingSet {
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum ResourceDesc {
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

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
    DrawCandidate, FrameInputs, FrameMaterial, FrameOutput, FrameResource, FrameSampledView,
    FrameView, MaterialBinding,
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
    /// Base width and height for images declared with `extent: frame`.
    pub frame_extent: Option<[u32; 2]>,
    /// Maximum concurrent frames using the transient classes in this configuration.
    pub frames_in_flight: usize,
    /// Maximum selected draws stored for each configured draw list.
    pub max_draws_per_list: usize,
    /// Maximum frame material instances referenced by indexed draws.
    pub max_materials: usize,
}

/// Upper bounds on Graph declarations added by a prepared pipeline.
///
/// Indexed pass bounds use the configured draw and material capacities.
/// Size Graph analysis and app-owned declarations separately.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PipelineRequirements {
    /// Transient resource declarations.
    pub resources: usize,
    pub passes: usize,
    pub compute_dispatches: usize,
    pub draw_lists: usize,
    pub draws: usize,
    pub bindings: usize,
    pub accesses: usize,
    pub attachments: usize,
    pub raster_constant_words: usize,
    /// UTF-8 bytes of pass names and transient resource names.
    pub name_bytes: usize,
    /// An external resource is declared as a Raster attachment or Compute
    /// storage write. Includes passes that the Graph may cull.
    pub writes_external_resources: bool,
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
    sampled_views: Vec<String>,
    passes: Vec<Pass>,
    draw_lists: Vec<schema::DrawListSpec>,
    output: usize,
    raster: Vec<RasterPipelineRequest>,
    compute: Vec<ManagedProgram>,
    classes: Vec<ResourceClass>,
    requirements: PipelineRequirements,
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

    /// Unique names required in [`FrameInputs::sampled_views`].
    pub fn sampled_view_names(&self) -> impl Iterator<Item = &str> {
        self.sampled_views.iter().map(String::as_str)
    }

    /// The declared output image name and its prepared descriptor.
    pub fn output_resource(&self) -> (&str, ImageDesc) {
        let output = &self.resources[self.output];
        let ResourceDesc::Image(desc) = output.desc else {
            unreachable!("pipeline preparation requires an output image")
        };
        (&output.id, desc)
    }

    /// Look up the prepared descriptor of a named image.
    pub fn image_resource(&self, name: &str) -> Option<ImageDesc> {
        let resource = self.resources.iter().find(|resource| resource.id == name)?;
        let ResourceDesc::Image(desc) = resource.desc else {
            return None;
        };
        Some(desc)
    }

    /// Graph declaration bounds against the limits supplied at preparation.
    pub fn requirements(&self) -> PipelineRequirements {
        self.requirements
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
    SampledView(usize),
    Buffer {
        resource: usize,
        offset: u64,
        size: u64,
    },
    Sampler(SamplerDescriptor),
    Material(String),
}

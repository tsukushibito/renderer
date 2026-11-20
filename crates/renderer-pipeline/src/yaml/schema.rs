use std::collections::BTreeMap;

use renderer::ImageFormat;
use renderer::graph::StoreOp;
use renderer::program::SamplerDescriptor;
use serde::Deserialize;

use super::PipelineError;
use super::assembly::Parameter;

pub(super) const MAX_YAML_BYTES: usize = 1024 * 1024;

pub(super) fn parse(yaml: &str) -> Result<Config, PipelineError> {
    let options = serde_saphyr::options! {
        strict_booleans: true,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        reject_unsupported_tags: true,
        budget: serde_saphyr::budget! {
            max_documents: 1,
            max_aliases: 0,
            max_anchors: 0,
            max_depth: 24,
        },
    };
    serde_saphyr::from_str_with_options(yaml, options)
        .map_err(|error| PipelineError::Yaml(Box::new(error)))
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub version: u32,
    #[serde(default)]
    pub resources: Vec<ResourceSpec>,
    #[serde(default)]
    pub draw_lists: Vec<DrawListSpec>,
    #[serde(default)]
    pub raster_batches: BTreeMap<String, Vec<RasterBatchSpec>>,
    #[serde(default)]
    pub passes: Vec<PassSpec>,
    pub output: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DrawListSpec {
    pub id: String,
    pub view: String,
    pub queue: String,
}

#[derive(Clone, Copy, Default, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResourceSource {
    #[default]
    Transient,
    External,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum ImageSourceSpec {
    Fixed(ResourceSource),
    Selectable(SelectableImageSource),
}

impl Default for ImageSourceSpec {
    fn default() -> Self {
        Self::Fixed(ResourceSource::Transient)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SelectableImageSource {
    pub render: String,
    pub reuse: ReuseSource,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReuseSource {
    FrameInput,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ResourceSpec {
    Image {
        id: String,
        #[serde(default)]
        source: ImageSourceSpec,
        width: Option<u32>,
        height: Option<u32>,
        extent: Option<ImageExtent>,
        divisor: Option<u32>,
        format: Format,
        samples: Option<u32>,
    },
    Buffer {
        id: String,
        #[serde(default)]
        source: ResourceSource,
        bytes: Parameter<u64>,
    },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ImageExtent {
    Frame,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Format {
    Rgba8Unorm,
    Rgba8Srgb,
    Rgba16Float,
    Depth32Float,
}

impl From<Format> for ImageFormat {
    fn from(format: Format) -> Self {
        match format {
            Format::Rgba8Unorm => Self::Rgba8Unorm,
            Format::Rgba8Srgb => Self::Rgba8Srgb,
            Format::Rgba16Float => Self::Rgba16Float,
            Format::Depth32Float => Self::Depth32Float,
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum PassSpec {
    Raster {
        id: String,
        shader: Option<String>,
        shader_template: Option<String>,
        #[serde(default)]
        material_types: Vec<String>,
        #[serde(default)]
        colors: Vec<AttachmentSpec<[f32; 4]>>,
        depth: Option<AttachmentSpec<f32>>,
        when: Option<DrawListConditionSpec>,
        batches: Option<RasterBatchesSpec>,
        draw_list: Option<String>,
        indirect: Option<String>,
        #[serde(default)]
        constants: Vec<u32>,
        #[serde(default)]
        bindings: Vec<BindingSpec>,
    },
    Attachment {
        id: String,
        #[serde(default)]
        colors: Vec<AttachmentSpec<[f32; 4]>>,
        depth: Option<AttachmentSpec<f32>>,
        when: Option<DrawListConditionSpec>,
    },
    Compute {
        id: String,
        shader: String,
        #[serde(default)]
        bindings: Vec<BindingSpec>,
        grid: Option<Parameter<[u32; 2]>>,
    },
    EncodeIndexedIndirect {
        id: String,
        visibility: String,
        table_list: String,
    },
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct RasterBatchSpec {
    pub shader: Option<String>,
    pub shader_template: Option<String>,
    #[serde(default)]
    pub material_types: Vec<String>,
    pub draw_list: Option<String>,
    pub indirect: Option<String>,
    #[serde(default)]
    pub constants: Vec<u32>,
    #[serde(default)]
    pub bindings: Vec<BindingSpec>,
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum RasterBatchesSpec {
    Inline(Vec<RasterBatchSpec>),
    Named(String),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DrawListConditionSpec {
    pub draw_list: String,
    pub empty: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AttachmentSpec<T> {
    pub image: String,
    pub load: LoadAction,
    pub clear: Option<Parameter<T>>,
    pub resolve: Option<String>,
    #[serde(default)]
    pub store: StoreAction,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum LoadAction {
    Load,
    Clear,
    Discard,
}

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum StoreAction {
    #[default]
    Store,
    Discard,
}

impl From<StoreAction> for StoreOp {
    fn from(store: StoreAction) -> Self {
        match store {
            StoreAction::Store => Self::Store,
            StoreAction::Discard => Self::Discard,
        }
    }
}

#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum BindingSpec {
    Image {
        group: u32,
        binding: u32,
        resource: String,
    },
    SampledView {
        group: u32,
        binding: u32,
        resource: String,
    },
    Buffer {
        group: u32,
        binding: u32,
        resource: String,
        offset: u64,
        size: Parameter<u64>,
    },
    Sampler {
        group: u32,
        binding: u32,
        sampler: Sampler,
    },
    Material {
        group: u32,
        binding: u32,
        resource: String,
    },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Sampler {
    NearestClamp,
    LinearRepeat,
    LinearClamp,
    #[serde(rename = "linear_repeat_u_clamp_v")]
    LinearRepeatUClampV,
}

impl From<Sampler> for SamplerDescriptor {
    fn from(sampler: Sampler) -> Self {
        match sampler {
            Sampler::NearestClamp => Self::NearestClamp,
            Sampler::LinearRepeat => Self::LinearRepeat,
            Sampler::LinearClamp => Self::LinearClamp,
            Sampler::LinearRepeatUClampV => Self::LinearRepeatUClampV,
        }
    }
}

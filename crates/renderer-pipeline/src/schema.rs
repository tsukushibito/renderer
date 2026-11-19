use renderer::ImageFormat;
use renderer::graph::StoreOp;
use renderer::program::SamplerDescriptor;
use serde::Deserialize;

use crate::PipelineError;

pub(super) fn parse(yaml: &str) -> Result<Config, PipelineError> {
    if yaml.len() > 1024 * 1024 {
        return Err(PipelineError::Invalid("YAML exceeds 1 MiB".into()));
    }
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Config {
    pub version: u32,
    pub resources: Vec<ResourceSpec>,
    #[serde(default)]
    pub draw_lists: Vec<DrawListSpec>,
    pub passes: Vec<PassSpec>,
    pub output: String,
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
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ResourceSpec {
    Image {
        id: String,
        #[serde(default)]
        source: ResourceSource,
        width: u32,
        height: u32,
        format: Format,
    },
    Buffer {
        id: String,
        #[serde(default)]
        source: ResourceSource,
        bytes: u64,
    },
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
        shader: String,
        #[serde(default)]
        colors: Vec<AttachmentSpec<[f32; 4]>>,
        depth: Option<AttachmentSpec<f32>>,
        draw_list: Option<String>,
        #[serde(default)]
        constants: Vec<u32>,
        #[serde(default)]
        bindings: Vec<BindingSpec>,
    },
    Compute {
        id: String,
        shader: String,
        #[serde(default)]
        bindings: Vec<BindingSpec>,
        grid: Option<[u32; 2]>,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct AttachmentSpec<T> {
    pub image: String,
    pub load: LoadAction,
    pub clear: Option<T>,
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

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum BindingSpec {
    Image {
        group: u32,
        binding: u32,
        resource: String,
    },
    Buffer {
        group: u32,
        binding: u32,
        resource: String,
        offset: u64,
        size: u64,
    },
    Sampler {
        group: u32,
        binding: u32,
        sampler: Sampler,
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

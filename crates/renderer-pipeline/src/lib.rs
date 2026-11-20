//! Compose public Renderer Graphs with reusable drawing passes and recipes.
//!
//! With the `yaml` feature, applications can also prepare declarative
//! pipelines, resolve trusted Programs, and supply graph-local frame inputs.
//! Renderer owns native preparation, scheduling and execution.

#![forbid(unsafe_code)]

pub mod standard;

#[cfg(feature = "yaml")]
mod yaml;
#[cfg(feature = "yaml")]
pub use yaml::{
    DrawCandidate, FrameImageSource, FrameIndirectGroup, FrameIndirectList, FrameInputs,
    FrameMaterial, FrameOutput, FrameResource, FrameSampledView, FrameView, ImageSource,
    IndirectTable, MaterialBinding, PipelineError, PipelineRequirements, PrepareIndirectList,
    PrepareInputs, PrepareOptions, PrepareParameter, PrepareValue, PreparedPipeline, Program,
    ResourceDesc, ShaderRequest,
};

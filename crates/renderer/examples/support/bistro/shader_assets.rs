//! Opt-in Bistro acceptance for the offline material shader bundle.
//!
//! Bistro supplies its existing attachments, cascade policy, and draw bindings.
//! The general YAML parser selects the Programs for those existing passes.

use std::{
    fs, io,
    path::{Component, Path, PathBuf},
};

use renderer::program::{
    D3D12InlineConstantBinding, D3D12RasterShaders, ManagedRasterProgram, RasterProgramDescriptor,
    RasterShaderStages, ShaderBinaries,
};
use renderer_pipeline::{PrepareOptions, PreparedPipeline, Program, ShaderRequest};
use serde_json::Value;

use super::{Error, digest, program};

type AssetError = Box<dyn std::error::Error + Send + Sync>;

const MATERIAL: &str = "bistro/pbr";

#[derive(Clone, Debug)]
pub(super) struct Selection {
    pub pipeline: PathBuf,
    pub manifest: PathBuf,
    pub programs: Option<Programs>,
}

#[derive(Clone, Debug)]
pub(super) struct Programs {
    pub forward: ManagedRasterProgram,
    pub blend: ManagedRasterProgram,
    pub shadow: ManagedRasterProgram,
}

impl Selection {
    pub(super) fn prepare(&mut self, api: &str, frames: usize) -> Result<(), Error> {
        let yaml = fs::read_to_string(&self.pipeline)?;
        let manifest: Value = serde_json::from_slice(&fs::read(&self.manifest)?)?;
        if manifest["schema_version"].as_u64() != Some(1) {
            return Err("unsupported Bistro shader manifest schema_version".into());
        }
        let bundle = Bundle {
            directory: self
                .manifest
                .parent()
                .ok_or("shader manifest has no parent")?,
            entries: manifest["programs"]
                .as_array()
                .ok_or("shader manifest has no programs array")?,
        };
        let pipeline = PreparedPipeline::from_yaml(
            &yaml,
            PrepareOptions {
                frames_in_flight: frames,
                max_draws_per_list: 0,
                max_materials: 0,
            },
            |request| bundle.resolve(api, request),
        )?;
        self.programs = Some(Programs {
            forward: selected(&pipeline, "forward", program::opaque_program())?,
            blend: selected(&pipeline, "forward_blend", program::blend_program())?,
            shadow: selected(&pipeline, "shadow", program::shadow_program())?,
        });
        Ok(())
    }
}

fn selected(
    pipeline: &PreparedPipeline,
    pass: &str,
    expected: ManagedRasterProgram,
) -> Result<ManagedRasterProgram, Error> {
    let program = pipeline.raster_program(pass, MATERIAL).ok_or_else(|| {
        format!("Bistro shader pipeline requires a Raster selection for {pass} and {MATERIAL}")
    })?;
    let actual = program.descriptor();
    let expected = expected.descriptor();
    if actual.entry != expected.entry
        || actual.vertex_stride != expected.vertex_stride
        || actual.vertex_attributes != expected.vertex_attributes
        || actual.parameters != expected.parameters
        || actual.color_outputs != expected.color_outputs
        || actual.bindings != expected.bindings
        || actual.binding_bytes != expected.binding_bytes
        || actual.sampling_pairs != expected.sampling_pairs
        || actual.blend != expected.blend
        || actual.depth != expected.depth
    {
        return Err(format!("Bistro shader ABI differs from the reviewed ABI for {pass}").into());
    }
    Ok(program.clone())
}

struct Bundle<'a> {
    directory: &'a Path,
    entries: &'a [Value],
}

impl Bundle<'_> {
    fn resolve(&self, api: &str, request: ShaderRequest<'_>) -> Result<Program, AssetError> {
        let program = match request {
            ShaderRequest::Program("bistro/forward") => program::opaque_program(),
            ShaderRequest::Program("bistro/forward-blend") => program::blend_program(),
            ShaderRequest::Program("bistro/shadow") => program::shadow_program(),
            ShaderRequest::Template {
                template,
                material_type: MATERIAL,
            } => self.raster(api, template)?,
            _ => return Err(invalid("unknown Bistro shader selection")),
        };
        Ok(Program::Raster(program))
    }

    fn raster(&self, api: &str, template: &str) -> Result<ManagedRasterProgram, AssetError> {
        let expected = match template {
            "bistro/forward" => program::opaque_program(),
            "bistro/shadow" => program::shadow_program(),
            _ => {
                return Err(invalid(format!(
                    "unknown Bistro shader template {template}"
                )));
            }
        };
        let mut matches = self
            .entries
            .iter()
            .filter(|entry| entry["template"] == template && entry["material"] == MATERIAL);
        let entry = matches
            .next()
            .ok_or_else(|| invalid(format!("shader bundle has no {template} for {MATERIAL}")))?;
        if matches.next().is_some() {
            return Err(invalid(format!(
                "shader bundle repeats {template} for {MATERIAL}"
            )));
        }
        let (format, vertex, fragment) = reviewed_binary(api, template)?;
        let stages = RasterShaderStages {
            vertex: self.read(&entry["vertex"], format, vertex)?,
            fragment: Some(self.read(&entry["fragment"], format, fragment)?),
        };
        let shaders = if format == "spv" {
            ShaderBinaries::vulkan(stages)
        } else {
            ShaderBinaries::d3d12(D3D12RasterShaders {
                stages,
                inline_constants: (template == "bistro/shadow").then_some(
                    D3D12InlineConstantBinding {
                        register: 7,
                        space: 0,
                    },
                ),
            })
        };
        let descriptor = expected.descriptor();
        // SAFETY: Both stages were checked byte-for-byte against the reviewed
        // Bistro program for this template. Its descriptor fixes the matching
        // ABI and pipeline state, independent of what the bundle declares.
        Ok(unsafe {
            ManagedRasterProgram::from_trusted_owned_descriptor(RasterProgramDescriptor {
                entry: descriptor.entry,
                vertex_stride: descriptor.vertex_stride,
                vertex_attributes: descriptor.vertex_attributes,
                parameters: descriptor.parameters,
                color_outputs: descriptor.color_outputs,
                bindings: descriptor.bindings,
                binding_bytes: descriptor.binding_bytes,
                sampling_pairs: descriptor.sampling_pairs,
                blend: descriptor.blend,
                depth: descriptor.depth,
                shaders,
            })
        }?)
    }

    fn read(&self, stage: &Value, format: &str, expected: &[u8]) -> Result<Vec<u8>, AssetError> {
        let path = stage[format]
            .as_str()
            .ok_or_else(|| invalid(format!("shader stage has no {format}")))?;
        let path = Path::new(path);
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(invalid(
                "shader artifact path must be relative to the bundle directory",
            ));
        }
        let path = self.directory.join(path);
        let bytes = fs::read(&path)?;
        if bytes != expected
            || stage[format!("{format}_sha256")].as_str() != Some(digest(&bytes).as_str())
        {
            return Err(invalid(format!(
                "shader artifact differs from the reviewed Bistro stage: {}",
                path.display()
            )));
        }
        Ok(bytes)
    }
}

fn reviewed_binary(
    api: &str,
    template: &str,
) -> Result<(&'static str, &'static [u8], &'static [u8]), AssetError> {
    match (api, template) {
        #[cfg(feature = "vulkan")]
        ("vulkan", "bistro/forward") => Ok((
            "spv",
            include_bytes!("shaders/pbr.vert.spv"),
            include_bytes!("shaders/pbr.frag.spv"),
        )),
        #[cfg(feature = "vulkan")]
        ("vulkan", "bistro/shadow") => Ok((
            "spv",
            include_bytes!("shaders/shadow.vert.spv"),
            include_bytes!("shaders/shadow.frag.spv"),
        )),
        #[cfg(all(windows, feature = "d3d12"))]
        ("d3d12", "bistro/forward") => Ok((
            "dxil",
            include_bytes!("shaders/dxil/pbr.vert.dxil"),
            include_bytes!("shaders/dxil/pbr.frag.dxil"),
        )),
        #[cfg(all(windows, feature = "d3d12"))]
        ("d3d12", "bistro/shadow") => Ok((
            "dxil",
            include_bytes!("shaders/dxil/shadow.vert.dxil"),
            include_bytes!("shaders/dxil/shadow.frag.dxil"),
        )),
        _ => Err(invalid(format!(
            "Bistro shader template {template} is unavailable for {api}"
        ))),
    }
}

fn invalid(message: impl Into<String>) -> AssetError {
    io::Error::new(io::ErrorKind::InvalidData, message.into()).into()
}

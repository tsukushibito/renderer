//! Immutable native graphics programs, created only at working-set preparation.

use std::mem::ManuallyDrop;

use crate::program::{
    BindingType, BlendState, CompareOp, ImageFormat, ManagedRasterProgram, ShaderStages,
    VertexFormat,
};

use super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{NativeObject, object},
};

pub(super) const INLINE_ROOT: u32 = 0;

fn visibility(stages: ShaderStages) -> D3D12_SHADER_VISIBILITY {
    if stages == ShaderStages::VERTEX {
        D3D12_SHADER_VISIBILITY_VERTEX
    } else if stages == ShaderStages::FRAGMENT {
        D3D12_SHADER_VISIBILITY_PIXEL
    } else {
        D3D12_SHADER_VISIBILITY_ALL
    }
}

fn vertex_format(format: VertexFormat) -> DXGI_FORMAT {
    match format {
        VertexFormat::Float32x2 => DXGI_FORMAT_R32G32_FLOAT,
        VertexFormat::Float32x3 => DXGI_FORMAT_R32G32B32_FLOAT,
        VertexFormat::Float32x4 => DXGI_FORMAT_R32G32B32A32_FLOAT,
    }
}

fn comparison(compare: CompareOp) -> D3D12_COMPARISON_FUNC {
    match compare {
        CompareOp::Never => D3D12_COMPARISON_FUNC_NEVER,
        CompareOp::Less => D3D12_COMPARISON_FUNC_LESS,
        CompareOp::Equal => D3D12_COMPARISON_FUNC_EQUAL,
        CompareOp::LessEqual => D3D12_COMPARISON_FUNC_LESS_EQUAL,
        CompareOp::Greater => D3D12_COMPARISON_FUNC_GREATER,
        CompareOp::NotEqual => D3D12_COMPARISON_FUNC_NOT_EQUAL,
        CompareOp::GreaterEqual => D3D12_COMPARISON_FUNC_GREATER_EQUAL,
        CompareOp::Always => D3D12_COMPARISON_FUNC_ALWAYS,
    }
}

fn blend(state: BlendState) -> D3D12_RENDER_TARGET_BLEND_DESC {
    let (source, destination, alpha_destination) = match state {
        BlendState::Replace => (D3D12_BLEND_ONE, D3D12_BLEND_ZERO, D3D12_BLEND_ZERO),
        BlendState::Alpha => (
            D3D12_BLEND_SRC_ALPHA,
            D3D12_BLEND_INV_SRC_ALPHA,
            D3D12_BLEND_INV_SRC_ALPHA,
        ),
        BlendState::PremultipliedAlpha => (
            D3D12_BLEND_ONE,
            D3D12_BLEND_INV_SRC_ALPHA,
            D3D12_BLEND_INV_SRC_ALPHA,
        ),
        BlendState::Additive => (D3D12_BLEND_ONE, D3D12_BLEND_ONE, D3D12_BLEND_ONE),
    };
    D3D12_RENDER_TARGET_BLEND_DESC {
        BlendEnable: (state != BlendState::Replace).into(),
        LogicOpEnable: false.into(),
        SrcBlend: source,
        DestBlend: destination,
        BlendOp: D3D12_BLEND_OP_ADD,
        SrcBlendAlpha: D3D12_BLEND_ONE,
        DestBlendAlpha: alpha_destination,
        BlendOpAlpha: D3D12_BLEND_OP_ADD,
        LogicOp: D3D12_LOGIC_OP_NOOP,
        RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL as u8,
    }
}

/// FramePool retains the Device after all pipelines and slot leases. These
/// immutable free-threaded COM objects remain live through recording and GPU use.
pub(super) struct RasterPipeline {
    pub(super) pipeline: NativeObject<ID3D12PipelineState>,
    pub(super) signature: NativeObject<ID3D12RootSignature>,
    pub(super) managed: ManagedRasterProgram,
    pub(super) depth: bool,
    clear: bool,
}
// SAFETY: Native pipeline/signature interfaces are free-threaded. Their immutable
// descriptors are never modified; the containing FramePool retains the Device.
unsafe impl Send for RasterPipeline {}
// SAFETY: Shared access only borrows immutable free-threaded native interfaces.
unsafe impl Sync for RasterPipeline {}

fn require_format(
    owner: &DeviceOwner,
    format: DXGI_FORMAT,
    required: D3D12_FORMAT_SUPPORT1,
) -> Result<(), D3D12RuntimeError> {
    let mut support = D3D12_FEATURE_DATA_FORMAT_SUPPORT {
        Format: format,
        ..Default::default()
    };
    // SAFETY: The feature enum, initialized output type and byte count match.
    unsafe {
        owner.raw.CheckFeatureSupport(
            D3D12_FEATURE_FORMAT_SUPPORT,
            std::ptr::from_mut(&mut support).cast(),
            size_of_val(&support) as u32,
        )
    }
    .ok()
    .map_err(|error| owner.native("CheckFeatureSupport(Raster format)", error))?;
    if support.Support1 & required != required {
        return Err(D3D12RuntimeError::unsupported(
            "Raster vertex or attachment format is unsupported",
        ));
    }
    Ok(())
}

fn signature(
    owner: &DeviceOwner,
    managed: &ManagedRasterProgram,
) -> Result<NativeObject<ID3D12RootSignature>, D3D12RuntimeError> {
    let inline = managed.parameter_layout();
    let cost = managed
        .bindings()
        .iter()
        .try_fold(inline.size / 4, |cost, slot| {
            cost.checked_add(match slot.kind {
                BindingType::Uniform | BindingType::StorageRead => 2,
                BindingType::SampledImage | BindingType::Sampler => 1,
                _ => unreachable!("trusted Raster only has read-only bindings"),
            })
        });
    if cost.is_none_or(|cost| cost > 64) {
        return Err(D3D12RuntimeError::unsupported(
            "Raster layout exceeds D3D12 root signature DWORD capacity",
        ));
    }
    // Build every range before taking pointers; the allocation stays immutable
    // until synchronous root-signature serialization returns.
    let ranges: Vec<_> = managed
        .bindings()
        .iter()
        .filter_map(|slot| {
            let kind = match slot.kind {
                BindingType::SampledImage => D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                BindingType::Sampler => D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER,
                _ => return None,
            };
            Some(D3D12_DESCRIPTOR_RANGE {
                RangeType: kind,
                NumDescriptors: 1,
                BaseShaderRegister: slot.binding,
                RegisterSpace: slot.group,
                OffsetInDescriptorsFromTableStart: 0,
            })
        })
        .collect();
    let mut parameters =
        Vec::with_capacity(managed.bindings().len() + usize::from(inline.size != 0));
    if inline.size != 0 {
        parameters.push(D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS {
                    ShaderRegister: inline.dxil_register,
                    RegisterSpace: inline.dxil_space,
                    Num32BitValues: inline.size / 4,
                },
            },
            ShaderVisibility: visibility(inline.stages),
        });
    }
    let mut ranges = ranges.iter();
    for slot in managed.bindings() {
        let (kind, value) = match slot.kind {
            BindingType::Uniform | BindingType::StorageRead => (
                if slot.kind == BindingType::Uniform {
                    D3D12_ROOT_PARAMETER_TYPE_CBV
                } else {
                    D3D12_ROOT_PARAMETER_TYPE_SRV
                },
                D3D12_ROOT_PARAMETER_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR {
                        ShaderRegister: slot.binding,
                        RegisterSpace: slot.group,
                    },
                },
            ),
            BindingType::SampledImage | BindingType::Sampler => (
                D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                D3D12_ROOT_PARAMETER_0 {
                    DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                        NumDescriptorRanges: 1,
                        pDescriptorRanges: ranges.next().unwrap(),
                    },
                },
            ),
            _ => unreachable!("trusted Raster only has read-only bindings"),
        };
        parameters.push(D3D12_ROOT_PARAMETER {
            ParameterType: kind,
            Anonymous: value,
            ShaderVisibility: visibility(slot.stages),
        });
    }
    let desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: parameters.len() as u32,
        pParameters: parameters.as_ptr(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
        ..Default::default()
    };
    let mut blob = None;
    let mut errors = None;
    // SAFETY: Tagged parameters agree with the trusted DXIL ABI; all nested
    // pointers refer into stable preparation arrays that outlive synchronous encoding.
    unsafe {
        D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, &mut errors)
    }
    .ok()
    .map_err(|error| owner.native("D3D12SerializeRootSignature(Raster)", error))?;
    let blob = blob.ok_or_else(|| {
        D3D12RuntimeError::invalid("Raster root signature serialization returned no blob")
    })?;
    // SAFETY: A successful live serialization blob owns its readable byte range.
    let bytes = unsafe {
        let pointer = blob.GetBufferPointer();
        let size = blob.GetBufferSize();
        if pointer.is_null() || size == 0 || size > isize::MAX as usize {
            return Err(D3D12RuntimeError::invalid(
                "invalid Raster root signature serialization blob",
            ));
        }
        std::slice::from_raw_parts(pointer.cast::<u8>(), size)
    };
    // SAFETY: The valid serialized v1 signature and live Device remain retained
    // through synchronous creation; the owning result cleans up partial work.
    object(
        owner,
        "ID3D12Device::CreateRootSignature(Raster)",
        || unsafe { owner.raw.CreateRootSignature(0, bytes) },
    )
}

impl RasterPipeline {
    pub(super) fn matches(&self, managed: &ManagedRasterProgram, depth: bool) -> bool {
        !self.clear && self.depth == depth && self.managed.same(managed)
    }

    pub(super) fn new(
        owner: &DeviceOwner,
        managed: &ManagedRasterProgram,
        depth: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        Self::new_inner(owner, managed, depth, false)
    }

    /// Prepares an untextured fullscreen clear program. Depth writes replace
    /// existing values regardless of their prior contents; color writes use the
    /// same unblended attachment output as ordinary mesh drawing.
    pub(super) fn clear(
        owner: &DeviceOwner,
        managed: &ManagedRasterProgram,
        depth: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        if !managed.bindings().is_empty() {
            return Err(D3D12RuntimeError::invalid(
                "fullscreen clear requires an untextured Raster program",
            ));
        }
        Self::new_inner(owner, managed, depth, true)
    }

    fn new_inner(
        owner: &DeviceOwner,
        managed: &ManagedRasterProgram,
        depth: bool,
        clear: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        owner.ensure_active()?;
        for slot in managed.bindings() {
            let maximum = match slot.kind {
                BindingType::Uniform => 65_536,
                BindingType::StorageRead => super::resources::MAX_BUFFER_BYTES,
                _ => continue,
            };
            if slot.min_size > maximum {
                return Err(D3D12RuntimeError::unsupported(
                    "Raster buffer ABI exceeds D3D12 descriptor range limits",
                ));
            }
        }
        if owner.info.shader_model < D3D_SHADER_MODEL_6_0 as u32 {
            return Err(D3D12RuntimeError::unsupported(
                "Raster programs require Shader Model 6.0",
            ));
        }
        if managed.requires_depth() && !depth {
            return Err(D3D12RuntimeError::invalid(
                "Raster Program requires a depth attachment",
            ));
        }
        if managed.vertex_stride() > 2048 {
            return Err(D3D12RuntimeError::unsupported(
                "Raster vertex stride exceeds the D3D12 input-assembler limit",
            ));
        }
        let attributes: Vec<_> = managed
            .vertex_attributes()
            .iter()
            .map(|attribute| D3D12_INPUT_ELEMENT_DESC {
                SemanticName: windows_core::s!("TEXCOORD"),
                SemanticIndex: attribute.location,
                Format: vertex_format(attribute.format),
                InputSlot: 0,
                AlignedByteOffset: attribute.offset,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            })
            .collect();
        for attribute in &attributes {
            require_format(
                owner,
                attribute.Format,
                D3D12_FORMAT_SUPPORT1_IA_VERTEX_BUFFER,
            )?;
        }
        if depth {
            require_format(
                owner,
                DXGI_FORMAT_D32_FLOAT,
                D3D12_FORMAT_SUPPORT1_DEPTH_STENCIL,
            )?;
        }
        let mut formats = [DXGI_FORMAT_UNKNOWN; 8];
        for (destination, format) in formats.iter_mut().zip(managed.color_outputs()) {
            *destination = match format {
                ImageFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
                ImageFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
                ImageFormat::D32Float => {
                    return Err(D3D12RuntimeError::unsupported(
                        "Raster color output cannot use a depth format",
                    ));
                }
            };
            require_format(
                owner,
                *destination,
                D3D12_FORMAT_SUPPORT1_RENDER_TARGET
                    | if managed.blend() != BlendState::Replace {
                        D3D12_FORMAT_SUPPORT1_BLENDABLE
                    } else {
                        0
                    },
            )?;
        }
        let signature = signature(owner, managed)?;
        let target_blend = blend(if clear {
            BlendState::Replace
        } else {
            managed.blend()
        });
        let stencil = D3D12_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D12_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
            StencilPassOp: D3D12_STENCIL_OP_KEEP,
            StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
        };
        let vertex = managed.vertex_dxil();
        let fragment = managed.fragment_dxil();
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: ManuallyDrop::new(Some(signature.clone())),
            VS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: vertex.as_ptr().cast(),
                BytecodeLength: vertex.len(),
            },
            PS: if fragment.is_empty() {
                D3D12_SHADER_BYTECODE::default()
            } else {
                D3D12_SHADER_BYTECODE {
                    pShaderBytecode: fragment.as_ptr().cast(),
                    BytecodeLength: fragment.len(),
                }
            },
            BlendState: D3D12_BLEND_DESC {
                AlphaToCoverageEnable: false.into(),
                IndependentBlendEnable: false.into(),
                RenderTarget: [target_blend; 8],
            },
            SampleMask: u32::MAX,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                FrontCounterClockwise: true.into(),
                DepthClipEnable: true.into(),
                ConservativeRaster: D3D12_CONSERVATIVE_RASTERIZATION_MODE_OFF,
                ..Default::default()
            },
            DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                DepthEnable: (depth && (clear || managed.depth().compare().is_some())).into(),
                DepthWriteMask: if depth && (clear || managed.depth().write_enabled()) {
                    D3D12_DEPTH_WRITE_MASK_ALL
                } else {
                    D3D12_DEPTH_WRITE_MASK_ZERO
                },
                DepthFunc: if clear {
                    D3D12_COMPARISON_FUNC_ALWAYS
                } else {
                    comparison(managed.depth().compare().unwrap_or(CompareOp::Always))
                },
                StencilEnable: false.into(),
                StencilReadMask: u8::MAX,
                StencilWriteMask: u8::MAX,
                FrontFace: stencil,
                BackFace: stencil,
            },
            InputLayout: D3D12_INPUT_LAYOUT_DESC {
                pInputElementDescs: attributes.as_ptr(),
                NumElements: attributes.len() as u32,
            },
            IBStripCutValue: D3D12_INDEX_BUFFER_STRIP_CUT_VALUE_DISABLED,
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: managed.color_outputs().len() as u32,
            RTVFormats: formats,
            DSVFormat: if depth {
                DXGI_FORMAT_D32_FLOAT
            } else {
                DXGI_FORMAT_UNKNOWN
            },
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        // SAFETY: The reviewed shader ABI, input layout, signature, attachment
        // capabilities and single-sample fixed state agree. All input bytecode
        // and native references outlive this synchronous creation operation.
        let pipeline = object(
            owner,
            "ID3D12Device::CreateGraphicsPipelineState",
            || unsafe { owner.raw.CreateGraphicsPipelineState(&desc) },
        );
        // SAFETY: This generated field suppresses Drop; balance our temporary
        // clone after the synchronous call on both success and failure paths.
        unsafe { ManuallyDrop::drop(&mut desc.pRootSignature) };
        Ok(Self {
            pipeline: pipeline?,
            signature,
            managed: *managed,
            depth,
            clear,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::d3d12::{D3D12RuntimeErrorKind, D3D12ValidationMode};

    #[test]
    #[ignore = "requires real D3D12 hardware and requested native diagnostics"]
    fn d3d12_raster_pipeline_working_set_creation() {
        let strict = cfg!(feature = "development-validation");
        let owner = DeviceOwner::new(if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        })
        .expect("hardware D3D12 Device and requested native diagnostics");
        let monitor = owner.monitor.clone();
        let depth_only = ManagedRasterProgram::depth_only();
        match RasterPipeline::new(&owner, &depth_only, false) {
            Err(error) => assert_eq!(error.kind(), D3D12RuntimeErrorKind::InvalidInput),
            Ok(_) => panic!("depth-only pipeline requires a depth attachment"),
        }
        for program in [
            ManagedRasterProgram::mesh(),
            ManagedRasterProgram::textured_mesh(),
            ManagedRasterProgram::mesh_hdr(),
            ManagedRasterProgram::textured_mesh_hdr(),
            ManagedRasterProgram::mesh_prepassed(),
            ManagedRasterProgram::mesh_hdr_prepassed(),
            depth_only,
        ] {
            for depth in [false, true] {
                if program.requires_depth() && !depth {
                    continue;
                }
                let pipeline = RasterPipeline::new(&owner, &program, depth)
                    .expect("reviewed graphics shader ABI and attachment capabilities");
                assert!(pipeline.matches(&program, depth));
                assert!(!pipeline.matches(&program, !depth));
                if !program.same(&ManagedRasterProgram::mesh()) {
                    assert!(!pipeline.matches(&ManagedRasterProgram::mesh(), depth));
                }
                drop(pipeline);
            }
        }
        for (program, depth) in [
            (ManagedRasterProgram::mesh(), false),
            (ManagedRasterProgram::mesh_hdr(), false),
            (depth_only, true),
        ] {
            let pipeline = RasterPipeline::clear(&owner, &program, depth)
                .expect("reviewed fullscreen color/depth clear program");
            assert!(!pipeline.matches(&program, depth));
            drop(pipeline);
        }
        drop(owner);
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:?}");
        if strict {
            assert!(
                report.debug_layer_enabled
                    && report.gpu_based_validation_enabled
                    && report.teardown_checked,
                "{report:?}"
            );
        } else {
            assert!(!report.debug_layer_enabled && !report.gpu_based_validation_enabled);
        }
    }
}

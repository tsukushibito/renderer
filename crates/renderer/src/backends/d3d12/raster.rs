//! Immutable native graphics programs, created only at working-set preparation.

use std::mem::ManuallyDrop;

use crate::program::{ImageFormat, ManagedRasterProgram};

use super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{NativeObject, object},
};

pub(super) const INLINE_ROOT: u32 = 0;
pub(super) const UNIFORM_ROOT: u32 = 1;
pub(super) const IMAGE_ROOT: u32 = 2;
pub(super) const SAMPLER_ROOT: u32 = 3;

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
    textured: bool,
) -> Result<NativeObject<ID3D12RootSignature>, D3D12RuntimeError> {
    let ranges = [
        D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 1,
            BaseShaderRegister: 0,
            RegisterSpace: 1,
            OffsetInDescriptorsFromTableStart: 0,
        },
        D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER,
            NumDescriptors: 1,
            BaseShaderRegister: 1,
            RegisterSpace: 1,
            OffsetInDescriptorsFromTableStart: 0,
        },
    ];
    // Generated DXIL reflection puts the inline block at cb0 for mesh and cb1
    // for textured, whose cb0 is the logical Uniform. Root cost is 20 or 24
    // DWORDs, within the API's 64-DWORD limit, with no overlapping registers.
    let parameters = [
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_32BIT_CONSTANTS,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Constants: D3D12_ROOT_CONSTANTS {
                    ShaderRegister: u32::from(textured),
                    RegisterSpace: 0,
                    Num32BitValues: 20,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_VERTEX,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Descriptor: D3D12_ROOT_DESCRIPTOR {
                    ShaderRegister: 0,
                    RegisterSpace: 0,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_VERTEX,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &ranges[0],
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        },
        D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &ranges[1],
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        },
    ];
    let desc = D3D12_ROOT_SIGNATURE_DESC {
        NumParameters: if textured { 4 } else { 1 },
        pParameters: parameters.as_ptr(),
        Flags: D3D12_ROOT_SIGNATURE_FLAG_ALLOW_INPUT_ASSEMBLER_INPUT_LAYOUT,
        ..Default::default()
    };
    let mut blob = None;
    let mut errors = None;
    // SAFETY: Tagged parameters agree with the reviewed DXIL ABI; all nested
    // pointers refer into fixed stack arrays that outlive synchronous encoding.
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
        !self.clear && self.depth == depth && self.managed.native_id() == managed.native_id()
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
        if managed.is_textured() {
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
        require_format(
            owner,
            DXGI_FORMAT_R32G32B32_FLOAT,
            D3D12_FORMAT_SUPPORT1_IA_VERTEX_BUFFER,
        )?;
        require_format(
            owner,
            DXGI_FORMAT_R32G32B32A32_FLOAT,
            D3D12_FORMAT_SUPPORT1_IA_VERTEX_BUFFER,
        )?;
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
            require_format(owner, *destination, D3D12_FORMAT_SUPPORT1_RENDER_TARGET)?;
        }
        let signature = signature(owner, managed.is_textured())?;
        let attributes = [
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: windows_core::s!("TEXCOORD"),
                SemanticIndex: 0,
                Format: DXGI_FORMAT_R32G32B32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 0,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
            D3D12_INPUT_ELEMENT_DESC {
                SemanticName: windows_core::s!("TEXCOORD"),
                SemanticIndex: 1,
                Format: DXGI_FORMAT_R32G32B32A32_FLOAT,
                InputSlot: 0,
                AlignedByteOffset: 12,
                InputSlotClass: D3D12_INPUT_CLASSIFICATION_PER_VERTEX_DATA,
                InstanceDataStepRate: 0,
            },
        ];
        let target_blend = D3D12_RENDER_TARGET_BLEND_DESC {
            BlendEnable: false.into(),
            LogicOpEnable: false.into(),
            SrcBlend: D3D12_BLEND_ONE,
            DestBlend: D3D12_BLEND_ZERO,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: D3D12_BLEND_ZERO,
            BlendOpAlpha: D3D12_BLEND_OP_ADD,
            LogicOp: D3D12_LOGIC_OP_NOOP,
            RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL as u8,
        };
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
                DepthEnable: depth.into(),
                DepthWriteMask: if depth && (clear || !managed.is_prepassed()) {
                    D3D12_DEPTH_WRITE_MASK_ALL
                } else {
                    D3D12_DEPTH_WRITE_MASK_ZERO
                },
                DepthFunc: if clear {
                    D3D12_COMPARISON_FUNC_ALWAYS
                } else if managed.is_prepassed() {
                    D3D12_COMPARISON_FUNC_LESS_EQUAL
                } else {
                    D3D12_COMPARISON_FUNC_LESS
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
                if program.native_id() != 0 {
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

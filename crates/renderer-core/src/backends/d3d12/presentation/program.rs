//! Immutable fullscreen linear-to-sRGB output program.

use std::mem::ManuallyDrop;

use super::super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    frame::{NativeObject, object},
};

pub(super) struct Program {
    pub(super) pipeline: NativeObject<ID3D12PipelineState>,
    pub(super) signature: NativeObject<ID3D12RootSignature>,
}

impl Program {
    pub(super) fn new(owner: &DeviceOwner) -> Result<Self, D3D12RuntimeError> {
        let range = D3D12_DESCRIPTOR_RANGE {
            RangeType: D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
            NumDescriptors: 1,
            BaseShaderRegister: 0,
            RegisterSpace: 0,
            OffsetInDescriptorsFromTableStart: 0,
        };
        let parameter = D3D12_ROOT_PARAMETER {
            ParameterType: D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                    NumDescriptorRanges: 1,
                    pDescriptorRanges: &range,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_PIXEL,
        };
        let desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: 1,
            pParameters: &parameter,
            ..Default::default()
        };
        let mut blob = None;
        let mut errors = None;
        // SAFETY: The stable tagged descriptor table agrees with the reviewed
        // composite DXIL's only live resource t0/space0, visible in the PS.
        unsafe {
            D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, &mut errors)
        }
        .ok()
        .map_err(|error| owner.native("D3D12SerializeRootSignature(presentation)", error))?;
        let blob = blob
            .ok_or_else(|| D3D12RuntimeError::invalid("presentation signature returned no blob"))?;
        // SAFETY: Successful serialization owns the nonempty byte buffer until
        // the synchronous CreateRootSignature below has consumed it.
        let bytes = unsafe {
            let pointer = blob.GetBufferPointer();
            let len = blob.GetBufferSize();
            if pointer.is_null() || len == 0 || len > isize::MAX as usize {
                return Err(D3D12RuntimeError::invalid(
                    "invalid presentation signature blob",
                ));
            }
            std::slice::from_raw_parts(pointer.cast::<u8>(), len)
        };
        let signature: NativeObject<ID3D12RootSignature> =
            object(owner, "CreateRootSignature(presentation)", || {
                // SAFETY: The validated serialization and Device remain live.
                unsafe { owner.raw.CreateRootSignature(0, bytes) }
            })?;
        let vertex = include_bytes!("../../../../shaders/dxil/composite.vert.dxil");
        let fragment = include_bytes!("../../../../shaders/dxil/composite.frag.dxil");
        let stencil = D3D12_DEPTH_STENCILOP_DESC {
            StencilFailOp: D3D12_STENCIL_OP_KEEP,
            StencilDepthFailOp: D3D12_STENCIL_OP_KEEP,
            StencilPassOp: D3D12_STENCIL_OP_KEEP,
            StencilFunc: D3D12_COMPARISON_FUNC_ALWAYS,
        };
        let blend = D3D12_RENDER_TARGET_BLEND_DESC {
            SrcBlend: D3D12_BLEND_ONE,
            DestBlend: D3D12_BLEND_ZERO,
            BlendOp: D3D12_BLEND_OP_ADD,
            SrcBlendAlpha: D3D12_BLEND_ONE,
            DestBlendAlpha: D3D12_BLEND_ZERO,
            BlendOpAlpha: D3D12_BLEND_OP_ADD,
            LogicOp: D3D12_LOGIC_OP_NOOP,
            RenderTargetWriteMask: D3D12_COLOR_WRITE_ENABLE_ALL as u8,
            ..Default::default()
        };
        let mut desc = D3D12_GRAPHICS_PIPELINE_STATE_DESC {
            pRootSignature: ManuallyDrop::new(Some(signature.clone())),
            VS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: vertex.as_ptr().cast(),
                BytecodeLength: vertex.len(),
            },
            PS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: fragment.as_ptr().cast(),
                BytecodeLength: fragment.len(),
            },
            BlendState: D3D12_BLEND_DESC {
                RenderTarget: [blend; 8],
                ..Default::default()
            },
            SampleMask: u32::MAX,
            RasterizerState: D3D12_RASTERIZER_DESC {
                FillMode: D3D12_FILL_MODE_SOLID,
                CullMode: D3D12_CULL_MODE_NONE,
                DepthClipEnable: true.into(),
                ConservativeRaster: D3D12_CONSERVATIVE_RASTERIZATION_MODE_OFF,
                ..Default::default()
            },
            DepthStencilState: D3D12_DEPTH_STENCIL_DESC {
                DepthWriteMask: D3D12_DEPTH_WRITE_MASK_ZERO,
                DepthFunc: D3D12_COMPARISON_FUNC_ALWAYS,
                FrontFace: stencil,
                BackFace: stencil,
                ..Default::default()
            },
            PrimitiveTopologyType: D3D12_PRIMITIVE_TOPOLOGY_TYPE_TRIANGLE,
            NumRenderTargets: 1,
            RTVFormats: [
                DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
                DXGI_FORMAT_UNKNOWN,
                DXGI_FORMAT_UNKNOWN,
                DXGI_FORMAT_UNKNOWN,
                DXGI_FORMAT_UNKNOWN,
                DXGI_FORMAT_UNKNOWN,
                DXGI_FORMAT_UNKNOWN,
                DXGI_FORMAT_UNKNOWN,
            ],
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            ..Default::default()
        };
        let pipeline = object(owner, "CreateGraphicsPipelineState(presentation)", || {
            // SAFETY: The fixed SRGB RTV, reviewed DXIL and root layout agree;
            // no IA input, depth, blending or sampler is used by this program.
            unsafe { owner.raw.CreateGraphicsPipelineState(&desc) }
        });
        // SAFETY: Balance the temporary COM clone in the generated no-Drop field
        // after the synchronous creation on both success and failure.
        unsafe { ManuallyDrop::drop(&mut desc.pRootSignature) };
        Ok(Self {
            pipeline: pipeline?,
            signature,
        })
    }
}

//! Prepared native Compute pipelines and managed shader root signatures.

use std::{mem::ManuallyDrop, sync::Arc};

use super::frame::{NativeObject, object};
use super::{D3D12RuntimeError, bindings::*, device::DeviceOwner};
use crate::program::ManagedProgram;

mod descriptors;
use descriptors::root_parameters;
#[cfg(test)]
mod tests;

pub(super) struct ComputePipeline {
    pipeline: NativeObject<ID3D12PipelineState>,
    signature: NativeObject<ID3D12RootSignature>,
    managed: ManagedProgram,
    _owner: Arc<DeviceOwner>,
}
// SAFETY: PSOs and root signatures are free-threaded D3D12 objects. This owner
// exposes immutable references only and retains its free-threaded Device.
unsafe impl Send for ComputePipeline {}
// SAFETY: The same immutable native-object ownership permits shared access.
unsafe impl Sync for ComputePipeline {}

impl ComputePipeline {
    pub(super) fn matches(&self, managed: &ManagedProgram) -> bool {
        self.managed.same_asset(managed)
    }
    pub(super) fn pipeline(&self) -> &ID3D12PipelineState {
        &self.pipeline
    }
    pub(super) fn signature(&self) -> &ID3D12RootSignature {
        &self.signature
    }
    pub(super) fn new(
        owner: &Arc<DeviceOwner>,
        managed: &ManagedProgram,
        #[cfg(test)] fail_after_signature: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        owner.ensure_active()?;
        if owner.info.shader_model < D3D_SHADER_MODEL_6_0 as u32 {
            return Err(D3D12RuntimeError::unsupported(
                "managed compute Program requires Shader Model 6.0",
            ));
        }
        super::compute::validate_program(owner, managed)?;
        let parameters = root_parameters(managed.bindings())?;
        let desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: parameters.parameters.len() as u32,
            pParameters: parameters.parameters.as_ptr(),
            ..Default::default()
        };
        let mut blob = None;
        let mut errors = None;
        // SAFETY: The tagged root descriptors match the managed reflected
        // manifest ABI. Local arrays outlive serialization.
        unsafe {
            D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, &mut errors)
        }
        .ok()
        .map_err(|error| owner.native("D3D12SerializeRootSignature", error))?;
        let blob = blob.ok_or_else(|| {
            D3D12RuntimeError::invalid("root signature serialization returned no blob")
        })?;
        // SAFETY: A successful owned serialization blob supplies exactly this
        // readable buffer until the synchronous CreateRootSignature returns.
        let bytes = unsafe {
            let pointer = blob.GetBufferPointer();
            let size = blob.GetBufferSize();
            if pointer.is_null() || size == 0 || size > isize::MAX as usize {
                return Err(D3D12RuntimeError::invalid(
                    "invalid serialized root signature blob",
                ));
            }
            std::slice::from_raw_parts(pointer.cast::<u8>(), size)
        };
        // SAFETY: The serialized v1.0 signature is valid on the retained Device.
        let signature: NativeObject<ID3D12RootSignature> =
            object(owner, "ID3D12Device::CreateRootSignature", || unsafe {
                owner.raw.CreateRootSignature(0, bytes)
            })?;
        #[cfg(test)]
        if fail_after_signature {
            return Err(owner.native(
                "injected compute PSO creation after root signature",
                windows_core::Error::from_hresult(windows_core::HRESULT(0x8007_000eu32 as i32)),
            ));
        }
        let shader = managed.dxil();
        let mut pipeline_desc = D3D12_COMPUTE_PIPELINE_STATE_DESC {
            pRootSignature: ManuallyDrop::new(Some(signature.clone())),
            CS: D3D12_SHADER_BYTECODE {
                pShaderBytecode: shader.as_ptr().cast(),
                BytecodeLength: shader.len(),
            },
            ..Default::default()
        };
        // SAFETY: The registry supplies validated SM6.0 DXIL and its matching
        // signature. Both bytecode and COM reference outlive this native call.
        let pipeline = object(
            owner,
            "ID3D12Device::CreateComputePipelineState",
            || unsafe { owner.raw.CreateComputePipelineState(&pipeline_desc) },
        );
        // SAFETY: The generated field suppresses Drop; balance our clone after
        // the synchronous call, including its failure path.
        unsafe { ManuallyDrop::drop(&mut pipeline_desc.pRootSignature) };
        let pipeline = pipeline?;
        Ok(Self {
            pipeline,
            signature,
            managed: *managed,
            _owner: Arc::clone(owner),
        })
    }
}

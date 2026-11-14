//! Immutable managed compute programs and uniform bindings.
//!
//! The shader registry establishes the ABI and fixed 16-byte access bounds.
//! Creation checks Device support, owner and logical ranges once. Root
//! descriptors expose no bounds, so no arbitrary binaries or addresses enter.

use std::{fmt, mem::ManuallyDrop, sync::Arc};

use crate::program::{ManagedProgram, UniformBindingDesc, UniformLayout, validate_uniform_binding};

use super::resources::{DeviceBuffer, MAX_BUFFER_BYTES};
use super::{D3D12Runtime, D3D12RuntimeError, bindings::*, device::DeviceOwner};

const UNIFORM_ALIGNMENT: u64 = D3D12_CONSTANT_BUFFER_DATA_PLACEMENT_ALIGNMENT as u64;
const MAX_UNIFORM_RANGE: u64 = D3D12_REQ_CONSTANT_BUFFER_ELEMENT_COUNT as u64 * 16;

struct ProgramOwner {
    // Native references are released before their retained Device owner.
    _pipeline: ID3D12PipelineState,
    _signature: ID3D12RootSignature,
    managed: ManagedProgram,
    owner: Arc<DeviceOwner>,
}

// SAFETY: PSOs and root signatures are free-threaded D3D12 objects. This owner
// exposes immutable references only and retains its free-threaded Device.
unsafe impl Send for ProgramOwner {}
// SAFETY: The same immutable native-object ownership permits shared access.
unsafe impl Sync for ProgramOwner {}

/// One immutable managed Program version, retained independently of its runtime.
#[derive(Clone)]
pub struct D3D12Program(Arc<ProgramOwner>);

struct UniformOwner {
    _buffer: DeviceBuffer,
    size: u64,
    owner: Arc<DeviceOwner>,
}

// SAFETY: D3D12 resources are free-threaded. Upload mapping finishes before
// publication; no mutation or native handle escapes this immutable owner.
unsafe impl Send for UniformOwner {}
// SAFETY: Shared uses read immutable bytes, and each binding retains the owner
// through GPU completion. Native release occurs only after the final reference.
unsafe impl Sync for UniformOwner {}

/// Immutable uniform bytes. Creating another buffer never updates existing uses.
#[derive(Clone)]
pub struct D3D12UniformBuffer(Arc<UniformOwner>);

struct BindingOwner {
    _program: D3D12Program,
    _buffer: D3D12UniformBuffer,
    desc: UniformBindingDesc,
}

/// An immutable range retaining its actual Program and uniform buffer versions.
#[derive(Clone)]
pub struct D3D12UniformBinding(Arc<BindingOwner>);

impl D3D12Program {
    /// The reflected uniform interface of this immutable Program version.
    pub fn uniform_layout(&self) -> UniformLayout {
        self.0.managed.uniform_layout()
    }
}

impl D3D12UniformBuffer {
    /// Logical byte length, excluding native allocation padding.
    pub fn len(&self) -> u64 {
        self.0.size
    }

    /// Uniform creation rejects empty input, so this always returns false.
    pub fn is_empty(&self) -> bool {
        false
    }
}

impl D3D12UniformBinding {
    /// The immutable slot and logical byte range established at creation.
    pub fn desc(&self) -> UniformBindingDesc {
        self.0.desc
    }
}

impl fmt::Debug for D3D12Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12Program")
            .field("managed", &self.0.managed)
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for D3D12UniformBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12UniformBuffer")
            .field("size", &self.0.size)
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for D3D12UniformBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12UniformBinding")
            .field("range", &self.0.desc)
            .finish_non_exhaustive()
    }
}

fn uniform_allocation_size(size: u64) -> Result<u64, D3D12RuntimeError> {
    if size == 0 || size > MAX_BUFFER_BYTES {
        return Err(D3D12RuntimeError::invalid(
            "uniform byte length is outside the supported buffer range",
        ));
    }
    Ok(size.div_ceil(UNIFORM_ALIGNMENT) * UNIFORM_ALIGNMENT)
}

fn require_same_owner<T>(
    runtime: &Arc<T>,
    program: &Arc<T>,
    buffer: &Arc<T>,
) -> Result<(), D3D12RuntimeError> {
    if !Arc::ptr_eq(runtime, program) || !Arc::ptr_eq(runtime, buffer) {
        return Err(D3D12RuntimeError::wrong_owner(
            "uniform binding resources belong to another D3D12 Device",
        ));
    }
    Ok(())
}

impl D3D12Runtime {
    /// Creates a native immutable Program from the trusted managed registry.
    pub fn create_program(
        &self,
        managed: &ManagedProgram,
    ) -> Result<D3D12Program, D3D12RuntimeError> {
        self.create_program_inner(
            managed,
            #[cfg(test)]
            false,
        )
    }

    fn create_program_inner(
        &self,
        managed: &ManagedProgram,
        #[cfg(test)] fail_after_signature: bool,
    ) -> Result<D3D12Program, D3D12RuntimeError> {
        self.device.ensure_active()?;
        if self.device.info.shader_model < D3D_SHADER_MODEL_6_0 as u32 {
            return Err(D3D12RuntimeError::unsupported(
                "managed compute Program requires Shader Model 6.0",
            ));
        }
        let uniform = managed.uniform_layout();
        let parameters = [
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_CBV,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR {
                        ShaderRegister: uniform.binding,
                        RegisterSpace: uniform.group,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
            D3D12_ROOT_PARAMETER {
                ParameterType: D3D12_ROOT_PARAMETER_TYPE_UAV,
                Anonymous: D3D12_ROOT_PARAMETER_0 {
                    Descriptor: D3D12_ROOT_DESCRIPTOR {
                        ShaderRegister: 0,
                        RegisterSpace: 1,
                    },
                },
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            },
        ];
        let desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: parameters.len() as u32,
            pParameters: parameters.as_ptr(),
            ..Default::default()
        };
        let mut blob = None;
        let mut errors = None;
        // SAFETY: The two tagged root descriptors match the managed reflected
        // b0/space0 and u0/space1 ABI. Local arrays outlive serialization.
        unsafe {
            D3D12SerializeRootSignature(&desc, D3D_ROOT_SIGNATURE_VERSION_1, &mut blob, &mut errors)
        }
        .ok()
        .map_err(|error| self.device.native("D3D12SerializeRootSignature", error))?;
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
        let signature: ID3D12RootSignature =
            unsafe { self.device.raw.CreateRootSignature(0, bytes) }.map_err(|error| {
                self.device
                    .native("ID3D12Device::CreateRootSignature", error)
            })?;
        #[cfg(test)]
        if fail_after_signature {
            return Err(self.device.native(
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
        let pipeline = unsafe { self.device.raw.CreateComputePipelineState(&pipeline_desc) };
        // SAFETY: The generated field suppresses Drop; balance our clone after
        // the synchronous call, including its failure path.
        unsafe { ManuallyDrop::drop(&mut pipeline_desc.pRootSignature) };
        let pipeline = pipeline.map_err(|error| {
            self.device
                .native("ID3D12Device::CreateComputePipelineState", error)
        })?;
        Ok(D3D12Program(Arc::new(ProgramOwner {
            _pipeline: pipeline,
            _signature: signature,
            managed: *managed,
            owner: Arc::clone(&self.device),
        })))
    }

    /// Copies immutable bytes once into an independently owned uniform buffer.
    /// Native padding does not enlarge the logical range accepted by bindings.
    pub fn create_uniform_buffer(
        &self,
        bytes: &[u8],
    ) -> Result<D3D12UniformBuffer, D3D12RuntimeError> {
        self.device.ensure_active()?;
        let size = bytes.len() as u64;
        let allocation_size = uniform_allocation_size(size)?;
        let buffer = DeviceBuffer::new(&self.device, allocation_size, D3D12_HEAP_TYPE_UPLOAD)?;
        // SAFETY: This new upload has at least bytes.len() writable bytes and
        // no GPU use. Its retained owner outlives copying and native release.
        unsafe { buffer.write(&self.device, bytes) }?;
        Ok(D3D12UniformBuffer(Arc::new(UniformOwner {
            _buffer: buffer,
            size,
            owner: Arc::clone(&self.device),
        })))
    }

    /// Checks the logical slot, range, alignment and Device ownership once.
    pub fn create_uniform_binding(
        &self,
        program: &D3D12Program,
        buffer: &D3D12UniformBuffer,
        desc: UniformBindingDesc,
    ) -> Result<D3D12UniformBinding, D3D12RuntimeError> {
        self.device.ensure_active()?;
        require_same_owner(&self.device, &program.0.owner, &buffer.0.owner)?;
        validate_uniform_binding(
            program.0.managed.uniform_layout(),
            buffer.0.size,
            desc,
            UNIFORM_ALIGNMENT,
            MAX_UNIFORM_RANGE,
        )
        .map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
        Ok(D3D12UniformBinding(Arc::new(BindingOwner {
            _program: program.clone(),
            _buffer: buffer.clone(),
            desc,
        })))
    }
}

#[cfg(test)]
impl D3D12UniformBinding {
    pub(super) fn check_owner(&self, owner: &Arc<DeviceOwner>) -> Result<(), D3D12RuntimeError> {
        require_same_owner(owner, &self.0._program.0.owner, &self.0._buffer.0.owner)
    }

    pub(super) fn record_fixture(
        &self,
        list: &ID3D12GraphicsCommandList,
    ) -> Result<(DeviceBuffer, DeviceBuffer), D3D12RuntimeError> {
        let binding = &self.0;
        let program = &binding._program.0;
        let output = DeviceBuffer::storage(&program.owner, 16)?;
        let readback = DeviceBuffer::new(&program.owner, 16, D3D12_HEAP_TYPE_READBACK)?;
        let mut state = D3D12_RESOURCE_STATE_COMMON;
        super::graph::transition(
            list,
            output.raw(),
            &mut state,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        );
        // SAFETY: Binding creation established the immutable slot/range/owner
        // contract. The managed single invocation copies exactly 16 bytes. The
        // private UAV has exactly that size and no counter; work retains both
        // resources, Program and binding until cancellation or Fence completion.
        unsafe {
            list.SetPipelineState(&program._pipeline);
            list.SetComputeRootSignature(&program._signature);
            list.SetComputeRootConstantBufferView(
                0,
                D3D12_GPU_VIRTUAL_ADDRESS(
                    binding._buffer.0._buffer.raw().GetGPUVirtualAddress().0 + binding.desc.offset,
                ),
            );
            list.SetComputeRootUnorderedAccessView(1, output.raw().GetGPUVirtualAddress());
            list.Dispatch(1, 1, 1);
        }
        // A UAV -> copy-source transition makes the dispatch writes visible.
        super::graph::transition(
            list,
            output.raw(),
            &mut state,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        // SAFETY: The transition orders the initialized output before its exact
        // 16-byte copy. The new readback starts in COPY_DEST and is retained.
        unsafe { list.CopyBufferRegion(readback.raw(), 0, output.raw(), 0, 16) };
        Ok((output, readback))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{D3D12RuntimeConfig, D3D12RuntimeErrorKind, D3D12ValidationMode};
    use super::*;
    use crate::graph::PassHandle;
    use std::time::Duration;

    fn binding_desc() -> UniformBindingDesc {
        UniformBindingDesc {
            group: 0,
            binding: 0,
            offset: 256,
            size: 16,
        }
    }

    #[test]
    fn uniform_padding_does_not_change_logical_size_and_rejects_unsupported_lengths() {
        assert_eq!(uniform_allocation_size(1).unwrap(), 256);
        assert_eq!(uniform_allocation_size(256).unwrap(), 256);
        assert_eq!(uniform_allocation_size(272).unwrap(), 512);
        assert_eq!(
            uniform_allocation_size(MAX_BUFFER_BYTES).unwrap(),
            MAX_BUFFER_BYTES
        );
        for size in [0, MAX_BUFFER_BYTES + 1, u64::MAX] {
            assert_eq!(
                uniform_allocation_size(size).unwrap_err().kind(),
                D3D12RuntimeErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn binding_owner_identity_is_not_replaced_by_equal_metadata() {
        let owner = Arc::new(());
        let other = Arc::new(());
        assert!(require_same_owner(&owner, &owner, &owner).is_ok());
        for (program, buffer) in [(&other, &owner), (&owner, &other), (&other, &other)] {
            assert_eq!(
                require_same_owner(&owner, program, buffer)
                    .unwrap_err()
                    .kind(),
                D3D12RuntimeErrorKind::WrongOwner
            );
        }
    }

    fn uniform_fixture(strict: bool) {
        let runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
        })
        .expect("real D3D12 hardware and requested diagnostics");
        let mut runtime = runtime;
        let monitor = runtime.validation_monitor();
        let device = Arc::downgrade(&runtime.device);
        let managed = ManagedProgram::uniform_readback();
        let owners_before_failure = Arc::strong_count(&runtime.device);
        assert_eq!(
            runtime
                .create_program_inner(&managed, true)
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::Native
        );
        assert_eq!(Arc::strong_count(&runtime.device), owners_before_failure);
        let program = runtime.create_program(&managed).unwrap();
        let program_owner = Arc::downgrade(&program.0);
        let [first, second] = crate::program::UNIFORM_TEST_DATA;
        let make_bytes = |values: [u8; 16]| {
            let mut bytes = vec![0; 256];
            bytes.extend_from_slice(&values);
            bytes
        };
        let buffer = runtime.create_uniform_buffer(&make_bytes(first)).unwrap();
        let updated = runtime.create_uniform_buffer(&make_bytes(second)).unwrap();
        assert_eq!(program.uniform_layout(), managed.uniform_layout());
        assert_eq!(buffer.len(), 272);
        assert!(!buffer.is_empty());
        let old = runtime
            .create_uniform_binding(&program, &buffer, binding_desc())
            .unwrap();
        let new = runtime
            .create_uniform_binding(&program, &updated, binding_desc())
            .unwrap();
        for desc in [
            UniformBindingDesc {
                group: 1,
                ..binding_desc()
            },
            UniformBindingDesc {
                binding: 1,
                ..binding_desc()
            },
            UniformBindingDesc {
                offset: 1,
                ..binding_desc()
            },
            UniformBindingDesc {
                size: 15,
                ..binding_desc()
            },
            // Native padding reaches 512, but logical bytes end at 272.
            UniformBindingDesc {
                size: 17,
                ..binding_desc()
            },
        ] {
            assert_eq!(
                runtime
                    .create_uniform_binding(&program, &buffer, desc)
                    .unwrap_err()
                    .kind(),
                D3D12RuntimeErrorKind::InvalidInput
            );
        }
        assert_eq!(old.desc(), binding_desc());
        let baseline = Arc::strong_count(&old.0);
        let canceled = runtime.prepare_uniform_fixture(&old).unwrap();
        assert_eq!(Arc::strong_count(&old.0), baseline + 1);
        drop(canceled);
        assert_eq!(Arc::strong_count(&old.0), baseline);
        let prepared_old = runtime.prepare_uniform_fixture(&old).unwrap();
        let prepared_new = runtime.prepare_uniform_fixture(&new).unwrap();
        drop((old, new, buffer, updated, program));
        assert!(
            program_owner.upgrade().is_some(),
            "recordings retain the actual Program version"
        );
        let mut old_pending = runtime.submit_prepared(prepared_old).unwrap();
        let mut new_pending = runtime.submit_prepared(prepared_new).unwrap();
        runtime.shutdown().unwrap();
        assert!(device.upgrade().is_some(), "pending work retains Device");
        let mut old_result = old_pending
            .wait(Duration::from_secs(30))
            .unwrap()
            .expect("old uniform completion");
        let mut new_result = new_pending
            .wait(Duration::from_secs(30))
            .unwrap()
            .expect("new uniform completion");
        let pass = PassHandle { graph: 0, index: 0 };
        let old_bytes = old_result.readback(pass).unwrap();
        let new_bytes = new_result.readback(pass).unwrap();
        assert_eq!(old_bytes, first);
        assert_eq!(new_bytes, second);
        drop((old_result, new_result, old_pending, new_pending));
        assert!(program_owner.upgrade().is_none());
        assert!(device.upgrade().is_none());
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
        println!(
            "D3D12_UNIFORM_REPORT strict={strict} old={old_bytes:?} new={new_bytes:?} validation={report:?}"
        );
    }

    #[cfg(feature = "d3d12-diagnostics")]
    #[test]
    #[ignore = "requires D3D12 hardware, Debug Layer and GPU-Based Validation"]
    fn d3d12_uniform_program_strict() {
        uniform_fixture(true);
    }

    #[cfg(not(any(
        feature = "development-validation",
        feature = "d3d12-diagnostics",
        feature = "vulkan-diagnostics",
        feature = "graph-diagnostics"
    )))]
    #[test]
    #[ignore = "requires optimized D3D12 hardware execution without diagnostics"]
    fn d3d12_uniform_program_shipping() {
        if cfg!(debug_assertions) {
            panic!("shipping content acceptance requires --release");
        }
        uniform_fixture(false);
    }
}

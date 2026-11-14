//! Managed compute programs, buffer ownership and immutable bindings.
//!
//! Layout slots come from the reviewed shader manifest. Creation establishes
//! ABI, usage, owner and logical range guarantees before native recording.

#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fmt, mem::ManuallyDrop, sync::Arc};

use super::resources::{DeviceBuffer, MAX_BUFFER_BYTES};
use super::{D3D12Runtime, D3D12RuntimeError, bindings::*, device::DeviceOwner};
use crate::program::{
    BufferBinding, BufferBindingDesc, BufferBindingLayout, BufferBindingType, BufferDescriptor,
    BufferUsage, ManagedProgram, validate_binding_slots, validate_buffer_binding,
};

const UNIFORM_ALIGNMENT: u64 = D3D12_CONSTANT_BUFFER_DATA_PLACEMENT_ALIGNMENT as u64;
const STORAGE_ALIGNMENT: u64 = D3D12_RAW_UAV_SRV_BYTE_ALIGNMENT as u64;
const MAX_UNIFORM_RANGE: u64 = D3D12_REQ_CONSTANT_BUFFER_ELEMENT_COUNT as u64 * 16;

struct ProgramOwner {
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

/// An immutable managed Program version, independent of its runtime handle.
#[derive(Clone)]
pub struct D3D12Program(Arc<ProgramOwner>);

struct BufferOwner {
    _native: DeviceBuffer,
    size: u64,
    usage: BufferUsage,
    #[cfg(test)]
    in_use: AtomicBool,
    owner: Arc<DeviceOwner>,
}
// SAFETY: D3D12 resources are free-threaded. Uniform upload mapping completes
// before publication; no CPU mapping or native handle escapes. Private storage
// execution takes an exclusive lease through cancellation or proven completion.
unsafe impl Send for BufferOwner {}
// SAFETY: Uniform reads are immutable. Storage has no public execution API and
// its private test executor serializes all accesses with the retained lease.
unsafe impl Sync for BufferOwner {}

/// A buffer with immutable description and ownership. Storage GPU contents are
/// mutable; this handle alone does not establish initialized content or ordering.
#[derive(Clone)]
pub struct D3D12Buffer(Arc<BufferOwner>);

struct BindingOwner {
    _program: D3D12Program,
    _buffers: Box<[D3D12Buffer]>,
    descs: Box<[BufferBindingDesc]>,
}
/// An immutable, manifest-ordered binding version retaining its actual Program
/// and every buffer. Creating another version never updates existing bindings.
#[derive(Clone)]
pub struct D3D12Bindings(Arc<BindingOwner>);

impl D3D12Program {
    /// Reflected buffer slots in their canonical native root-parameter order.
    pub fn layout(&self) -> &'static [BufferBindingLayout] {
        self.0.managed.bindings()
    }
}
impl D3D12Buffer {
    pub fn usage(&self) -> BufferUsage {
        self.0.usage
    }
    /// Logical byte length, excluding native uniform allocation padding.
    pub fn len(&self) -> u64 {
        self.0.size
    }
    /// Buffer creation rejects empty resources.
    pub fn is_empty(&self) -> bool {
        false
    }
}
impl D3D12Bindings {
    /// Logical ranges in the same canonical order as `Program::layout`.
    pub fn descs(&self) -> &[BufferBindingDesc] {
        &self.0.descs
    }
}
impl fmt::Debug for D3D12Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12Program")
            .field("managed", &self.0.managed)
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for D3D12Buffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12Buffer")
            .field("size", &self.len())
            .field("usage", &self.usage())
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for D3D12Bindings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12Bindings")
            .field("ranges", &self.0.descs)
            .finish_non_exhaustive()
    }
}

fn allocation_size(size: u64, usage: BufferUsage) -> Result<u64, D3D12RuntimeError> {
    if size == 0 || size > MAX_BUFFER_BYTES {
        return Err(D3D12RuntimeError::invalid(
            "managed byte length is outside the supported buffer range",
        ));
    }
    Ok(match usage {
        BufferUsage::Uniform => size.div_ceil(UNIFORM_ALIGNMENT) * UNIFORM_ALIGNMENT,
        BufferUsage::Storage => size,
    })
}
fn require_same_owner<T>(owner: &Arc<T>, resource: &Arc<T>) -> Result<(), D3D12RuntimeError> {
    if !Arc::ptr_eq(owner, resource) {
        return Err(D3D12RuntimeError::wrong_owner(
            "managed binding belongs to another D3D12 Device",
        ));
    }
    Ok(())
}

fn root_parameters(
    layout: &[BufferBindingLayout],
) -> Result<Vec<D3D12_ROOT_PARAMETER>, D3D12RuntimeError> {
    // Each root descriptor costs two DWORDs, within the v1.0 64-DWORD limit.
    if layout.len() > 32 {
        return Err(D3D12RuntimeError::unsupported(
            "managed buffer layout exceeds D3D12 root signature capacity",
        ));
    }
    Ok(layout
        .iter()
        .map(|slot| D3D12_ROOT_PARAMETER {
            ParameterType: match slot.kind {
                BufferBindingType::Uniform => D3D12_ROOT_PARAMETER_TYPE_CBV,
                BufferBindingType::StorageWrite => D3D12_ROOT_PARAMETER_TYPE_UAV,
            },
            Anonymous: D3D12_ROOT_PARAMETER_0 {
                Descriptor: D3D12_ROOT_DESCRIPTOR {
                    ShaderRegister: slot.binding,
                    RegisterSpace: slot.group,
                },
            },
            ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
        })
        .collect())
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
        let parameters = root_parameters(managed.bindings())?;
        let desc = D3D12_ROOT_SIGNATURE_DESC {
            NumParameters: parameters.len() as u32,
            pParameters: parameters.as_ptr(),
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

    /// Creates immutable uploaded uniform bytes or an uninitialized storage
    /// resource. No storage CPU mapping or public GPU execution is exposed.
    pub fn create_managed_buffer(
        &self,
        descriptor: BufferDescriptor<'_>,
    ) -> Result<D3D12Buffer, D3D12RuntimeError> {
        self.device.ensure_active()?;
        let (usage, size, initial) = match descriptor {
            BufferDescriptor::Uniform { bytes } => {
                (BufferUsage::Uniform, bytes.len() as u64, Some(bytes))
            }
            BufferDescriptor::Storage { size } => (BufferUsage::Storage, size, None),
        };
        let allocation = allocation_size(size, usage)?;
        let native = match usage {
            BufferUsage::Uniform => {
                DeviceBuffer::new(&self.device, allocation, D3D12_HEAP_TYPE_UPLOAD)?
            }
            BufferUsage::Storage => DeviceBuffer::storage(&self.device, allocation)?,
        };
        if let Some(bytes) = initial {
            // SAFETY: A newly owned upload has at least bytes.len() bytes and
            // no GPU use. Mapping completes before this owner is published.
            unsafe { native.write(&self.device, bytes) }?;
        }
        Ok(D3D12Buffer(Arc::new(BufferOwner {
            _native: native,
            size,
            usage,
            #[cfg(test)]
            in_use: AtomicBool::new(false),
            owner: Arc::clone(&self.device),
        })))
    }

    /// Validates every reflected slot and canonicalizes caller declaration order.
    /// The immutable result retains both its Program and each resource version.
    pub fn create_bindings(
        &self,
        program: &D3D12Program,
        bindings: &[BufferBinding<'_, D3D12Buffer>],
    ) -> Result<D3D12Bindings, D3D12RuntimeError> {
        self.device.ensure_active()?;
        require_same_owner(&self.device, &program.0.owner)?;
        let descs = bindings
            .iter()
            .map(|binding| binding.desc)
            .collect::<Vec<_>>();
        validate_binding_slots(program.layout(), &descs)
            .map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
        let mut buffers = Vec::with_capacity(bindings.len());
        let mut canonical_descs = Vec::with_capacity(bindings.len());
        for &layout in program.layout() {
            // Complete, unique slot membership was established above.
            let binding = bindings
                .iter()
                .find(|binding| {
                    binding.desc.group == layout.group && binding.desc.binding == layout.binding
                })
                .unwrap();
            require_same_owner(&self.device, &binding.buffer.0.owner)?;
            let (alignment, max_range) = match layout.kind {
                BufferBindingType::Uniform => (UNIFORM_ALIGNMENT, MAX_UNIFORM_RANGE),
                BufferBindingType::StorageWrite => (STORAGE_ALIGNMENT, MAX_BUFFER_BYTES),
            };
            validate_buffer_binding(
                layout,
                binding.buffer.usage(),
                binding.buffer.len(),
                binding.desc,
                alignment,
                max_range,
            )
            .map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
            buffers.push(binding.buffer.clone());
            canonical_descs.push(binding.desc);
        }
        Ok(D3D12Bindings(Arc::new(BindingOwner {
            _program: program.clone(),
            _buffers: buffers.into_boxed_slice(),
            descs: canonical_descs.into_boxed_slice(),
        })))
    }
}

/// Test-only exclusive storage use. The execution owner retains this through
/// cancel or proven completion; unproven native failure retains it indefinitely.
#[cfg(test)]
pub(super) struct StorageLease(D3D12Buffer);
#[cfg(test)]
impl StorageLease {
    fn acquire(buffer: &D3D12Buffer) -> Result<Self, D3D12RuntimeError> {
        buffer
            .0
            .in_use
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                D3D12RuntimeError::invalid(
                    "managed storage buffer is already reserved by recorded or pending work",
                )
            })?;
        Ok(Self(buffer.clone()))
    }
}
#[cfg(test)]
impl Drop for StorageLease {
    fn drop(&mut self) {
        self.0.0.in_use.store(false, Ordering::Release);
    }
}

#[cfg(test)]
impl D3D12Bindings {
    pub(super) fn check_owner(&self, owner: &Arc<DeviceOwner>) -> Result<(), D3D12RuntimeError> {
        require_same_owner(owner, &self.0._program.0.owner)
    }

    pub(super) fn record_fixture(
        &self,
        list: &ID3D12GraphicsCommandList,
    ) -> Result<(DeviceBuffer, StorageLease), D3D12RuntimeError> {
        let binding = &self.0;
        let program = &binding._program.0;
        let storage_index = program
            .managed
            .bindings()
            .iter()
            .position(|slot| slot.kind == BufferBindingType::StorageWrite)
            .unwrap();
        let output = &binding._buffers[storage_index];
        let output_desc = binding.descs[storage_index];
        let lease = StorageLease::acquire(output)?;
        let readback = DeviceBuffer::new(&program.owner, 16, D3D12_HEAP_TYPE_READBACK)?;
        let mut state = D3D12_RESOURCE_STATE_COMMON;
        super::graph::transition(
            list,
            output.0._native.raw(),
            &mut state,
            D3D12_RESOURCE_STATE_UNORDERED_ACCESS,
        );
        // SAFETY: Every immutable binding's usage/slot/range/owner was checked
        // once. Root slots and kinds follow the reflected manifest. The reviewed
        // single invocation reads/writes exactly 16 bytes, within these ranges.
        // The exclusive lease and Work retain all references until real retirement.
        unsafe {
            list.SetPipelineState(&program._pipeline);
            list.SetComputeRootSignature(&program._signature);
            for (root, ((slot, buffer), desc)) in program
                .managed
                .bindings()
                .iter()
                .zip(&binding._buffers)
                .zip(&binding.descs)
                .enumerate()
            {
                let address = D3D12_GPU_VIRTUAL_ADDRESS(
                    buffer.0._native.raw().GetGPUVirtualAddress().0 + desc.offset,
                );
                match slot.kind {
                    BufferBindingType::Uniform => {
                        list.SetComputeRootConstantBufferView(root as u32, address)
                    }
                    BufferBindingType::StorageWrite => {
                        list.SetComputeRootUnorderedAccessView(root as u32, address)
                    }
                }
            }
            list.Dispatch(1, 1, 1);
        }
        super::graph::transition(
            list,
            output.0._native.raw(),
            &mut state,
            D3D12_RESOURCE_STATE_COPY_SOURCE,
        );
        // SAFETY: UAV writes precede the copy-source transition. Only the exact
        // 16-byte initialized subrange is copied, never the uninitialized prefix.
        unsafe {
            list.CopyBufferRegion(
                readback.raw(),
                0,
                output.0._native.raw(),
                output_desc.offset,
                16,
            )
        };
        // Idle and canceled work both retain COMMON. Exclusive use prevents a
        // second recording from assuming that state while this work is pending.
        super::graph::transition(
            list,
            output.0._native.raw(),
            &mut state,
            D3D12_RESOURCE_STATE_COMMON,
        );
        Ok((readback, lease))
    }
}

#[cfg(test)]
mod tests {
    use super::super::{D3D12RuntimeConfig, D3D12RuntimeErrorKind, D3D12ValidationMode};
    use super::*;
    use crate::graph::PassHandle;
    use std::time::Duration;

    fn uniform_desc() -> BufferBindingDesc {
        BufferBindingDesc {
            group: 0,
            binding: 0,
            offset: 256,
            size: 16,
        }
    }
    fn storage_desc() -> BufferBindingDesc {
        BufferBindingDesc {
            group: 1,
            binding: 0,
            offset: 256,
            size: 16,
        }
    }
    fn entries<'a>(
        uniform: &'a D3D12Buffer,
        storage: &'a D3D12Buffer,
    ) -> [BufferBinding<'a, D3D12Buffer>; 2] {
        [
            BufferBinding {
                buffer: uniform,
                desc: uniform_desc(),
            },
            BufferBinding {
                buffer: storage,
                desc: storage_desc(),
            },
        ]
    }

    #[test]
    fn allocation_padding_applies_only_to_uniforms_and_never_changes_logical_ranges() {
        assert_eq!(allocation_size(1, BufferUsage::Uniform).unwrap(), 256);
        assert_eq!(allocation_size(256, BufferUsage::Uniform).unwrap(), 256);
        assert_eq!(allocation_size(272, BufferUsage::Uniform).unwrap(), 512);
        assert_eq!(allocation_size(272, BufferUsage::Storage).unwrap(), 272);
        for usage in [BufferUsage::Uniform, BufferUsage::Storage] {
            assert_eq!(
                allocation_size(MAX_BUFFER_BYTES, usage).unwrap(),
                MAX_BUFFER_BYTES
            );
            for size in [0, MAX_BUFFER_BYTES + 1, u64::MAX] {
                assert_eq!(
                    allocation_size(size, usage).unwrap_err().kind(),
                    D3D12RuntimeErrorKind::InvalidInput
                );
            }
        }
    }

    #[test]
    fn binding_owner_identity_is_not_replaced_by_equal_metadata() {
        let owner = Arc::new(());
        assert!(require_same_owner(&owner, &owner).is_ok());
        assert_eq!(
            require_same_owner(&owner, &Arc::new(()))
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::WrongOwner
        );
    }

    #[test]
    fn root_descriptor_order_kinds_and_registers_follow_the_manifest() {
        let layout = [
            BufferBindingLayout {
                group: 5,
                binding: 7,
                kind: BufferBindingType::StorageWrite,
                min_size: 16,
            },
            BufferBindingLayout {
                group: 2,
                binding: 3,
                kind: BufferBindingType::Uniform,
                min_size: 16,
            },
        ];
        let parameters = root_parameters(&layout).unwrap();
        for (index, (slot, parameter)) in layout.iter().zip(&parameters).enumerate() {
            assert_eq!(
                parameter.ParameterType,
                if index == 0 {
                    D3D12_ROOT_PARAMETER_TYPE_UAV
                } else {
                    D3D12_ROOT_PARAMETER_TYPE_CBV
                }
            );
            // SAFETY: root_parameters selected the Descriptor union member for
            // both reflected buffer kinds; this inspects that lowering contract.
            let descriptor = unsafe { parameter.Anonymous.Descriptor };
            assert_eq!(
                (descriptor.RegisterSpace, descriptor.ShaderRegister),
                (slot.group, slot.binding)
            );
        }
        assert_eq!(
            root_parameters(&[layout[0]; 33]).err().unwrap().kind(),
            D3D12RuntimeErrorKind::Unsupported
        );
    }

    fn managed_fixture(strict: bool) {
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
        })
        .expect("real D3D12 hardware and requested diagnostics");
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
        let buffer = runtime
            .create_managed_buffer(BufferDescriptor::Uniform {
                bytes: &make_bytes(first),
            })
            .unwrap();
        let updated = runtime
            .create_managed_buffer(BufferDescriptor::Uniform {
                bytes: &make_bytes(second),
            })
            .unwrap();
        let output = runtime
            .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
            .unwrap();
        let new_output = runtime
            .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
            .unwrap();
        assert_eq!(program.layout(), managed.bindings());
        assert_eq!(buffer.usage(), BufferUsage::Uniform);
        assert_eq!(output.usage(), BufferUsage::Storage);
        assert_eq!((buffer.len(), output.len()), (272, 272));
        assert!(!buffer.is_empty() && !output.is_empty());
        let old = runtime
            .create_bindings(&program, &entries(&buffer, &output))
            .unwrap();
        // Deliberately reverse caller order; the resulting immutable root slots
        // must still follow the manifest rather than declaration order.
        let mut reversed = entries(&updated, &new_output);
        reversed.reverse();
        let new = runtime.create_bindings(&program, &reversed).unwrap();
        assert_eq!(old.descs(), &[uniform_desc(), storage_desc()]);
        assert_eq!(new.descs(), old.descs());
        assert_eq!(
            runtime.create_bindings(&program, &[]).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        assert_eq!(
            runtime
                .create_bindings(
                    &program,
                    &[
                        BufferBinding {
                            buffer: &buffer,
                            desc: uniform_desc()
                        },
                        BufferBinding {
                            buffer: &buffer,
                            desc: uniform_desc()
                        },
                    ]
                )
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        // Valid slots with wrong resource kinds cannot become native descriptors.
        assert_eq!(
            runtime
                .create_bindings(&program, &entries(&output, &buffer))
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        for desc in [
            BufferBindingDesc {
                group: 7,
                ..uniform_desc()
            },
            BufferBindingDesc {
                binding: 7,
                ..uniform_desc()
            },
            BufferBindingDesc {
                offset: 1,
                ..uniform_desc()
            },
            BufferBindingDesc {
                size: 15,
                ..uniform_desc()
            },
            BufferBindingDesc {
                size: 17,
                ..uniform_desc()
            },
        ] {
            let bindings = [
                BufferBinding {
                    buffer: &buffer,
                    desc,
                },
                BufferBinding {
                    buffer: &output,
                    desc: storage_desc(),
                },
            ];
            assert_eq!(
                runtime
                    .create_bindings(&program, &bindings)
                    .unwrap_err()
                    .kind(),
                D3D12RuntimeErrorKind::InvalidInput
            );
        }
        for desc in [
            BufferBindingDesc {
                offset: 4,
                ..storage_desc()
            },
            BufferBindingDesc {
                size: 17,
                ..storage_desc()
            },
        ] {
            let bindings = [
                BufferBinding {
                    buffer: &buffer,
                    desc: uniform_desc(),
                },
                BufferBinding {
                    buffer: &output,
                    desc,
                },
            ];
            assert_eq!(
                runtime
                    .create_bindings(&program, &bindings)
                    .unwrap_err()
                    .kind(),
                D3D12RuntimeErrorKind::InvalidInput
            );
        }
        let baseline = Arc::strong_count(&old.0);
        let canceled = runtime.prepare_managed_fixture(&old).unwrap();
        assert_eq!(Arc::strong_count(&old.0), baseline + 1);
        assert_eq!(
            runtime.prepare_managed_fixture(&old).err().unwrap().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        drop(canceled);
        assert_eq!(Arc::strong_count(&old.0), baseline);
        let pass = PassHandle { graph: 0, index: 0 };
        // Reuse after cancellation and after actual completion. The executor
        // restores COMMON and the lease does not release while work is pending.
        for _ in 0..2 {
            let prepared = runtime.prepare_managed_fixture(&old).unwrap();
            let mut pending = runtime.submit_prepared(prepared).unwrap();
            assert_eq!(
                runtime.prepare_managed_fixture(&old).err().unwrap().kind(),
                D3D12RuntimeErrorKind::InvalidInput
            );
            let mut result = pending
                .wait(Duration::from_secs(30))
                .unwrap()
                .expect("storage reuse completion");
            assert_eq!(result.readback(pass).unwrap(), first);
        }
        let prepared_old = runtime.prepare_managed_fixture(&old).unwrap();
        let prepared_new = runtime.prepare_managed_fixture(&new).unwrap();
        drop((old, new, buffer, updated, output, new_output, program));
        assert!(
            program_owner.upgrade().is_some(),
            "recordings retain the actual Program and all buffers"
        );
        let mut old_pending = runtime.submit_prepared(prepared_old).unwrap();
        let mut new_pending = runtime.submit_prepared(prepared_new).unwrap();
        runtime.shutdown().unwrap();
        assert!(device.upgrade().is_some(), "pending work retains Device");
        let mut old_result = old_pending
            .wait(Duration::from_secs(30))
            .unwrap()
            .expect("old bindings completion");
        let mut new_result = new_pending
            .wait(Duration::from_secs(30))
            .unwrap()
            .expect("new bindings completion");
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
            "D3D12_STORAGE_REPORT strict={strict} old={old_bytes:?} new={new_bytes:?} validation={report:?}"
        );
    }

    #[cfg(feature = "d3d12-diagnostics")]
    #[test]
    #[ignore = "requires D3D12 hardware, Debug Layer and GPU-Based Validation"]
    fn d3d12_managed_storage_strict() {
        managed_fixture(true);
    }

    #[cfg(not(any(
        feature = "development-validation",
        feature = "d3d12-diagnostics",
        feature = "vulkan-diagnostics",
        feature = "graph-diagnostics"
    )))]
    #[test]
    #[ignore = "requires optimized D3D12 hardware execution without diagnostics"]
    fn d3d12_managed_storage_shipping() {
        if cfg!(debug_assertions) {
            panic!("shipping content acceptance requires --release");
        }
        managed_fixture(false);
    }
}

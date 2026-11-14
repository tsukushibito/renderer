//! Managed compute programs, buffer ownership and immutable bindings.
//!
//! Layout slots come from the reviewed shader manifest. Creation establishes
//! ABI, usage, owner and logical range guarantees before native recording.

#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::{fmt, mem::ManuallyDrop, sync::Arc};

use super::image::{D3D12ImageView, D3D12Sampler};
use super::resources::{DeviceBuffer, MAX_BUFFER_BYTES};

mod descriptors;
#[cfg(test)]
mod fixture;
#[cfg(test)]
mod tests;
use super::{D3D12Runtime, D3D12RuntimeError, bindings::*, device::DeviceOwner};
use crate::program::{
    Binding, BindingLayout, BindingResource, BindingType, BufferDescriptor, BufferRange,
    BufferUsage, ManagedProgram, resolve_bindings, validate_buffer_binding,
};
use descriptors::{DescriptorHeaps, root_parameters};
#[cfg(test)]
pub(super) use fixture::ExecutionLease;

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

#[derive(Clone)]
enum BoundResource {
    Buffer {
        _buffer: D3D12Buffer,
        _range: BufferRange,
    },
    ImageView(D3D12ImageView),
    Sampler(D3D12Sampler),
}
struct BindingOwner {
    // Drop native heaps, then resources, before releasing the last Program/Device.
    _heaps: DescriptorHeaps,
    resources: Box<[BoundResource]>,
    _program: D3D12Program,
}
/// Immutable manifest-ordered bindings retaining their native heaps, Program,
/// Views, images, samplers and buffer versions through GPU ownership transfer.
#[derive(Clone)]
pub struct D3D12Bindings(Arc<BindingOwner>);

impl D3D12Program {
    /// Reflected buffer slots in their canonical native root-parameter order.
    pub fn layout(&self) -> &'static [BindingLayout] {
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
    pub fn layout(&self) -> &'static [BindingLayout] {
        self.0._program.layout()
    }
    pub fn len(&self) -> usize {
        self.0.resources.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.resources.is_empty()
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
            .field("layout", &self.layout())
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
pub(super) fn require_same_owner<T>(
    owner: &Arc<T>,
    resource: &Arc<T>,
) -> Result<(), D3D12RuntimeError> {
    if !Arc::ptr_eq(owner, resource) {
        return Err(D3D12RuntimeError::wrong_owner(
            "managed binding belongs to another D3D12 Device",
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

    /// Resolves every reflected slot once, validates native owners/usages and
    /// creates immutable shader-visible descriptors only after complete success.
    pub fn create_bindings(
        &self,
        program: &D3D12Program,
        bindings: &[Binding<'_, D3D12Buffer, D3D12ImageView, D3D12Sampler>],
    ) -> Result<D3D12Bindings, D3D12RuntimeError> {
        self.device.ensure_active()?;
        require_same_owner(&self.device, &program.0.owner)?;
        let resolved = resolve_bindings(program.layout(), bindings)
            .map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
        let mut resources = Vec::with_capacity(bindings.len());
        for entry in resolved {
            let (layout, binding) =
                entry.map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
            let resource = match &binding.resource {
                BindingResource::Buffer {
                    buffer,
                    offset,
                    size,
                } => {
                    require_same_owner(&self.device, &buffer.0.owner)?;
                    let (alignment, max_range) = match layout.kind {
                        BindingType::Uniform => (UNIFORM_ALIGNMENT, MAX_UNIFORM_RANGE),
                        BindingType::StorageWrite => (STORAGE_ALIGNMENT, MAX_BUFFER_BYTES),
                        _ => {
                            return Err(D3D12RuntimeError::invalid(
                                "buffer resource does not match image or sampler slot",
                            ));
                        }
                    };
                    let range = BufferRange {
                        offset: *offset,
                        size: *size,
                    };
                    validate_buffer_binding(
                        layout,
                        buffer.usage(),
                        buffer.len(),
                        range,
                        alignment,
                        max_range,
                    )
                    .map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
                    BoundResource::Buffer {
                        _buffer: (*buffer).clone(),
                        _range: range,
                    }
                }
                BindingResource::ImageView(view) => {
                    if layout.kind != BindingType::SampledImage {
                        return Err(D3D12RuntimeError::invalid(
                            "image View resource does not match Program slot",
                        ));
                    }
                    require_same_owner(&self.device, &view.image.0.owner)?;
                    BoundResource::ImageView((*view).clone())
                }
                BindingResource::Sampler(sampler) => {
                    if layout.kind != BindingType::Sampler {
                        return Err(D3D12RuntimeError::invalid(
                            "sampler resource does not match Program slot",
                        ));
                    }
                    require_same_owner(&self.device, &sampler.owner)?;
                    BoundResource::Sampler((*sampler).clone())
                }
            };
            resources.push(resource);
        }
        let heaps = DescriptorHeaps::new(&self.device, &resources)?;
        Ok(D3D12Bindings(Arc::new(BindingOwner {
            _heaps: heaps,
            resources: resources.into_boxed_slice(),
            _program: program.clone(),
        })))
    }
}

//! Committed buffer/image ownership and scoped or persistent CPU mapping.
//! Prepared runtime owners release native resources before the Device.

use std::{ffi::c_void, ptr};

use windows_core::{Error, HRESULT};

use super::{D3D12RuntimeError, bindings::*, device::DeviceOwner};

// Deliberately limited baseline, not an available-VRAM guarantee. Keep this
// factory bound shared with planning so unsupported inputs fail before recording.
pub(super) const MAX_BUFFER_BYTES: u64 = 2 * 1024 * 1024 * 1024;

fn validate_buffer_request(
    size: u64,
    heap: D3D12_HEAP_TYPE,
) -> Result<D3D12_RESOURCE_STATES, D3D12RuntimeError> {
    if size == 0 {
        return Err(D3D12RuntimeError::invalid("buffer size must be nonzero"));
    }
    if size > MAX_BUFFER_BYTES {
        return Err(D3D12RuntimeError::unsupported(
            "buffer exceeds the D3D12 M4-04 supported 2 GiB per-buffer limit",
        ));
    }
    match heap {
        D3D12_HEAP_TYPE_UPLOAD => Ok(D3D12_RESOURCE_STATE_GENERIC_READ),
        D3D12_HEAP_TYPE_READBACK => Ok(D3D12_RESOURCE_STATE_COPY_DEST),
        D3D12_HEAP_TYPE_DEFAULT => Ok(D3D12_RESOURCE_STATE_COMMON),
        _ => Err(D3D12RuntimeError::unsupported(
            "unsupported committed buffer heap type",
        )),
    }
}

pub(super) struct DeviceBuffer {
    raw: ID3D12Resource,
    mapped: Option<ptr::NonNull<c_void>>,
    // Drop native COM ownership before removing the optional accounting entry.
    #[cfg(feature = "memory-profiling")]
    _memory: super::memory::CommittedAllocation,
}

impl Drop for DeviceBuffer {
    fn drop(&mut self) {
        if self.mapped.is_some() {
            // SAFETY: Persistent mapping belongs to this resource's sole owner.
            // Slot retirement established GPU/CPU completion before this Drop.
            unsafe {
                self.raw.Unmap(0, None);
            }
        }
        #[cfg(feature = "memory-profiling")]
        {
            crate::measurement::record_native(crate::measurement::NativeEvent::ResourceDestroy, 1);
            crate::measurement::record_native(crate::measurement::NativeEvent::MemoryFree, 1);
        }
    }
}

impl DeviceBuffer {
    /// Borrow the native resource without extending or changing its ownership.
    pub(super) fn raw(&self) -> &ID3D12Resource {
        &self.raw
    }

    pub(super) fn new(
        owner: &DeviceOwner,
        size: u64,
        heap: D3D12_HEAP_TYPE,
    ) -> Result<Self, D3D12RuntimeError> {
        Self::with_flags(owner, size, heap, D3D12_RESOURCE_FLAG_NONE)
    }

    pub(super) fn storage(owner: &DeviceOwner, size: u64) -> Result<Self, D3D12RuntimeError> {
        Self::with_flags(
            owner,
            size,
            D3D12_HEAP_TYPE_DEFAULT,
            D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
        )
    }

    fn with_flags(
        owner: &DeviceOwner,
        size: u64,
        heap: D3D12_HEAP_TYPE,
        flags: D3D12_RESOURCE_FLAGS,
    ) -> Result<Self, D3D12RuntimeError> {
        let initial = validate_buffer_request(size, heap)?;
        let heap_properties = D3D12_HEAP_PROPERTIES {
            Type: heap,
            CPUPageProperty: D3D12_CPU_PAGE_PROPERTY_UNKNOWN,
            MemoryPoolPreference: D3D12_MEMORY_POOL_UNKNOWN,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
        };
        let desc = D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_BUFFER,
            Width: size,
            Height: 1,
            DepthOrArraySize: 1,
            MipLevels: 1,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_ROW_MAJOR,
            Flags: flags,
            ..Default::default()
        };
        let mut raw = None;
        // SAFETY: The creation boundary checked nonzero supported size and heap;
        // node masks and fixed heap states satisfy committed buffer contracts.
        // The owned output slot is empty and remains local on failure.
        #[cfg(feature = "memory-profiling")]
        {
            crate::measurement::record_native(crate::measurement::NativeEvent::ResourceCreate, 1);
            crate::measurement::record_native(crate::measurement::NativeEvent::MemoryAllocate, 1);
        }
        // SAFETY: The checked creation request and owned output satisfy the
        // committed resource contract described above; instrumentation is inert.
        unsafe {
            owner.raw.CreateCommittedResource(
                &heap_properties,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                initial,
                None,
                &mut raw,
            )
        }
        .map_err(|error| owner.native("ID3D12Device::CreateCommittedResource", error))?;
        let raw =
            raw.ok_or_else(|| missing_output(owner, "ID3D12Device::CreateCommittedResource"))?;
        Ok(Self {
            raw,
            mapped: None,
            #[cfg(feature = "memory-profiling")]
            _memory: super::memory::CommittedAllocation::new(owner, heap, &desc)?,
        })
    }

    /// Maps a newly owned CPU-visible arena once at the explicit preparation
    /// boundary. Later accesses still require separate CPU/GPU lease guarantees.
    pub(super) fn map_persistent(
        &mut self,
        owner: &DeviceOwner,
        size: u64,
        read: bool,
    ) -> Result<(), D3D12RuntimeError> {
        if self.mapped.is_some() {
            return Ok(());
        }
        let end = usize::try_from(size)
            .ok()
            .filter(|size| *size <= isize::MAX as usize)
            .ok_or_else(|| D3D12RuntimeError::invalid("mapped arena exceeds CPU address space"))?;
        let range = D3D12_RANGE {
            Begin: 0,
            End: if read { end } else { 0 },
        };
        let mut pointer = ptr::null_mut();
        // SAFETY: Only a newly owned CPU-visible frame arena reaches this call;
        // the caller provided its exact physical capacity and no GPU use exists.
        unsafe { self.raw.Map(0, Some(&range), &mut pointer) }
            .ok()
            .map_err(|error| owner.native("ID3D12Resource::Map(persistent)", error))?;
        let Some(pointer) = ptr::NonNull::new(pointer) else {
            // SAFETY: Balance the successful Map even for a missing pointer.
            unsafe {
                self.raw.Unmap(0, Some(&D3D12_RANGE { Begin: 0, End: 0 }));
            }
            return Err(missing_output(owner, "ID3D12Resource::Map(persistent)"));
        };
        self.mapped = Some(pointer);
        Ok(())
    }

    /// # Safety
    /// The caller must own an UPLOAD allocation from this Device, with at least
    /// `bytes.len()` logical bytes, exclusively before any GPU use. The CPU
    /// source must not overlap the mapped native allocation. The retained
    /// native owner must outlive both this buffer and the CPU access.
    pub(super) unsafe fn write(
        &self,
        owner: &DeviceOwner,
        bytes: &[u8],
    ) -> Result<(), D3D12RuntimeError> {
        // SAFETY: The caller supplies the same exclusive upload guarantees for
        // the prefix range as write_at requires for an arbitrary range.
        unsafe { self.write_at(owner, 0, bytes) }
    }

    /// # Safety
    /// The caller exclusively owns the UPLOAD range `offset..offset+bytes.len()`
    /// before GPU access. That range must fit the live resource, and the CPU
    /// source must not overlap it. The Device owner outlives mapping and release.
    pub(super) unsafe fn write_at(
        &self,
        owner: &DeviceOwner,
        offset: u64,
        bytes: &[u8],
    ) -> Result<(), D3D12RuntimeError> {
        let begin = usize::try_from(offset)
            .map_err(|_| D3D12RuntimeError::invalid("upload offset exceeds CPU address space"))?;
        let end = begin
            .checked_add(bytes.len())
            .filter(|end| *end <= isize::MAX as usize)
            .ok_or_else(|| D3D12RuntimeError::invalid("upload range exceeds CPU address space"))?;
        if let Some(pointer) = self.mapped {
            // SAFETY: The caller supplies exclusive in-bounds upload ownership;
            // persistent Map remains live until the resource's final Drop.
            unsafe {
                ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    pointer.as_ptr().cast::<u8>().add(begin),
                    bytes.len(),
                );
            }
            return Ok(());
        }
        let empty = D3D12_RANGE { Begin: 0, End: 0 };
        let written = D3D12_RANGE {
            Begin: begin,
            End: end,
        };
        let mapping = Mapping::new(owner, &self.raw, empty, written)?;
        // SAFETY: The caller guarantees at least bytes.len() writable bytes;
        // it has not been submitted and the mapped native pointer is nonnull.
        // The CPU source and native allocation do not overlap.
        unsafe {
            ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                mapping.pointer.cast::<u8>().add(begin),
                bytes.len(),
            );
        }
        Ok(())
    }

    /// # Safety
    /// The READBACK range must fit the retained resource and contain initialized
    /// bytes from proven-complete GPU work. No GPU access may overlap this CPU
    /// access, and the destination must not alias the mapped native range.
    pub(super) unsafe fn read_into_at(
        &self,
        owner: &DeviceOwner,
        offset: u64,
        destination: &mut [u8],
    ) -> Result<(), D3D12RuntimeError> {
        let begin = usize::try_from(offset)
            .map_err(|_| D3D12RuntimeError::invalid("readback offset exceeds CPU address space"))?;
        let end = begin
            .checked_add(destination.len())
            .filter(|end| *end <= isize::MAX as usize)
            .ok_or_else(|| {
                D3D12RuntimeError::invalid("readback range exceeds CPU address space")
            })?;
        if let Some(pointer) = self.mapped {
            // SAFETY: Normal Fence completion and the retained CPU lease protect
            // this initialized mapped range from another GPU writer.
            unsafe {
                ptr::copy_nonoverlapping(
                    pointer.as_ptr().cast::<u8>().add(begin),
                    destination.as_mut_ptr(),
                    destination.len(),
                );
            }
            return Ok(());
        }
        let mapping = Mapping::new(
            owner,
            &self.raw,
            D3D12_RANGE {
                Begin: begin,
                End: end,
            },
            D3D12_RANGE { Begin: 0, End: 0 },
        )?;
        // SAFETY: The caller supplies an initialized, completed exclusive range;
        // the checked pointer displacement remains inside its native allocation.
        unsafe {
            ptr::copy_nonoverlapping(
                mapping.pointer.cast::<u8>().add(begin),
                destination.as_mut_ptr(),
                destination.len(),
            );
        }
        Ok(())
    }
}

/// Physical sampled image ownership. Its sole creation caller establishes
/// nonzero Device-supported extent/format before reaching this native factory.
pub(super) struct DeviceImage {
    raw: ID3D12Resource,
    #[cfg(feature = "memory-profiling")]
    _memory: super::memory::CommittedAllocation,
}

impl Drop for DeviceImage {
    fn drop(&mut self) {
        #[cfg(feature = "memory-profiling")]
        {
            crate::measurement::record_native(crate::measurement::NativeEvent::ResourceDestroy, 1);
            crate::measurement::record_native(crate::measurement::NativeEvent::MemoryFree, 1);
        }
    }
}
impl DeviceImage {
    pub(super) fn graph(
        owner: &DeviceOwner,
        desc: crate::graph::ImageDesc,
    ) -> Result<Self, D3D12RuntimeError> {
        let properties = D3D12_HEAP_PROPERTIES {
            Type: D3D12_HEAP_TYPE_DEFAULT,
            CreationNodeMask: 1,
            VisibleNodeMask: 1,
            ..Default::default()
        };
        // The optimized clear is a creation hint. Graph operations establish
        // initialized content independently of it.
        let clear = match desc.format {
            crate::ImageFormat::Depth32Float => Some(D3D12_CLEAR_VALUE {
                Format: DXGI_FORMAT_D32_FLOAT,
                Anonymous: D3D12_CLEAR_VALUE_0 {
                    DepthStencil: D3D12_DEPTH_STENCIL_VALUE {
                        Depth: 1.0,
                        Stencil: 0,
                    },
                },
            }),
            crate::ImageFormat::Rgba8Srgb => None,
            crate::ImageFormat::Rgba8Unorm | crate::ImageFormat::Rgba16Float => {
                Some(D3D12_CLEAR_VALUE {
                    Format: image_format(desc.format),
                    Anonymous: D3D12_CLEAR_VALUE_0 {
                        Color: [0.0, 0.0, 0.0, 1.0],
                    },
                })
            }
        };
        let desc = Self::graph_resource_desc(desc);
        let mut raw = None;
        // SAFETY: Working-set preparation checked the single-layer description,
        // sample count and format support. The owned COM output stays local
        // until creation and memory accounting succeed.
        unsafe {
            #[cfg(feature = "memory-profiling")]
            {
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::ResourceCreate,
                    1,
                );
                crate::measurement::record_native(
                    crate::measurement::NativeEvent::MemoryAllocate,
                    1,
                );
            }
            owner.raw.CreateCommittedResource(
                &properties,
                D3D12_HEAP_FLAG_NONE,
                &desc,
                D3D12_RESOURCE_STATE_COMMON,
                clear.as_ref().map(std::ptr::from_ref),
                &mut raw,
            )
        }
        .map_err(|error| owner.native("ID3D12Device::CreateCommittedResource(image)", error))?;
        Ok(Self {
            raw: raw.ok_or_else(|| {
                missing_output(owner, "ID3D12Device::CreateCommittedResource(image)")
            })?,
            #[cfg(feature = "memory-profiling")]
            _memory: super::memory::CommittedAllocation::new(
                owner,
                D3D12_HEAP_TYPE_DEFAULT,
                &desc,
            )?,
        })
    }
    pub(super) fn graph_resource_desc(desc: crate::graph::ImageDesc) -> D3D12_RESOURCE_DESC {
        let flags = match desc.format {
            crate::ImageFormat::Depth32Float => D3D12_RESOURCE_FLAG_ALLOW_DEPTH_STENCIL,
            crate::ImageFormat::Rgba8Srgb => D3D12_RESOURCE_FLAG_NONE,
            crate::ImageFormat::Rgba8Unorm | crate::ImageFormat::Rgba16Float => {
                // Multisampled render targets do not support UAV use.
                let mut flags = D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET;
                if desc.sample_count == crate::graph::SampleCount::X1 {
                    flags |= D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS;
                }
                flags
            }
        };
        D3D12_RESOURCE_DESC {
            Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
            Width: u64::from(desc.width),
            Height: desc.height,
            DepthOrArraySize: 1,
            MipLevels: desc.mip_levels as u16,
            Format: image_format(desc.format),
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: desc.sample_count.as_u32(),
                Quality: 0,
            },
            Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
            Flags: flags,
            ..Default::default()
        }
    }

    pub(super) fn raw(&self) -> &ID3D12Resource {
        &self.raw
    }
}

pub(super) fn image_format(format: crate::ImageFormat) -> DXGI_FORMAT {
    match format {
        crate::ImageFormat::Rgba8Unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
        crate::ImageFormat::Rgba8Srgb => DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
        crate::ImageFormat::Rgba16Float => DXGI_FORMAT_R16G16B16A16_FLOAT,
        crate::ImageFormat::Depth32Float => DXGI_FORMAT_R32_TYPELESS,
    }
}

pub(super) fn sampled_format(format: crate::ImageFormat) -> DXGI_FORMAT {
    match format {
        crate::ImageFormat::Depth32Float => DXGI_FORMAT_R32_FLOAT,
        _ => image_format(format),
    }
}

struct Mapping<'a> {
    resource: &'a ID3D12Resource,
    pointer: *mut c_void,
    written: D3D12_RANGE,
}
impl<'a> Mapping<'a> {
    fn new(
        owner: &DeviceOwner,
        resource: &'a ID3D12Resource,
        read: D3D12_RANGE,
        written: D3D12_RANGE,
    ) -> Result<Self, D3D12RuntimeError> {
        let mut pointer = ptr::null_mut();
        // SAFETY: This private buffer has one subresource and is CPU-visible.
        // The given ranges are within its logical length. The caller guarantees
        // either not-yet-submitted upload ownership or completed readback.
        unsafe { resource.Map(0, Some(&read), &mut pointer) }
            .ok()
            .map_err(|error| owner.native("ID3D12Resource::Map", error))?;
        let mapping = Self {
            resource,
            pointer,
            written,
        };
        if pointer.is_null() {
            return Err(missing_output(owner, "ID3D12Resource::Map"));
        }
        Ok(mapping)
    }
}
impl Drop for Mapping<'_> {
    fn drop(&mut self) {
        // SAFETY: A successful Map created this guard, which uniquely balances
        // it even when copying or error handling unwinds. No pointer escapes.
        unsafe {
            self.resource.Unmap(0, Some(&self.written));
        }
    }
}

fn missing_output(owner: &DeviceOwner, operation: &'static str) -> D3D12RuntimeError {
    owner.native(
        operation,
        Error::from_hresult(HRESULT(0x8000_4005u32 as i32)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::d3d12::D3D12RuntimeErrorKind;

    #[test]
    fn native_buffer_creation_contract_rejects_unsupported_requests() {
        assert_eq!(
            validate_buffer_request(0, D3D12_HEAP_TYPE_DEFAULT)
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
        assert_eq!(
            validate_buffer_request(MAX_BUFFER_BYTES + 1, D3D12_HEAP_TYPE_DEFAULT)
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::Unsupported
        );
        assert_eq!(
            validate_buffer_request(16, D3D12_HEAP_TYPE_CUSTOM)
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::Unsupported
        );
        for (heap, initial) in [
            (D3D12_HEAP_TYPE_DEFAULT, D3D12_RESOURCE_STATE_COMMON),
            (D3D12_HEAP_TYPE_UPLOAD, D3D12_RESOURCE_STATE_GENERIC_READ),
            (D3D12_HEAP_TYPE_READBACK, D3D12_RESOURCE_STATE_COPY_DEST),
        ] {
            assert_eq!(
                validate_buffer_request(MAX_BUFFER_BYTES, heap).unwrap(),
                initial
            );
        }
    }
}

#[cfg(test)]
mod image_description_tests {
    use super::*;
    #[test]
    fn srgb_mip_chains_are_typed_sampled_resources_without_uav_flags() {
        let native = DeviceImage::graph_resource_desc(crate::graph::ImageDesc {
            width: 8,
            height: 4,
            format: crate::ImageFormat::Rgba8Srgb,
            mip_levels: 4,
            array_layers: 1,
            sample_count: Default::default(),
        });
        assert_eq!(native.MipLevels, 4);
        assert_eq!(native.Format, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB);
        assert_eq!(native.Flags, D3D12_RESOURCE_FLAG_NONE);
    }
}

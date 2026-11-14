//! Root signatures and immutable shader-visible descriptor heaps.

use super::*;

pub(super) struct RootParameters {
    // Pointers in parameters refer into this stable, fully built allocation.
    _ranges: Vec<D3D12_DESCRIPTOR_RANGE>,
    pub(super) parameters: Vec<D3D12_ROOT_PARAMETER>,
}
pub(super) fn root_parameters(
    layout: &[BindingLayout],
) -> Result<RootParameters, D3D12RuntimeError> {
    let cost: usize = layout
        .iter()
        .map(|slot| match slot.kind {
            BindingType::Uniform | BindingType::StorageRead | BindingType::StorageWrite => 2,
            BindingType::SampledImage | BindingType::Sampler => 1,
        })
        .sum();
    if cost > 64 {
        return Err(D3D12RuntimeError::unsupported(
            "managed layout exceeds D3D12 root signature DWORD capacity",
        ));
    }
    let ranges: Vec<_> = layout
        .iter()
        .map(|slot| D3D12_DESCRIPTOR_RANGE {
            RangeType: if slot.kind == BindingType::Sampler {
                D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER
            } else {
                D3D12_DESCRIPTOR_RANGE_TYPE_SRV
            },
            NumDescriptors: 1,
            BaseShaderRegister: slot.binding,
            RegisterSpace: slot.group,
            OffsetInDescriptorsFromTableStart: 0,
        })
        .collect();
    let parameters = layout
        .iter()
        .zip(&ranges)
        .map(|(slot, range)| {
            let (kind, value) = match slot.kind {
                BindingType::Uniform | BindingType::StorageRead | BindingType::StorageWrite => (
                    match slot.kind {
                        BindingType::Uniform => D3D12_ROOT_PARAMETER_TYPE_CBV,
                        BindingType::StorageRead => D3D12_ROOT_PARAMETER_TYPE_SRV,
                        BindingType::StorageWrite => D3D12_ROOT_PARAMETER_TYPE_UAV,
                        _ => unreachable!(),
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
                            pDescriptorRanges: range,
                        },
                    },
                ),
            };
            D3D12_ROOT_PARAMETER {
                ParameterType: kind,
                Anonymous: value,
                ShaderVisibility: D3D12_SHADER_VISIBILITY_ALL,
            }
        })
        .collect();
    Ok(RootParameters {
        _ranges: ranges,
        parameters,
    })
}

pub(super) struct DescriptorHeaps {
    _views: Option<ID3D12DescriptorHeap>,
    _samplers: Option<ID3D12DescriptorHeap>,
    pub(super) _tables: Box<[Option<D3D12_GPU_DESCRIPTOR_HANDLE>]>,
}
// SAFETY: Heaps are free-threaded D3D12 objects. Descriptor writes finish before
// publication; immutable bindings retain all referenced resources and Device.
unsafe impl Send for DescriptorHeaps {}
// SAFETY: No descriptor mutation escapes after creation. GPU uses are read-only.
unsafe impl Sync for DescriptorHeaps {}

impl DescriptorHeaps {
    pub(super) fn new(
        owner: &DeviceOwner,
        resources: &[BoundResource],
    ) -> Result<Self, D3D12RuntimeError> {
        Self::new_inner(
            owner,
            resources,
            #[cfg(test)]
            false,
        )
    }

    pub(super) fn new_inner(
        owner: &DeviceOwner,
        resources: &[BoundResource],
        #[cfg(test)] fail_after_views: bool,
    ) -> Result<Self, D3D12RuntimeError> {
        let counts = [
            resources
                .iter()
                .filter(|r| matches!(r, BoundResource::ImageView(_)))
                .count(),
            resources
                .iter()
                .filter(|r| matches!(r, BoundResource::Sampler(_)))
                .count(),
        ];
        let create =
            |count: usize, kind| -> Result<Option<ID3D12DescriptorHeap>, D3D12RuntimeError> {
                if count == 0 {
                    return Ok(None);
                }
                let desc = D3D12_DESCRIPTOR_HEAP_DESC {
                    Type: kind,
                    NumDescriptors: count as u32,
                    Flags: D3D12_DESCRIPTOR_HEAP_FLAG_SHADER_VISIBLE,
                    NodeMask: 0,
                };
                // SAFETY: The Program's root budget bounds these exact counts. Each
                // supported shader-visible heap is newly owned and immutable later.
                unsafe { owner.raw.CreateDescriptorHeap(&desc) }
                    .map(Some)
                    .map_err(|error| owner.native("ID3D12Device::CreateDescriptorHeap", error))
            };
        let views = create(counts[0], D3D12_DESCRIPTOR_HEAP_TYPE_CBV_SRV_UAV)?;
        #[cfg(test)]
        if fail_after_views && views.is_some() {
            return Err(owner.native(
                "injected sampler heap creation after SRV heap",
                windows_core::Error::from_hresult(windows_core::HRESULT(0x8007_000eu32 as i32)),
            ));
        }
        let samplers = create(counts[1], D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER)?;
        let mut tables = Vec::with_capacity(resources.len());
        let mut used = [0usize; 2];
        for resource in resources {
            let (index, heap) = match resource {
                BoundResource::ImageView(_) => (0, views.as_ref().unwrap()),
                BoundResource::Sampler(_) => (1, samplers.as_ref().unwrap()),
                BoundResource::Buffer { .. } => {
                    tables.push(None);
                    continue;
                }
            };
            // SAFETY: Each immutable descriptor goes to its own allocated slot.
            // Resource kind, View, sampler and Device owner were guaranteed by
            // creation and binding resolution before these void native calls.
            let gpu = unsafe {
                let cpu = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: heap.GetCPUDescriptorHandleForHeapStart().ptr
                        + used[index] * owner.descriptor_steps[index] as usize,
                };
                match resource {
                    BoundResource::ImageView(view) => owner.raw.CreateShaderResourceView(
                        view.image.0.native.raw(),
                        Some(&view.native_desc()),
                        cpu,
                    ),
                    BoundResource::Sampler(sampler) => {
                        owner.raw.CreateSampler(&sampler.native_desc(), cpu)
                    }
                    BoundResource::Buffer { .. } => unreachable!(),
                }
                D3D12_GPU_DESCRIPTOR_HANDLE {
                    ptr: heap.GetGPUDescriptorHandleForHeapStart().ptr
                        + used[index] as u64 * u64::from(owner.descriptor_steps[index]),
                }
            };
            used[index] += 1;
            tables.push(Some(gpu));
        }
        Ok(Self {
            _views: views,
            _samplers: samplers,
            _tables: tables.into_boxed_slice(),
        })
    }

    #[cfg(test)]
    pub(super) fn bind(&self, list: &ID3D12GraphicsCommandList) {
        // SAFETY: All nonnull heaps are shader-visible and owned by the retained
        // binding. D3D12 permits one heap of each type, set together in one call.
        unsafe {
            match (&self._views, &self._samplers) {
                (Some(a), Some(b)) => list.SetDescriptorHeaps(&[Some(a.clone()), Some(b.clone())]),
                (Some(a), None) | (None, Some(a)) => list.SetDescriptorHeaps(&[Some(a.clone())]),
                (None, None) => {}
            }
        }
    }
}

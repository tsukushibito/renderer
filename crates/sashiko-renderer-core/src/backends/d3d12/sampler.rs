//! Immutable, device-wide sampler descriptors for graphics and compute.
use super::{
    D3D12Runtime, D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    raster_storage::{DescriptorHeap, checked_handle_capacity},
};
use crate::capacity::{CapacityRecovery, RuntimeCapacityError, RuntimeCapacityKind};
use crate::program::{AddressMode, FilterMode, MipFilter, SamplerDescriptor};
use std::sync::{Arc, OnceLock};

fn address(mode: AddressMode) -> D3D12_TEXTURE_ADDRESS_MODE {
    match mode {
        AddressMode::Repeat => D3D12_TEXTURE_ADDRESS_MODE_WRAP,
        AddressMode::ClampToEdge => D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
        AddressMode::MirroredRepeat => D3D12_TEXTURE_ADDRESS_MODE_MIRROR,
    }
}

fn sampler_desc(desc: SamplerDescriptor) -> D3D12_SAMPLER_DESC {
    // D3D12_ENCODE_BASIC_FILTER packs min, mag and mip in bits 4, 2 and 0.
    let filter = (i32::from(desc.min_filter == FilterMode::Linear) << 4)
        | (i32::from(desc.mag_filter == FilterMode::Linear) << 2)
        | i32::from(desc.mip_filter == MipFilter::Linear);
    let max_lod = match desc.mip_filter {
        MipFilter::None if desc.mag_filter == desc.min_filter => 0.0,
        // Min/mag selection follows the LOD clamp. A positive value below
        // half a mip preserves minification while point mip selection reads 0.
        MipFilter::None => 0.25,
        _ => f32::MAX,
    };
    D3D12_SAMPLER_DESC {
        Filter: filter
            | if desc.compare_depth {
                D3D12_FILTER_COMPARISON_MIN_MAG_MIP_POINT
            } else {
                0
            },
        AddressU: address(desc.wrap_u),
        AddressV: address(desc.wrap_v),
        AddressW: address(desc.wrap_v),
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: if desc.compare_depth {
            D3D12_COMPARISON_FUNC_LESS_EQUAL
        } else {
            D3D12_COMPARISON_FUNC_NEVER
        },
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: max_lod,
    }
}

pub(super) struct SamplerHeap {
    heap: DescriptorHeap,
    cpu: D3D12_CPU_DESCRIPTOR_HANDLE,
    gpu: D3D12_GPU_DESCRIPTOR_HANDLE,
    step: u32,
    // Slots are append-only. A reader sees a handle only after CreateSampler
    // finishes. Other runtimes on the Device can prepare concurrently.
    slots: [OnceLock<D3D12_GPU_DESCRIPTOR_HANDLE>; SamplerDescriptor::COUNT],
}

// SAFETY: The owned D3D12 descriptor heap is free-threaded. No thread owns
// an apartment-bound object, and heap destruction follows its final Arc.
unsafe impl Send for SamplerHeap {}
// SAFETY: OnceLock serializes the sole write to each descriptor. Other
// writers access disjoint descriptors, and readers see only published slots.
unsafe impl Sync for SamplerHeap {}

impl SamplerHeap {
    fn new(owner: &DeviceOwner) -> Result<Self, D3D12RuntimeError> {
        let heap = DescriptorHeap::new(
            owner,
            SamplerDescriptor::COUNT,
            D3D12_DESCRIPTOR_HEAP_TYPE_SAMPLER,
            true,
        )?;
        // SAFETY: The heap is live, shader visible and owns the complete range.
        let (cpu, gpu) = unsafe {
            (
                heap.raw.GetCPUDescriptorHandleForHeapStart(),
                heap.raw.GetGPUDescriptorHandleForHeapStart(),
            )
        };
        let step = owner.descriptor_steps[1];
        checked_handle_capacity(cpu, Some(gpu), step, SamplerDescriptor::COUNT)?;
        Ok(Self {
            heap,
            cpu,
            gpu,
            step,
            slots: std::array::from_fn(|_| OnceLock::new()),
        })
    }
    fn prepare(&self, owner: &DeviceOwner, descriptors: &[SamplerDescriptor]) {
        for &descriptor in descriptors {
            let index = descriptor.index();
            self.slots[index].get_or_init(|| {
                let cpu = D3D12_CPU_DESCRIPTOR_HANDLE {
                    ptr: self.cpu.ptr + index * self.step as usize,
                };
                // SAFETY: Each valid policy has a unique in-bounds slot for
                // this heap's lifetime. Only this initializer can write it, and
                // the single-entry root table is bound only after publication.
                unsafe { owner.raw.CreateSampler(&sampler_desc(descriptor), cpu) };
                D3D12_GPU_DESCRIPTOR_HANDLE {
                    ptr: self.gpu.ptr + index as u64 * u64::from(self.step),
                }
            });
        }
    }
    pub(super) fn get(
        &self,
        descriptor: SamplerDescriptor,
    ) -> Result<D3D12_GPU_DESCRIPTOR_HANDLE, D3D12RuntimeError> {
        self.slots[descriptor.index()]
            .get()
            .copied()
            .ok_or_else(|| {
                D3D12RuntimeError::capacity_with(
                    "prepare this sampler on the recording device before use",
                    RuntimeCapacityError::new(
                        RuntimeCapacityKind::Samplers,
                        CapacityRecovery::Prepare,
                        None,
                        None,
                    ),
                )
            })
    }
    pub(super) fn raw(&self) -> &ID3D12DescriptorHeap {
        &self.heap.raw
    }
}

impl DeviceOwner {
    pub(super) fn sampler_heap(&self) -> Result<Arc<SamplerHeap>, D3D12RuntimeError> {
        let mut cached = self
            .samplers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(heap) = cached.as_ref() {
            return Ok(Arc::clone(heap));
        }
        let heap = Arc::new(SamplerHeap::new(self)?);
        *cached = Some(Arc::clone(&heap));
        Ok(heap)
    }
}

impl D3D12Runtime {
    /// Prepare immutable descriptors on this Device for subsequent Graph use.
    pub fn prepare_samplers(
        &mut self,
        descriptors: &[SamplerDescriptor],
    ) -> Result<(), D3D12RuntimeError> {
        self.device.ensure_active()?;
        if !descriptors.is_empty() {
            self.device
                .sampler_heap()?
                .prepare(&self.device, descriptors);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_sampling_modes_map_each_filter_and_address_axis_independently() {
        for (wrap_u, expected_u) in [
            (33071, D3D12_TEXTURE_ADDRESS_MODE_CLAMP),
            (33648, D3D12_TEXTURE_ADDRESS_MODE_MIRROR),
            (10497, D3D12_TEXTURE_ADDRESS_MODE_WRAP),
        ] {
            for (wrap_v, expected_v) in [
                (33071, D3D12_TEXTURE_ADDRESS_MODE_CLAMP),
                (33648, D3D12_TEXTURE_ADDRESS_MODE_MIRROR),
                (10497, D3D12_TEXTURE_ADDRESS_MODE_WRAP),
            ] {
                for (min, nearest, linear) in [
                    (
                        9728,
                        D3D12_FILTER_MIN_MAG_MIP_POINT,
                        D3D12_FILTER_MIN_POINT_MAG_LINEAR_MIP_POINT,
                    ),
                    (
                        9729,
                        D3D12_FILTER_MIN_LINEAR_MAG_MIP_POINT,
                        D3D12_FILTER_MIN_MAG_LINEAR_MIP_POINT,
                    ),
                    (
                        9984,
                        D3D12_FILTER_MIN_MAG_MIP_POINT,
                        D3D12_FILTER_MIN_POINT_MAG_LINEAR_MIP_POINT,
                    ),
                    (
                        9985,
                        D3D12_FILTER_MIN_LINEAR_MAG_MIP_POINT,
                        D3D12_FILTER_MIN_MAG_LINEAR_MIP_POINT,
                    ),
                    (
                        9986,
                        D3D12_FILTER_MIN_MAG_POINT_MIP_LINEAR,
                        D3D12_FILTER_MIN_POINT_MAG_MIP_LINEAR,
                    ),
                    (
                        9987,
                        D3D12_FILTER_MIN_LINEAR_MAG_POINT_MIP_LINEAR,
                        D3D12_FILTER_MIN_MAG_MIP_LINEAR,
                    ),
                ] {
                    for (mag, expected_filter) in [(9728, nearest), (9729, linear)] {
                        let desc = sampler_desc(
                            SamplerDescriptor::from_gltf(
                                Some(wrap_u),
                                Some(wrap_v),
                                Some(mag),
                                Some(min),
                            )
                            .unwrap(),
                        );
                        assert_eq!(desc.Filter, expected_filter);
                        assert_eq!((desc.AddressU, desc.AddressV), (expected_u, expected_v));
                        if min >= 9984 {
                            assert_eq!(desc.MaxLOD, f32::MAX);
                        } else {
                            assert!(desc.MaxLOD < 0.5);
                            if min != mag {
                                assert!(desc.MaxLOD > 0.0);
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn linear_repeat_preserves_mip_filtering_and_nearest_clamp_stays_lod_zero() {
        let nearest = sampler_desc(SamplerDescriptor::NearestClamp);
        assert_eq!(nearest.Filter, D3D12_FILTER_MIN_MAG_MIP_POINT);
        assert_eq!(nearest.AddressU, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
        assert_eq!(nearest.MaxLOD, 0.0);
        let linear = sampler_desc(SamplerDescriptor::LinearRepeat);
        assert_eq!(linear.Filter, D3D12_FILTER_MIN_MAG_MIP_LINEAR);
        assert_eq!(linear.AddressU, D3D12_TEXTURE_ADDRESS_MODE_WRAP);
        assert_eq!(linear.AddressV, D3D12_TEXTURE_ADDRESS_MODE_WRAP);
        assert_eq!(linear.AddressW, D3D12_TEXTURE_ADDRESS_MODE_WRAP);
        assert_eq!(linear.MaxLOD, f32::MAX);
        for (policy, u) in [
            (
                SamplerDescriptor::LinearClamp,
                D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
            ),
            (
                SamplerDescriptor::LinearRepeatUClampV,
                D3D12_TEXTURE_ADDRESS_MODE_WRAP,
            ),
        ] {
            let linear = sampler_desc(policy);
            assert_eq!(linear.Filter, D3D12_FILTER_MIN_MAG_MIP_LINEAR);
            assert_eq!(linear.AddressU, u);
            assert_eq!(linear.AddressV, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
            assert_eq!(linear.AddressW, D3D12_TEXTURE_ADDRESS_MODE_CLAMP);
            assert_eq!(linear.MaxLOD, f32::MAX);
        }
    }
}

//! Immutable sampled Image/View/Sampler creation, without queue submission.

#[cfg(test)]
use std::sync::atomic::AtomicBool;
use std::{fmt, sync::Arc};

use super::{
    D3D12Runtime, D3D12RuntimeError, bindings::*, device::DeviceOwner, resources::DeviceImage,
};
use crate::program::{
    ImageDescriptor, ImageViewDescriptor, SamplerDescriptor, validate_image_descriptor,
    validate_image_view_descriptor,
};

pub(super) struct ImageOwner {
    pub(super) native: DeviceImage,
    pub(super) desc: ImageDescriptor,
    #[cfg(test)]
    pub(super) in_use: AtomicBool,
    #[cfg(test)]
    pub(super) initialized: AtomicBool,
    pub(super) owner: Arc<DeviceOwner>,
}
// SAFETY: D3D12 resources are free-threaded; no CPU map/native handle escapes.
// The private executor exclusively leases images through actual retirement.
unsafe impl Send for ImageOwner {}
// SAFETY: Only immutable description/ownership is shared. Private GPU updates
// are serialized by the retained lease, independently of Rust handle cloning.
unsafe impl Sync for ImageOwner {}

/// An uninitialized sampled 2D image with immutable description and ownership.
#[derive(Clone)]
pub struct D3D12Image(pub(super) Arc<ImageOwner>);

/// A validated mip-range SRV interpretation retaining its actual image.
/// D3D12 descriptors are materialized into each immutable binding's own heap.
#[derive(Clone)]
pub struct D3D12ImageView {
    pub(super) image: D3D12Image,
    desc: ImageViewDescriptor,
}

/// An immutable sampler definition retaining its Device.
/// Its native descriptor is materialized when a binding owns a descriptor heap.
#[derive(Clone)]
pub struct D3D12Sampler {
    pub(super) owner: Arc<DeviceOwner>,
    desc: SamplerDescriptor,
}

impl D3D12Image {
    pub fn desc(&self) -> ImageDescriptor {
        self.0.desc
    }
}
impl D3D12ImageView {
    pub fn desc(&self) -> ImageViewDescriptor {
        self.desc
    }
    pub(super) fn native_desc(&self) -> D3D12_SHADER_RESOURCE_VIEW_DESC {
        D3D12_SHADER_RESOURCE_VIEW_DESC {
            Format: super::resources::sampled_format(self.image.desc().format),
            ViewDimension: D3D12_SRV_DIMENSION_TEXTURE2D,
            Shader4ComponentMapping: D3D12_DEFAULT_SHADER_4_COMPONENT_MAPPING as u32,
            Anonymous: D3D12_SHADER_RESOURCE_VIEW_DESC_0 {
                Texture2D: D3D12_TEX2D_SRV {
                    MostDetailedMip: self.desc.base_mip_level,
                    MipLevels: self.desc.mip_level_count,
                    PlaneSlice: 0,
                    ResourceMinLODClamp: 0.0,
                },
            },
        }
    }
}
impl D3D12Sampler {
    pub fn desc(&self) -> SamplerDescriptor {
        self.desc
    }
    pub(super) fn native_desc(&self) -> D3D12_SAMPLER_DESC {
        sampler_desc(self.desc)
    }
}
impl fmt::Debug for D3D12Image {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12Image")
            .field("desc", &self.desc())
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for D3D12ImageView {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12ImageView")
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}
impl fmt::Debug for D3D12Sampler {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("D3D12Sampler")
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}
impl D3D12Runtime {
    /// Creates a single-layer/sample sampled mip chain, without uploading
    /// pixels or submitting commands. Creation does not establish valid contents.
    pub fn create_managed_image(
        &self,
        desc: ImageDescriptor,
    ) -> Result<D3D12Image, D3D12RuntimeError> {
        self.device.ensure_active()?;
        validate_image_descriptor(desc, D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION as u32)
            .map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
        let supported = match desc.format {
            crate::ImageFormat::Rgba8Unorm => self.device.rgba8_sampled,
            crate::ImageFormat::Rgba8Srgb => self.device.rgba8_srgb_sampled,
            crate::ImageFormat::Rgba16Float => self.device.rgba16f_sampled,
            crate::ImageFormat::Depth32Float => self.device.d32_sampled,
        };
        if !supported {
            return Err(D3D12RuntimeError::unsupported(format!(
                "{:?} sampled Texture2D is unsupported",
                desc.format,
            )));
        }
        if desc.mip_levels > 1 {
            super::raster_storage::require_format(
                &self.device,
                super::resources::sampled_format(desc.format),
                D3D12_FORMAT_SUPPORT1_MIP,
            )?;
        }
        let native = DeviceImage::new(&self.device, desc)?;
        Ok(D3D12Image(Arc::new(ImageOwner {
            native,
            desc,
            #[cfg(test)]
            in_use: AtomicBool::new(false),
            #[cfg(test)]
            initialized: AtomicBool::new(false),
            owner: Arc::clone(&self.device),
        })))
    }
    pub fn create_image_view(
        &self,
        image: &D3D12Image,
        desc: ImageViewDescriptor,
    ) -> Result<D3D12ImageView, D3D12RuntimeError> {
        self.device.ensure_active()?;
        super::program::require_same_owner(&self.device, &image.0.owner)?;
        validate_image_view_descriptor(image.desc().format, image.desc().mip_levels, desc)
            .map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
        Ok(D3D12ImageView {
            image: image.clone(),
            desc,
        })
    }
    pub fn create_sampler(
        &self,
        desc: SamplerDescriptor,
    ) -> Result<D3D12Sampler, D3D12RuntimeError> {
        self.device.ensure_active()?;
        Ok(D3D12Sampler {
            owner: Arc::clone(&self.device),
            desc,
        })
    }
}

/// Fixed sampler settings shared by immutable native bindings and prepared Graph heaps.
pub(super) fn sampler_desc(desc: SamplerDescriptor) -> D3D12_SAMPLER_DESC {
    let linear = desc.is_linear();
    let repeat_u = matches!(
        desc,
        SamplerDescriptor::LinearRepeat | SamplerDescriptor::LinearRepeatUClampV
    );
    let repeat_vw = desc == SamplerDescriptor::LinearRepeat;
    D3D12_SAMPLER_DESC {
        Filter: if linear {
            D3D12_FILTER_MIN_MAG_MIP_LINEAR
        } else {
            D3D12_FILTER_MIN_MAG_MIP_POINT
        },
        AddressU: if repeat_u {
            D3D12_TEXTURE_ADDRESS_MODE_WRAP
        } else {
            D3D12_TEXTURE_ADDRESS_MODE_CLAMP
        },
        AddressV: if repeat_vw {
            D3D12_TEXTURE_ADDRESS_MODE_WRAP
        } else {
            D3D12_TEXTURE_ADDRESS_MODE_CLAMP
        },
        AddressW: if repeat_vw {
            D3D12_TEXTURE_ADDRESS_MODE_WRAP
        } else {
            D3D12_TEXTURE_ADDRESS_MODE_CLAMP
        },
        MipLODBias: 0.0,
        MaxAnisotropy: 1,
        ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
        BorderColor: [0.0; 4],
        MinLOD: 0.0,
        MaxLOD: if linear { f32::MAX } else { 0.0 },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

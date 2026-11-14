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

/// A validated full-image SRV interpretation retaining its actual image.
/// D3D12 descriptors are materialized into each immutable binding's own heap.
#[derive(Clone)]
pub struct D3D12ImageView {
    pub(super) image: D3D12Image,
    desc: ImageViewDescriptor,
}

/// An immutable nearest/clamp sampler definition retaining its Device.
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
            Format: DXGI_FORMAT_R8G8B8A8_UNORM,
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
        match self.desc {
            SamplerDescriptor::NearestClamp => D3D12_SAMPLER_DESC {
                Filter: D3D12_FILTER_MIN_MAG_MIP_POINT,
                AddressU: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressV: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                AddressW: D3D12_TEXTURE_ADDRESS_MODE_CLAMP,
                MipLODBias: 0.0,
                MaxAnisotropy: 1,
                ComparisonFunc: D3D12_COMPARISON_FUNC_NEVER,
                BorderColor: [0.0; 4],
                MinLOD: 0.0,
                MaxLOD: 0.0,
            },
        }
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
    /// Creates a single mip/layer/sample RGBA8 sampled image, without uploading
    /// pixels or submitting commands. Creation does not establish valid contents.
    pub fn create_managed_image(
        &self,
        desc: ImageDescriptor,
    ) -> Result<D3D12Image, D3D12RuntimeError> {
        self.device.ensure_active()?;
        validate_image_descriptor(desc, D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION as u32)
            .map_err(|error| D3D12RuntimeError::invalid(error.to_string()))?;
        if !self.device.rgba8_sampled {
            return Err(D3D12RuntimeError::unsupported(
                "RGBA8_UNORM sampled Texture2D is unsupported",
            ));
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
        validate_image_view_descriptor(desc)
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

//! Native sampler descriptors for the prepared Graph heaps.

use super::bindings::*;
use crate::program::SamplerDescriptor;

/// Translate the supported sampler policies for the prepared Graph heaps.
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

//! Native Compute root parameters derived from the managed shader ABI.

use super::super::{D3D12RuntimeError, bindings::*};
use crate::program::{BindingLayout, BindingType};

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
            BindingType::Uniform
            | BindingType::StorageRead
            | BindingType::StorageWrite
            | BindingType::StorageReadWrite => 2,
            BindingType::SampledImage
            | BindingType::StorageImageRead(_)
            | BindingType::StorageImageWrite(_)
            | BindingType::Sampler => 1,
        })
        .sum();
    if cost > 64 {
        return Err(D3D12RuntimeError::unsupported(
            "managed layout exceeds D3D12 root signature DWORD capacity",
        ));
    }
    let ranges: Vec<_> = layout
        .iter()
        .filter(|slot| {
            matches!(
                slot.kind,
                BindingType::SampledImage
                    | BindingType::StorageImageRead(_)
                    | BindingType::StorageImageWrite(_)
                    | BindingType::Sampler
            )
        })
        .map(|slot| D3D12_DESCRIPTOR_RANGE {
            RangeType: match slot.kind {
                BindingType::Sampler => D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER,
                BindingType::SampledImage => D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                BindingType::StorageImageRead(_) | BindingType::StorageImageWrite(_) => {
                    D3D12_DESCRIPTOR_RANGE_TYPE_UAV
                }
                _ => unreachable!(),
            },
            NumDescriptors: 1,
            BaseShaderRegister: slot.binding,
            RegisterSpace: slot.group,
            OffsetInDescriptorsFromTableStart: 0,
        })
        .collect();
    // Finish the table-only allocation before taking any native pointers.
    // Root buffer descriptors retain their manifest index without a range entry.
    let mut table_ranges = ranges.iter();
    let parameters = layout
        .iter()
        .map(|slot| {
            let (kind, value) = match slot.kind {
                BindingType::Uniform
                | BindingType::StorageRead
                | BindingType::StorageWrite
                | BindingType::StorageReadWrite => (
                    match slot.kind {
                        BindingType::Uniform => D3D12_ROOT_PARAMETER_TYPE_CBV,
                        BindingType::StorageRead => D3D12_ROOT_PARAMETER_TYPE_SRV,
                        BindingType::StorageWrite | BindingType::StorageReadWrite => {
                            D3D12_ROOT_PARAMETER_TYPE_UAV
                        }
                        _ => unreachable!(),
                    },
                    D3D12_ROOT_PARAMETER_0 {
                        Descriptor: D3D12_ROOT_DESCRIPTOR {
                            ShaderRegister: slot.binding,
                            RegisterSpace: slot.group,
                        },
                    },
                ),
                BindingType::SampledImage
                | BindingType::StorageImageRead(_)
                | BindingType::StorageImageWrite(_)
                | BindingType::Sampler => (
                    D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE,
                    D3D12_ROOT_PARAMETER_0 {
                        DescriptorTable: D3D12_ROOT_DESCRIPTOR_TABLE {
                            NumDescriptorRanges: 1,
                            pDescriptorRanges: table_ranges.next().unwrap(),
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

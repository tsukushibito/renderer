use super::super::{D3D12Runtime, D3D12RuntimeConfig, D3D12RuntimeErrorKind};
use super::*;
use crate::program::{BindingLayout, BindingType};

#[test]
fn root_descriptors_and_tables_follow_manifest_registers_and_weighted_budget() {
    let layout = [
        BindingLayout {
            group: 5,
            binding: 7,
            kind: BindingType::StorageWrite,
            stages: crate::program::ShaderStages::COMPUTE,
            min_size: 16,
        },
        BindingLayout {
            group: 4,
            binding: 6,
            kind: BindingType::SampledImage,
            stages: crate::program::ShaderStages::COMPUTE,
            min_size: 0,
        },
        BindingLayout {
            group: 2,
            binding: 3,
            kind: BindingType::Uniform,
            stages: crate::program::ShaderStages::COMPUTE,
            min_size: 16,
        },
        BindingLayout {
            group: 8,
            binding: 9,
            kind: BindingType::Sampler,
            stages: crate::program::ShaderStages::COMPUTE,
            min_size: 0,
        },
        BindingLayout {
            group: 10,
            binding: 11,
            kind: BindingType::StorageRead,
            stages: crate::program::ShaderStages::COMPUTE,
            min_size: 16,
        },
        BindingLayout {
            group: 12,
            binding: 13,
            kind: BindingType::StorageImageRead(crate::ImageFormat::Rgba16Float),
            stages: crate::program::ShaderStages::COMPUTE,
            min_size: 0,
        },
        BindingLayout {
            group: 14,
            binding: 15,
            kind: BindingType::StorageImageWrite(crate::ImageFormat::Rgba16Float),
            stages: crate::program::ShaderStages::COMPUTE,
            min_size: 0,
        },
    ];
    let params = root_parameters(&layout).unwrap();
    assert_eq!(params.parameters.len(), layout.len());
    for (slot, parameter) in layout.iter().zip(&params.parameters) {
        // SAFETY: Lowering selected Descriptor for buffers and DescriptorTable
        // for sampled entries; owned ranges keep table pointers valid here.
        unsafe {
            if matches!(
                slot.kind,
                BindingType::Uniform | BindingType::StorageRead | BindingType::StorageWrite
            ) {
                assert_eq!(
                    parameter.ParameterType,
                    match slot.kind {
                        BindingType::StorageWrite => D3D12_ROOT_PARAMETER_TYPE_UAV,
                        BindingType::Uniform => D3D12_ROOT_PARAMETER_TYPE_CBV,
                        BindingType::StorageRead => D3D12_ROOT_PARAMETER_TYPE_SRV,
                        _ => unreachable!(),
                    }
                );
                let desc = parameter.Anonymous.Descriptor;
                assert_eq!(
                    (desc.RegisterSpace, desc.ShaderRegister),
                    (slot.group, slot.binding)
                );
            } else {
                assert_eq!(
                    parameter.ParameterType,
                    D3D12_ROOT_PARAMETER_TYPE_DESCRIPTOR_TABLE
                );
                let table = parameter.Anonymous.DescriptorTable;
                assert_eq!(table.NumDescriptorRanges, 1);
                let desc = &*table.pDescriptorRanges;
                assert_eq!(
                    (desc.RegisterSpace, desc.BaseShaderRegister),
                    (slot.group, slot.binding)
                );
                assert_eq!(
                    desc.RangeType,
                    match slot.kind {
                        BindingType::SampledImage => D3D12_DESCRIPTOR_RANGE_TYPE_SRV,
                        BindingType::StorageImageRead(_) | BindingType::StorageImageWrite(_) =>
                            D3D12_DESCRIPTOR_RANGE_TYPE_UAV,
                        BindingType::Sampler => D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER,
                        _ => unreachable!(),
                    }
                );
            }
        }
    }
    assert_eq!(
        root_parameters(&[layout[4]; 33]).err().unwrap().kind(),
        D3D12RuntimeErrorKind::Unsupported
    );
    assert!(root_parameters(&[layout[1]; 64]).is_ok());
    assert_eq!(
        root_parameters(&[layout[1]; 65]).err().unwrap().kind(),
        D3D12RuntimeErrorKind::Unsupported
    );
}

#[test]
#[ignore = "requires D3D12 hardware and requested native validation"]
fn d3d12_compute_pipeline_creation_failure_releases_root_signature() {
    let runtime = D3D12Runtime::new(D3D12RuntimeConfig::default())
        .expect("D3D12 hardware and requested native validation");
    let monitor = runtime.validation_monitor();
    let managed = ManagedProgram::uniform_readback();
    let owners = Arc::strong_count(&runtime.device);
    let error = ComputePipeline::new(&runtime.device, &managed, true)
        .err()
        .expect("creation failure after the real root signature was created");
    assert_eq!(error.kind(), D3D12RuntimeErrorKind::Native);
    assert!(std::error::Error::source(&error).is_some());
    assert_eq!(Arc::strong_count(&runtime.device), owners);

    let pipeline = ComputePipeline::new(&runtime.device, &managed, false).unwrap();
    assert!(pipeline.matches(managed.identity()));
    drop(pipeline);
    runtime.shutdown().unwrap();
    let report = monitor.snapshot();
    assert!(report.is_clean(), "{report:?}");
    if cfg!(feature = "development-validation") {
        assert!(report.debug_layer_enabled && report.gpu_based_validation_enabled);
        assert!(report.teardown_checked, "{report:?}");
    }
}

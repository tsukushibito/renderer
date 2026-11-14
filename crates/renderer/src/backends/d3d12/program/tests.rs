use super::super::{D3D12RuntimeConfig, D3D12RuntimeErrorKind, D3D12ValidationMode};
use super::*;
use crate::{
    graph::PassHandle,
    program::{ImageDescriptor, ImageFormat, ImageViewDescriptor, SamplerDescriptor},
};
use std::time::Duration;

type Entry<'a> = Binding<'a, D3D12Buffer, D3D12ImageView, D3D12Sampler>;
fn range() -> BufferRange {
    BufferRange {
        offset: 256,
        size: 16,
    }
}
fn buffer_entry(group: u32, buffer: &D3D12Buffer, range: BufferRange) -> Entry<'_> {
    Binding {
        group,
        binding: 0,
        resource: BindingResource::Buffer {
            buffer,
            offset: range.offset,
            size: range.size,
        },
    }
}
fn entries<'a>(uniform: &'a D3D12Buffer, storage: &'a D3D12Buffer) -> [Entry<'a>; 2] {
    [
        buffer_entry(0, uniform, range()),
        buffer_entry(1, storage, range()),
    ]
}
fn image_desc() -> ImageDescriptor {
    ImageDescriptor {
        width: 2,
        height: 2,
        format: ImageFormat::Rgba8Unorm,
    }
}
fn sampled_entries<'a>(
    view: &'a D3D12ImageView,
    sampler: &'a D3D12Sampler,
    output: &'a D3D12Buffer,
) -> [Entry<'a>; 3] {
    [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(view),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Sampler(sampler),
        },
        buffer_entry(1, output, range()),
    ]
}

#[test]
fn allocation_padding_applies_only_to_uniforms_and_never_changes_logical_ranges() {
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
fn root_descriptors_and_tables_follow_manifest_registers_and_weighted_budget() {
    let layout = [
        BindingLayout {
            group: 5,
            binding: 7,
            kind: BindingType::StorageWrite,
            min_size: 16,
        },
        BindingLayout {
            group: 2,
            binding: 3,
            kind: BindingType::Uniform,
            min_size: 16,
        },
        BindingLayout {
            group: 4,
            binding: 6,
            kind: BindingType::SampledImage,
            min_size: 0,
        },
        BindingLayout {
            group: 8,
            binding: 9,
            kind: BindingType::Sampler,
            min_size: 0,
        },
    ];
    let params = root_parameters(&layout).unwrap();
    for (index, (slot, parameter)) in layout.iter().zip(&params.parameters).enumerate() {
        // SAFETY: Lowering selected Descriptor for buffers and DescriptorTable
        // for sampled entries; owned ranges keep table pointers valid here.
        unsafe {
            if index < 2 {
                assert_eq!(
                    parameter.ParameterType,
                    if index == 0 {
                        D3D12_ROOT_PARAMETER_TYPE_UAV
                    } else {
                        D3D12_ROOT_PARAMETER_TYPE_CBV
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
                    if index == 2 {
                        D3D12_DESCRIPTOR_RANGE_TYPE_SRV
                    } else {
                        D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER
                    }
                );
            }
        }
    }
    assert_eq!(
        root_parameters(&[layout[0]; 33]).err().unwrap().kind(),
        D3D12RuntimeErrorKind::Unsupported
    );
    assert!(root_parameters(&[layout[2]; 64]).is_ok());
    assert_eq!(
        root_parameters(&[layout[2]; 65]).err().unwrap().kind(),
        D3D12RuntimeErrorKind::Unsupported
    );
}

fn runtime(strict: bool) -> D3D12Runtime {
    D3D12Runtime::new(D3D12RuntimeConfig {
        validation: if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
    })
    .expect("real D3D12 hardware and requested diagnostics")
}
fn validate_report(monitor: &super::super::D3D12ValidationMonitor, strict: bool) {
    let report = monitor.snapshot();
    for message in report
        .messages
        .iter()
        .filter(|message| message.severity <= 2)
    {
        eprintln!("D3D12_NATIVE_PROBLEM {message:?}");
    }
    assert!(
        report.is_clean(),
        "warnings={} errors={} corruption={}",
        report.warnings,
        report.errors,
        report.corruption
    );
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
}
fn check_creation_failure(runtime: &D3D12Runtime, managed: &ManagedProgram) {
    let owners = Arc::strong_count(&runtime.device);
    assert_eq!(
        runtime
            .create_program_inner(managed, true)
            .unwrap_err()
            .kind(),
        D3D12RuntimeErrorKind::Native
    );
    assert_eq!(Arc::strong_count(&runtime.device), owners);
}
fn wait_bytes(runtime: &mut D3D12Runtime, prepared: super::super::D3D12PreparedGraph) -> Vec<u8> {
    let mut pending = runtime.submit_prepared(prepared).unwrap();
    let mut result = pending
        .wait(Duration::from_secs(30))
        .unwrap()
        .expect("managed fixture completion");
    result.readback(PassHandle { graph: 0, index: 0 }).unwrap()
}

fn storage_fixture(strict: bool) {
    let mut runtime = runtime(strict);
    let monitor = runtime.validation_monitor();
    let device = Arc::downgrade(&runtime.device);
    let managed = ManagedProgram::uniform_readback();
    check_creation_failure(&runtime, &managed);
    let program = runtime.create_program(&managed).unwrap();
    let program_owner = Arc::downgrade(&program.0);
    let [first, second] = crate::program::UNIFORM_TEST_DATA;
    let bytes = |values: [u8; 16]| {
        let mut bytes = vec![0; 256];
        bytes.extend_from_slice(&values);
        bytes
    };
    let uniform = runtime
        .create_managed_buffer(BufferDescriptor::Uniform {
            bytes: &bytes(first),
        })
        .unwrap();
    let newer = runtime
        .create_managed_buffer(BufferDescriptor::Uniform {
            bytes: &bytes(second),
        })
        .unwrap();
    let output = runtime
        .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
        .unwrap();
    let new_output = runtime
        .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
        .unwrap();
    assert_eq!(program.layout(), managed.bindings());
    assert_eq!(
        (uniform.usage(), output.usage()),
        (BufferUsage::Uniform, BufferUsage::Storage)
    );
    assert_eq!((uniform.len(), output.len()), (272, 272));
    assert!(!uniform.is_empty() && !output.is_empty());
    let old = runtime
        .create_bindings(&program, &entries(&uniform, &output))
        .unwrap();
    let mut reversed = entries(&newer, &new_output);
    reversed.reverse();
    let new = runtime.create_bindings(&program, &reversed).unwrap();
    assert_eq!(old.layout(), program.layout());
    assert_eq!(new.len(), 2);
    assert!(!new.is_empty());
    assert_eq!(
        runtime.create_bindings(&program, &[]).unwrap_err().kind(),
        D3D12RuntimeErrorKind::InvalidInput
    );
    assert_eq!(
        runtime
            .create_bindings(
                &program,
                &[
                    buffer_entry(0, &uniform, range()),
                    buffer_entry(0, &uniform, range())
                ]
            )
            .unwrap_err()
            .kind(),
        D3D12RuntimeErrorKind::InvalidInput
    );
    assert_eq!(
        runtime
            .create_bindings(&program, &entries(&output, &uniform))
            .unwrap_err()
            .kind(),
        D3D12RuntimeErrorKind::InvalidInput
    );
    for invalid in [
        BufferRange {
            offset: 1,
            ..range()
        },
        BufferRange {
            size: 15,
            ..range()
        },
        BufferRange {
            size: 17,
            ..range()
        },
    ] {
        assert_eq!(
            runtime
                .create_bindings(
                    &program,
                    &[
                        buffer_entry(0, &uniform, invalid),
                        buffer_entry(1, &output, range())
                    ]
                )
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
    }
    let baseline = Arc::strong_count(&uniform.0);
    for entry in [
        buffer_entry(7, &output, range()),
        buffer_entry(
            1,
            &output,
            BufferRange {
                offset: 4,
                ..range()
            },
        ),
        buffer_entry(
            1,
            &output,
            BufferRange {
                size: 17,
                ..range()
            },
        ),
    ] {
        assert_eq!(
            runtime
                .create_bindings(&program, &[buffer_entry(0, &uniform, range()), entry])
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
    }
    assert_eq!(Arc::strong_count(&uniform.0), baseline);
    let canceled = runtime.prepare_managed_fixture(&old).unwrap();
    assert!(runtime.prepare_managed_fixture(&old).is_err());
    drop(canceled);
    let prepared = runtime.prepare_managed_fixture(&old).unwrap();
    let mut pending = runtime.submit_prepared(prepared).unwrap();
    assert!(runtime.prepare_managed_fixture(&old).is_err());
    let mut result = pending.wait(Duration::from_secs(30)).unwrap().unwrap();
    assert_eq!(
        result.readback(PassHandle { graph: 0, index: 0 }).unwrap(),
        first
    );
    drop((result, pending));
    let prepared_old = runtime.prepare_managed_fixture(&old).unwrap();
    let prepared_new = runtime.prepare_managed_fixture(&new).unwrap();
    drop((old, new, uniform, newer, output, new_output, program));
    assert!(program_owner.upgrade().is_some());
    let mut old_pending = runtime.submit_prepared(prepared_old).unwrap();
    let mut new_pending = runtime.submit_prepared(prepared_new).unwrap();
    runtime.shutdown().unwrap();
    let pass = PassHandle { graph: 0, index: 0 };
    let mut old_result = old_pending.wait(Duration::from_secs(30)).unwrap().unwrap();
    let mut new_result = new_pending.wait(Duration::from_secs(30)).unwrap().unwrap();
    let old_bytes = old_result.readback(pass).unwrap();
    let new_bytes = new_result.readback(pass).unwrap();
    assert_eq!(old_bytes, first);
    assert_eq!(new_bytes, second);
    drop((old_result, new_result, old_pending, new_pending));
    assert!(program_owner.upgrade().is_none());
    assert!(device.upgrade().is_none());
    validate_report(&monitor, strict);
    println!(
        "D3D12_STORAGE_REPORT strict={strict} old={old_bytes:?} new={new_bytes:?} validation={:?}",
        monitor.snapshot()
    );
}

fn sampled_fixture(strict: bool) {
    let mut runtime = runtime(strict);
    let monitor = runtime.validation_monitor();
    let device = Arc::downgrade(&runtime.device);
    let managed = ManagedProgram::sampled_readback();
    check_creation_failure(&runtime, &managed);
    let program = runtime.create_program(&managed).unwrap();
    let program_owner = Arc::downgrade(&program.0);
    for desc in [
        ImageDescriptor {
            width: 0,
            ..image_desc()
        },
        ImageDescriptor {
            height: 0,
            ..image_desc()
        },
        ImageDescriptor {
            width: D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION as u32 + 1,
            ..image_desc()
        },
    ] {
        assert_eq!(
            runtime.create_managed_image(desc).unwrap_err().kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
    }
    let image = runtime.create_managed_image(image_desc()).unwrap();
    let newer = runtime.create_managed_image(image_desc()).unwrap();
    let image_owner = Arc::downgrade(&image.0);
    let view = runtime
        .create_image_view(&image, ImageViewDescriptor::default())
        .unwrap();
    let new_view = runtime
        .create_image_view(&newer, ImageViewDescriptor::default())
        .unwrap();
    assert_eq!(image.desc(), image_desc());
    assert_eq!(view.desc(), ImageViewDescriptor::default());
    assert_eq!(
        runtime
            .create_image_view(
                &image,
                ImageViewDescriptor {
                    base_mip_level: 1,
                    ..Default::default()
                }
            )
            .unwrap_err()
            .kind(),
        D3D12RuntimeErrorKind::InvalidInput
    );
    let sampler = runtime
        .create_sampler(SamplerDescriptor::NearestClamp)
        .unwrap();
    assert_eq!(sampler.desc(), SamplerDescriptor::NearestClamp);
    let output = runtime
        .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
        .unwrap();
    let new_output = runtime
        .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
        .unwrap();
    let old = runtime
        .create_bindings(&program, &sampled_entries(&view, &sampler, &output))
        .unwrap();
    let mut reversed = sampled_entries(&new_view, &sampler, &new_output);
    reversed.reverse();
    let new = runtime.create_bindings(&program, &reversed).unwrap();
    assert_eq!(new.layout(), managed.bindings());
    assert_eq!(new.len(), 3);
    // The selected slot is valid, but the resource kind is not.
    let wrong = [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::Sampler(&sampler),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Sampler(&sampler),
        },
        buffer_entry(1, &output, range()),
    ];
    assert_eq!(
        runtime
            .create_bindings(&program, &wrong)
            .unwrap_err()
            .kind(),
        D3D12RuntimeErrorKind::InvalidInput
    );
    let owners_before_failure = Arc::strong_count(&runtime.device);
    assert_eq!(
        descriptors::DescriptorHeaps::new_inner(&runtime.device, &old.0.resources, true)
            .err()
            .unwrap()
            .kind(),
        D3D12RuntimeErrorKind::Native
    );
    assert_eq!(Arc::strong_count(&runtime.device), owners_before_failure);
    let [first, second] = crate::program::SAMPLE_TEST_DATA;
    assert!(runtime.prepare_sampled_fixture(&old, None).is_err());
    let canceled = runtime.prepare_sampled_fixture(&old, Some(&first)).unwrap();
    assert!(runtime.prepare_sampled_fixture(&old, Some(&first)).is_err());
    drop(canceled);
    assert!(runtime.prepare_sampled_fixture(&old, None).is_err());
    let prepared = runtime.prepare_sampled_fixture(&old, Some(&first)).unwrap();
    assert_eq!(wait_bytes(&mut runtime, prepared), first);
    // Image exclusivity belongs to the shared Image, not the binding version.
    let alias = runtime
        .create_bindings(&program, &sampled_entries(&view, &sampler, &new_output))
        .unwrap();
    let prepared_old = runtime.prepare_sampled_fixture(&old, None).unwrap();
    assert!(runtime.prepare_sampled_fixture(&alias, None).is_err());
    let prepared_new = runtime
        .prepare_sampled_fixture(&new, Some(&second))
        .unwrap();
    drop((
        alias, old, new, view, new_view, image, newer, sampler, output, new_output, program,
    ));
    assert!(image_owner.upgrade().is_some());
    assert!(program_owner.upgrade().is_some());
    let mut old_pending = runtime.submit_prepared(prepared_old).unwrap();
    let mut new_pending = runtime.submit_prepared(prepared_new).unwrap();
    runtime.shutdown().unwrap();
    let pass = PassHandle { graph: 0, index: 0 };
    let mut old_result = old_pending.wait(Duration::from_secs(30)).unwrap().unwrap();
    let mut new_result = new_pending.wait(Duration::from_secs(30)).unwrap().unwrap();
    let old_bytes = old_result.readback(pass).unwrap();
    let new_bytes = new_result.readback(pass).unwrap();
    assert_eq!(old_bytes, first);
    assert_eq!(new_bytes, second);
    drop((old_result, new_result, old_pending, new_pending));
    assert!(image_owner.upgrade().is_none());
    assert!(program_owner.upgrade().is_none());
    assert!(device.upgrade().is_none());
    validate_report(&monitor, strict);
    println!(
        "D3D12_SAMPLED_REPORT strict={strict} old={old_bytes:?} new={new_bytes:?} validation={:?}",
        monitor.snapshot()
    );
}

#[cfg(feature = "d3d12-diagnostics")]
#[test]
#[ignore = "requires D3D12 hardware, Debug Layer and GPU-Based Validation"]
fn d3d12_managed_storage_strict() {
    storage_fixture(true);
}
#[cfg(feature = "d3d12-diagnostics")]
#[test]
#[ignore = "requires D3D12 hardware, Debug Layer and GPU-Based Validation"]
fn d3d12_managed_sampled_strict() {
    sampled_fixture(true);
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
        panic!("shipping acceptance requires --release");
    }
    storage_fixture(false);
}
#[cfg(not(any(
    feature = "development-validation",
    feature = "d3d12-diagnostics",
    feature = "vulkan-diagnostics",
    feature = "graph-diagnostics"
)))]
#[test]
#[ignore = "requires optimized D3D12 hardware execution without diagnostics"]
fn d3d12_managed_sampled_shipping() {
    if cfg!(debug_assertions) {
        panic!("shipping acceptance requires --release");
    }
    sampled_fixture(false);
}

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
        preferred_depth_clear: None,
    }
}
fn sampled_entries<'a>(
    view: &'a D3D12ImageView,
    sampler: &'a D3D12Sampler,
    output: &'a D3D12Buffer,
    output_size: u64,
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
        buffer_entry(
            1,
            output,
            BufferRange {
                size: output_size,
                ..range()
            },
        ),
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
            group: 10,
            binding: 11,
            kind: BindingType::StorageRead,
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
            if index < 3 {
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
                    if index == 3 {
                        D3D12_DESCRIPTOR_RANGE_TYPE_SRV
                    } else {
                        D3D12_DESCRIPTOR_RANGE_TYPE_SAMPLER
                    }
                );
            }
        }
    }
    assert_eq!(
        root_parameters(&[layout[2]; 33]).err().unwrap().kind(),
        D3D12RuntimeErrorKind::Unsupported
    );
    assert!(root_parameters(&[layout[3]; 64]).is_ok());
    assert_eq!(
        root_parameters(&[layout[3]; 65]).err().unwrap().kind(),
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

struct StorageCase {
    producer: [D3D12Bindings; 2],
    reader: Option<[D3D12Bindings; 2]>,
}
impl StorageCase {
    fn prepare(
        &self,
        runtime: &D3D12Runtime,
        version: usize,
    ) -> Result<super::super::D3D12PreparedGraph, D3D12RuntimeError> {
        if let Some(reader) = &self.reader {
            runtime.prepare_storage_read_fixture(&self.producer[version], &reader[version])
        } else {
            runtime.prepare_managed_fixture(&self.producer[version])
        }
    }
}

fn storage_read_bindings(
    runtime: &D3D12Runtime,
    producer: &[D3D12Bindings; 2],
    input: [&D3D12Buffer; 2],
    uniform: &D3D12Buffer,
) -> [D3D12Bindings; 2] {
    let program = runtime
        .create_program(&ManagedProgram::storage_readback())
        .unwrap();
    let output = [0; 2].map(|_| {
        runtime
            .create_managed_buffer(BufferDescriptor::Storage { size: 272 })
            .unwrap()
    });
    let old = runtime
        .create_bindings(&program, &entries(input[0], &output[0]))
        .unwrap();
    let mut reversed = entries(input[1], &output[1]);
    reversed.reverse();
    let new = runtime.create_bindings(&program, &reversed).unwrap();
    // Root SRV creation must forward usage, raw GPUVA alignment, reviewed Load4
    // minimum and logical bounds; padded/native allocation is not a substitute.
    for (buffer, invalid_range) in [
        (uniform, range()),
        (
            input[0],
            BufferRange {
                offset: 4,
                ..range()
            },
        ),
        (
            input[0],
            BufferRange {
                size: 15,
                ..range()
            },
        ),
        (
            input[0],
            BufferRange {
                size: 17,
                ..range()
            },
        ),
    ] {
        assert_eq!(
            runtime
                .create_bindings(
                    &program,
                    &[
                        buffer_entry(0, buffer, invalid_range),
                        buffer_entry(1, &output[0], range())
                    ],
                )
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
    }
    // A public reader binding is not evidence that its contents were initialized.
    assert!(runtime.prepare_managed_fixture(&old).is_err());
    assert!(
        runtime
            .prepare_storage_read_fixture(&old, &producer[0])
            .is_err()
    );
    assert!(
        runtime
            .prepare_storage_read_fixture(&producer[0], &new)
            .is_err()
    );
    let wrong_offset = runtime
        .create_bindings(
            &program,
            &[
                buffer_entry(
                    0,
                    input[0],
                    BufferRange {
                        offset: 0,
                        ..range()
                    },
                ),
                buffer_entry(1, &output[0], range()),
            ],
        )
        .unwrap();
    assert!(
        runtime
            .prepare_storage_read_fixture(&producer[0], &wrong_offset)
            .is_err()
    );
    // Even disjoint byte ranges share one physical resource state. Creation is
    // legal, but this simultaneous SRV/UAV execution cannot use the same buffer.
    let alias = runtime
        .create_bindings(
            &program,
            &[
                buffer_entry(0, input[0], range()),
                buffer_entry(
                    1,
                    input[0],
                    BufferRange {
                        offset: 0,
                        ..range()
                    },
                ),
            ],
        )
        .unwrap();
    assert!(
        runtime
            .prepare_storage_read_fixture(&producer[0], &alias)
            .is_err()
    );
    [old, new]
}

fn storage_fixture(strict: bool, storage_read: bool) {
    let mut runtime = runtime(strict);
    let monitor = runtime.validation_monitor();
    let device = Arc::downgrade(&runtime.device);
    let managed = ManagedProgram::uniform_readback();
    if !storage_read {
        check_creation_failure(&runtime, &managed);
    }
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
    if !storage_read {
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
    }
    let producer = [old, new];
    let reader = storage_read
        .then(|| storage_read_bindings(&runtime, &producer, [&output, &new_output], &uniform));
    let reader_owner = reader
        .as_ref()
        .map(|bindings| Arc::downgrade(&bindings[0].0._program.0));
    let case = StorageCase { producer, reader };
    let buffer_owners: Vec<_> = case
        .producer
        .iter()
        .chain(case.reader.iter().flatten())
        .flat_map(|binding| binding.0.resources.iter())
        .filter_map(|resource| match resource {
            BoundResource::Buffer { _buffer, .. } => Some(Arc::downgrade(&_buffer.0)),
            _ => None,
        })
        .collect();
    let canceled = case.prepare(&runtime, 0).unwrap();
    assert!(case.prepare(&runtime, 0).is_err());
    drop(canceled);
    let prepared = case.prepare(&runtime, 0).unwrap();
    let mut pending = runtime.submit_prepared(prepared).unwrap();
    assert!(case.prepare(&runtime, 0).is_err());
    let mut result = pending.wait(Duration::from_secs(30)).unwrap().unwrap();
    assert_eq!(
        result.readback(PassHandle { graph: 0, index: 0 }).unwrap(),
        first
    );
    drop((result, pending));
    let prepared_old = case.prepare(&runtime, 0).unwrap();
    if let Some(reader) = &case.reader {
        let BoundResource::Buffer {
            _buffer: old_output,
            ..
        } = &reader[0].0.resources[1]
        else {
            unreachable!()
        };
        let alias = runtime
            .create_bindings(&reader[0].0._program, &entries(&new_output, old_output))
            .unwrap();
        // Failing the second lease must release the first intermediate lease.
        assert!(
            runtime
                .prepare_storage_read_fixture(&case.producer[1], &alias)
                .is_err()
        );
    }
    let prepared_new = case.prepare(&runtime, 1).unwrap();
    drop((case, uniform, newer, output, new_output, program));
    assert!(program_owner.upgrade().is_some());
    assert!(buffer_owners.iter().all(|owner| owner.upgrade().is_some()));
    assert!(
        reader_owner
            .as_ref()
            .is_none_or(|owner| owner.upgrade().is_some())
    );
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
    assert!(buffer_owners.iter().all(|owner| owner.upgrade().is_none()));
    assert!(
        reader_owner
            .as_ref()
            .is_none_or(|owner| owner.upgrade().is_none())
    );
    assert!(device.upgrade().is_none());
    validate_report(&monitor, strict);
    println!(
        "D3D12_STORAGE_REPORT storage_read={storage_read} strict={strict} old={old_bytes:?} new={new_bytes:?} validation={:?}",
        monitor.snapshot()
    );
}

enum SampledInitialization<'a> {
    Upload([&'a [u8]; 2]),
    DepthClear([f32; 2]),
}

enum ExpectedSamples {
    All([Vec<u8>; 2]),
    DepthRed([u32; 2]),
}
impl ExpectedSamples {
    fn check(&self, version: usize, actual: &[u8]) {
        match self {
            Self::All(expected) => assert_eq!(actual, expected[version]),
            Self::DepthRed(expected) => {
                assert_eq!(actual.len(), 64);
                for sample in actual.as_chunks::<16>().0 {
                    assert_eq!(
                        u32::from_le_bytes(sample[..4].try_into().unwrap()),
                        expected[version]
                    );
                }
            }
        }
    }
}

fn prepare_sampled_case(
    runtime: &D3D12Runtime,
    bindings: &D3D12Bindings,
    initialize: &SampledInitialization<'_>,
    depth_views: &Option<[D3D12ImageView; 2]>,
    version: usize,
) -> Result<super::super::D3D12PreparedGraph, D3D12RuntimeError> {
    match initialize {
        SampledInitialization::Upload(bytes) => {
            runtime.prepare_sampled_fixture(bindings, Some(bytes[version]))
        }
        SampledInitialization::DepthClear(_) => {
            runtime.prepare_depth_fixture(bindings, Some(&depth_views.as_ref().unwrap()[version]))
        }
    }
}

fn sampled_fixture(
    strict: bool,
    format: ImageFormat,
    managed: ManagedProgram,
    initialize: SampledInitialization<'_>,
    expected: ExpectedSamples,
) {
    let mut runtime = runtime(strict);
    let monitor = runtime.validation_monitor();
    let device = Arc::downgrade(&runtime.device);
    let desc = ImageDescriptor {
        format,
        ..image_desc()
    };
    let (desc, newer_desc) = match &initialize {
        SampledInitialization::DepthClear(values) => (
            ImageDescriptor {
                preferred_depth_clear: Some(values[0]),
                ..desc
            },
            ImageDescriptor {
                preferred_depth_clear: Some(values[1]),
                ..desc
            },
        ),
        SampledInitialization::Upload(_) => (desc, desc),
    };
    let output_size = managed
        .bindings()
        .iter()
        .find(|slot| slot.kind == BindingType::StorageWrite)
        .unwrap()
        .min_size;
    if format == ImageFormat::Rgba8Unorm {
        check_creation_failure(&runtime, &managed);
    }
    let program = runtime.create_program(&managed).unwrap();
    let program_owner = Arc::downgrade(&program.0);
    if format == ImageFormat::Rgba8Unorm {
        for desc in [
            ImageDescriptor { width: 0, ..desc },
            ImageDescriptor { height: 0, ..desc },
            ImageDescriptor {
                width: D3D12_REQ_TEXTURE2D_U_OR_V_DIMENSION as u32 + 1,
                ..desc
            },
        ] {
            assert_eq!(
                runtime.create_managed_image(desc).unwrap_err().kind(),
                D3D12RuntimeErrorKind::InvalidInput
            );
        }
    }
    let image = runtime.create_managed_image(desc).unwrap();
    let newer = runtime.create_managed_image(newer_desc).unwrap();
    let image_owner = Arc::downgrade(&image.0);
    let view = runtime
        .create_image_view(&image, ImageViewDescriptor::default())
        .unwrap();
    let new_view = runtime
        .create_image_view(&newer, ImageViewDescriptor::default())
        .unwrap();
    let depth_views = if format == ImageFormat::D32Float {
        let descriptor = ImageViewDescriptor {
            usage: crate::program::ImageViewUsage::DepthAttachment,
            ..Default::default()
        };
        Some([
            runtime.create_image_view(&image, descriptor).unwrap(),
            runtime.create_image_view(&newer, descriptor).unwrap(),
        ])
    } else {
        None
    };
    assert_eq!(image.desc(), desc);
    assert_eq!(view.desc(), ImageViewDescriptor::default());
    if format == ImageFormat::Rgba8Unorm {
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
    }
    let sampler = runtime
        .create_sampler(SamplerDescriptor::NearestClamp)
        .unwrap();
    assert_eq!(sampler.desc(), SamplerDescriptor::NearestClamp);
    let output = runtime
        .create_managed_buffer(BufferDescriptor::Storage {
            size: 256 + output_size,
        })
        .unwrap();
    let new_output = runtime
        .create_managed_buffer(BufferDescriptor::Storage {
            size: 256 + output_size,
        })
        .unwrap();
    let old = runtime
        .create_bindings(
            &program,
            &sampled_entries(&view, &sampler, &output, output_size),
        )
        .unwrap();
    let mut reversed = sampled_entries(&new_view, &sampler, &new_output, output_size);
    reversed.reverse();
    let new = runtime.create_bindings(&program, &reversed).unwrap();
    assert_eq!(new.layout(), managed.bindings());
    assert_eq!(new.len(), 3);
    if let Some(attachments) = &depth_views {
        assert_eq!(
            runtime
                .prepare_depth_fixture(&old, Some(&attachments[1]))
                .err()
                .unwrap()
                .kind(),
            D3D12RuntimeErrorKind::InvalidInput,
        );
        assert_eq!(
            runtime
                .create_bindings(
                    &program,
                    &sampled_entries(&attachments[0], &sampler, &output, output_size)
                )
                .unwrap_err()
                .kind(),
            D3D12RuntimeErrorKind::InvalidInput
        );
    }

    if format == ImageFormat::Rgba8Unorm {
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
    }
    // Enforce the selected Program's 16- or 64-byte ABI at the native API.
    let too_short = [
        Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::ImageView(&view),
        },
        Binding {
            group: 0,
            binding: 1,
            resource: BindingResource::Sampler(&sampler),
        },
        buffer_entry(
            1,
            &output,
            BufferRange {
                size: output_size - 1,
                ..range()
            },
        ),
    ];
    assert_eq!(
        runtime
            .create_bindings(&program, &too_short)
            .unwrap_err()
            .kind(),
        D3D12RuntimeErrorKind::InvalidInput
    );
    assert!(runtime.prepare_sampled_fixture(&old, None).is_err());
    let canceled = prepare_sampled_case(&runtime, &old, &initialize, &depth_views, 0).unwrap();
    assert!(prepare_sampled_case(&runtime, &old, &initialize, &depth_views, 0).is_err());
    drop(canceled);
    assert!(runtime.prepare_sampled_fixture(&old, None).is_err());
    let prepared = prepare_sampled_case(&runtime, &old, &initialize, &depth_views, 0).unwrap();
    expected.check(0, &wait_bytes(&mut runtime, prepared));
    // Image exclusivity belongs to the shared Image, not the binding version.
    let alias = runtime
        .create_bindings(
            &program,
            &sampled_entries(&view, &sampler, &new_output, output_size),
        )
        .unwrap();
    let prepared_old = runtime.prepare_sampled_fixture(&old, None).unwrap();
    assert!(runtime.prepare_sampled_fixture(&alias, None).is_err());
    let prepared_new = prepare_sampled_case(&runtime, &new, &initialize, &depth_views, 1).unwrap();
    drop(depth_views);
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
    expected.check(0, &old_bytes);
    expected.check(1, &new_bytes);
    drop((old_result, new_result, old_pending, new_pending));
    assert!(image_owner.upgrade().is_none());
    assert!(program_owner.upgrade().is_none());
    assert!(device.upgrade().is_none());
    validate_report(&monitor, strict);
    println!(
        "D3D12_SAMPLED_REPORT format={format:?} strict={strict} old={old_bytes:?} new={new_bytes:?} validation={:?}",
        monitor.snapshot()
    );
}

fn sampled_cases(strict: bool) {
    let rgba8 = crate::program::SAMPLE_TEST_DATA;
    sampled_fixture(
        strict,
        ImageFormat::Rgba8Unorm,
        ManagedProgram::sampled_readback(),
        SampledInitialization::Upload([&rgba8[0], &rgba8[1]]),
        ExpectedSamples::All(rgba8.map(Vec::from)),
    );
    let rgba16 = crate::program::SAMPLE_FLOAT_TEST_DATA;
    let expected = crate::program::SAMPLE_FLOAT_EXPECTED
        .map(|words| words.into_iter().flat_map(u32::to_le_bytes).collect());
    sampled_fixture(
        strict,
        ImageFormat::Rgba16Float,
        ManagedProgram::sampled_float_readback(),
        SampledInitialization::Upload([&rgba16[0], &rgba16[1]]),
        ExpectedSamples::All(expected),
    );
    sampled_fixture(
        strict,
        ImageFormat::D32Float,
        ManagedProgram::sampled_float_readback(),
        SampledInitialization::DepthClear(crate::program::DEPTH_TEST_VALUES),
        ExpectedSamples::DepthRed(crate::program::DEPTH_TEST_EXPECTED),
    );
}

#[cfg(feature = "d3d12-diagnostics")]
#[test]
#[ignore = "requires D3D12 hardware, Debug Layer and GPU-Based Validation"]
fn d3d12_managed_storage_strict() {
    storage_fixture(true, false);
    storage_fixture(true, true);
}
#[cfg(feature = "d3d12-diagnostics")]
#[test]
#[ignore = "requires D3D12 hardware, Debug Layer and GPU-Based Validation"]
fn d3d12_managed_sampled_strict() {
    sampled_cases(true);
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
    storage_fixture(false, false);
    storage_fixture(false, true);
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
    sampled_cases(false);
}

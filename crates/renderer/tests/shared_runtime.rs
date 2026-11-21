#![cfg(all(windows, feature = "vulkan", feature = "d3d12"))]

use std::time::Duration;

use renderer::capacity::CapacityRecovery;
use renderer::graph::{BufferDesc, GraphCapacity, GraphPool, ImageDesc, ImportBuffer};
use renderer::runtime::{
    D3D12Config, ResourceClass, Runtime, RuntimeConfig, RuntimeLimits, VulkanConfig, WorkingSet,
};
use renderer::{d3d12::D3D12ValidationMode, vulkan::ValidationMode};

const BYTES: [u8; 8] = [0, 17, 63, 128, 255, 31, 3, 99];
const WAIT: Duration = Duration::from_secs(5);

fn expect_buffer(runtime: &mut Runtime, pool: &mut GraphPool, snapshot: &ImportBuffer) {
    let mut graph = pool.begin().unwrap();
    let input = graph.import_buffer("registered buffer", snapshot).unwrap();
    let pass = graph
        .readback_buffer("registered contents", input, 0, 8)
        .unwrap();
    let plan = graph.compile().unwrap();
    let receipt = runtime.submit(&plan).unwrap();
    let id = receipt.id();
    drop((receipt, plan));
    let mut completed = runtime.wait_for_frame(id, WAIT).unwrap().unwrap();
    let mut output = [0; 8];
    completed.readback_into(pass, &mut output).unwrap();
    assert_eq!(output, BYTES);
}

fn reprepare(runtime: &mut Runtime, limits: RuntimeLimits) {
    // The exported buffer can retain the upload bytes as index provenance.
    // Keep a second Graph slot for plans that consume the registration.
    let mut pool = GraphPool::new(limits.graph, 2).unwrap();
    let mut upload = pool.begin().unwrap();
    let output = upload
        .buffer("persistent input", BufferDesc { size: 8 })
        .unwrap();
    upload
        .upload_buffer("initial contents", output, 0, &BYTES)
        .unwrap();
    upload.export_buffer(output).unwrap();
    let plan = upload.compile().unwrap();
    let receipt = runtime.submit(&plan).unwrap();
    let snapshot = receipt.export_buffer(output).unwrap();
    let id = receipt.id();
    drop(plan);
    assert_eq!(runtime.pending_frames(), 1);
    let completion = runtime.wait_for_frame(id, WAIT).unwrap().unwrap();
    assert_eq!(completion.id(), id);
    drop(completion);

    // The receipt still owns a submission metadata slot, independently of the
    // immutable buffer snapshot that we keep across failed and successful changes.
    let error = runtime
        .prepare_working_set(WorkingSet::default())
        .unwrap_err();
    assert_eq!(
        error.capacity().map(|details| details.recovery),
        Some(CapacityRecovery::ReleaseOwners)
    );
    drop(receipt);

    let mut unsupported = ImageDesc::rgba8(8, 8);
    unsupported.array_layers = 2;
    assert!(
        runtime
            .prepare_working_set(WorkingSet {
                resources: &[ResourceClass::Image {
                    desc: unsupported,
                    count: 1,
                }],
                ..Default::default()
            })
            .is_err()
    );
    expect_buffer(runtime, &mut pool, &snapshot);

    runtime
        .prepare_working_set(WorkingSet {
            resources: &[ResourceClass::Image {
                desc: ImageDesc::rgba8(13, 7),
                count: 1,
            }],
            ..Default::default()
        })
        .unwrap();
    expect_buffer(runtime, &mut pool, &snapshot);
    // Repreparing must not silently duplicate the range pool and thereby
    // increase the number of outstanding snapshots allowed by RuntimeLimits.
    let key = snapshot.key();
    let error = runtime.refresh_buffer(key).unwrap_err();
    assert_eq!(
        error.capacity().unwrap().recovery,
        CapacityRecovery::ReleaseOwners
    );
    drop(snapshot);
    let snapshot = runtime.refresh_buffer(key).unwrap();
    expect_buffer(runtime, &mut pool, &snapshot);
    runtime.delete(key).unwrap();
    assert!(runtime.drain_frames(Duration::ZERO).unwrap());
}

fn run(strict: bool) {
    let limits = RuntimeLimits {
        graph: GraphCapacity {
            resources: 4,
            passes: 4,
            draws: 1,
            ..Default::default()
        },
        frames: 2,
        results: 2,
        range_snapshots: 1,
        ..Default::default()
    };
    let working_set = WorkingSet {
        resources: &[ResourceClass::Buffer {
            desc: BufferDesc { size: 8 },
            count: 1,
        }],
        ..Default::default()
    };
    let mut vk = Runtime::new(
        RuntimeConfig::Vulkan(VulkanConfig {
            validation: if strict {
                ValidationMode::Required
            } else {
                ValidationMode::Disabled
            },
            ..Default::default()
        }),
        limits,
        working_set,
    )
    .unwrap();
    let mut dx = Runtime::new(
        RuntimeConfig::D3D12(D3D12Config {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            ..Default::default()
        }),
        limits,
        working_set,
    )
    .unwrap();
    let vk_device = vk.as_vulkan().unwrap();
    let dx_device = dx.as_d3d12().unwrap();
    assert_eq!(
        vk_device.device_info().device_luid,
        Some(dx_device.device_info().luid)
    );
    let vk_monitor = vk_device.validation_monitor();
    let dx_monitor = dx_device.validation_monitor();
    reprepare(&mut vk, limits);
    reprepare(&mut dx, limits);
    drop((vk, dx));
    let vk_report = vk_monitor.report();
    let dx_report = dx_monitor.snapshot();
    assert!(vk_report.is_clean(), "{vk_report:?}");
    assert!(dx_report.is_clean(), "{dx_report:?}");
    if strict {
        assert_eq!(vk_report.core_validation, Some(true));
        assert_eq!(vk_report.synchronization_validation, Some(true));
        assert!(dx_report.debug_layer_enabled && dx_report.gpu_based_validation_enabled);
        assert!(dx_report.teardown_checked);
    }
}

#[cfg(feature = "development-validation")]
#[test]
#[ignore = "requires same-adapter Vulkan and D3D12 with strict native validation"]
fn shared_runtime_reprepare_strict() {
    run(true);
}

#[cfg(not(feature = "development-validation"))]
#[test]
#[ignore = "requires same-adapter Vulkan and D3D12"]
fn shared_runtime_reprepare_shipping() {
    run(false);
}

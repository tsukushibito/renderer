//! Headless host process. The host creates its own Device, queue, and texture.
#![cfg(all(windows, feature = "d3d12"))]
use renderer::{
    graph::{GraphCapacity, GraphPool, ImageDesc},
    runtime::{D3D12Config, ResourceClass, Runtime, RuntimeLimits, WorkingSet},
};
use std::{
    error::Error,
    time::{Duration, Instant},
};
use windows_core::{IUnknown, Interface};
#[allow(
    dead_code, non_camel_case_types, non_snake_case, non_upper_case_globals,
    unsafe_op_in_unsafe_fn, clippy::all, clippy::undocumented_unsafe_blocks
)]
#[rustfmt::skip]
#[path = "../src/backends/d3d12/bindings.rs"]
mod bindings;
use bindings::*;

#[test]
#[ignore = "requires a hardware D3D12 adapter and configured validation"]
fn host_retains_device_queue_and_texture_across_renderer_shutdown() -> Result<(), Box<dyn Error>> {
    if cfg!(feature = "development-validation") {
        let mut debug: Option<ID3D12Debug> = None;
        // SAFETY: This test executable is a fresh host process with no Device yet.
        unsafe {
            D3D12GetDebugInterface(&mut debug)?;
            let debug = debug.ok_or("missing Debug Layer")?;
            debug.EnableDebugLayer();
            debug
                .cast::<ID3D12Debug1>()?
                .SetEnableGPUBasedValidation(true);
        }
    }
    let mut device: Option<ID3D12Device> = None;
    // SAFETY: The host selects the platform's default adapter and owns the output.
    unsafe { D3D12CreateDevice(None::<&IUnknown>, D3D_FEATURE_LEVEL_11_0, &mut device) }?;
    let device = device.ok_or("host device creation returned no device")?;
    let queue = |kind| -> windows_core::Result<ID3D12CommandQueue> {
        // SAFETY: The descriptor selects a valid queue of the specified type on node zero.
        unsafe {
            device.CreateCommandQueue(&D3D12_COMMAND_QUEUE_DESC {
                Type: kind,
                ..Default::default()
            })
        }
    };
    let direct = queue(D3D12_COMMAND_LIST_TYPE_DIRECT)?;
    let other = queue(D3D12_COMMAND_LIST_TYPE_COPY)?;
    let desc = ImageDesc::rgba8(2, 1);
    let set = WorkingSet {
        resources: &[ResourceClass::Image { desc, count: 1 }],
        ..Default::default()
    };
    let adopt = |queue: &ID3D12CommandQueue| {
        Runtime::from_d3d12_queue(
            &queue.cast().expect("queue implements IUnknown"),
            D3D12Config::default(),
            RuntimeLimits::default(),
            set,
        )
    };
    assert!(adopt(&other).is_err());
    let mut texture: Option<ID3D12Resource> = None;
    // SAFETY: Valid single-node default heap, 2D color image, and typed COM output.
    unsafe {
        device.CreateCommittedResource(
            &D3D12_HEAP_PROPERTIES {
                Type: D3D12_HEAP_TYPE_DEFAULT,
                CreationNodeMask: 1,
                VisibleNodeMask: 1,
                ..Default::default()
            },
            D3D12_HEAP_FLAG_NONE,
            &D3D12_RESOURCE_DESC {
                Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
                Width: 2,
                Height: 1,
                DepthOrArraySize: 1,
                MipLevels: 1,
                Format: DXGI_FORMAT_R8G8B8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET
                    | D3D12_RESOURCE_FLAG_ALLOW_UNORDERED_ACCESS,
                Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
                ..Default::default()
            },
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &mut texture,
        )?;
    }
    let texture = texture.ok_or("host texture creation returned no texture")?;
    let pixels = [5, 12, 23, 255, 71, 42, 19, 255];
    for initialized in [false, true] {
        let mut runtime = adopt(&direct)?;
        let monitor = runtime.as_d3d12().unwrap().validation_monitor();
        // SAFETY: The host owns the committed COMMON texture on this Device,
        // yields exclusive access, and declares initialized only after the first
        // runtime finished the upload and returned it.
        let imported = unsafe {
            runtime.as_d3d12_mut().unwrap().register_external_image(
                &texture.cast()?,
                desc,
                initialized,
            )?
        };
        let mut pool = GraphPool::new(GraphCapacity::default(), 1)?;
        let mut graph = pool.begin()?;
        let image = graph.import_image("host texture", &imported)?;
        if !initialized {
            graph.upload_image("host handoff", image, &pixels)?;
        }
        let pass = graph.readback_image("verify retained contents", image)?;
        let frame = runtime.submit(&graph.compile()?)?;
        let mut result = runtime
            .wait_for_frame(frame.id(), Duration::from_secs(15))?
            .ok_or("GPU timeout")?;
        let mut bytes = [0; 8];
        result.readback_into(pass, &mut bytes)?;
        assert_eq!(bytes, pixels);
        drop((result, frame, pool));
        assert_eq!(
            runtime
                .as_d3d12_mut()
                .unwrap()
                .release_external_image(imported.key())?,
            Some(true)
        );
        drop(runtime);
        let report = monitor.snapshot();
        if !report.is_clean() {
            let problems: Vec<_> = report
                .messages
                .iter()
                .filter(|m| m.severity <= D3D12_MESSAGE_SEVERITY_WARNING)
                .collect();
            panic!(
                "D3D12 validation failed: {problems:#?}, callback failures {}, dropped {}",
                report.callback_failures, report.dropped_messages
            );
        }
        if cfg!(feature = "development-validation") {
            assert!(
                report.debug_layer_enabled
                    && report.gpu_based_validation_enabled
                    && report.teardown_checked
            );
        }

        // SAFETY: Host COM references remain live after all Renderer owners retire.
        let fence: ID3D12Fence = unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_NONE) }?;
        // SAFETY: A live host queue signals a fresh fence on its own Device.
        unsafe { direct.Signal(&fence, 1) }.ok()?;
        let start = Instant::now();
        loop {
            // SAFETY: This is a read-only query on the retained host fence.
            let completed = unsafe { fence.GetCompletedValue() };
            if completed == 1 {
                break;
            }
            assert_ne!(completed, u64::MAX, "host Device was removed");
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "host fence timed out"
            );
            std::thread::yield_now();
        }
    }
    Ok(())
}

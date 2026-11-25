use super::*;

#[wasm_bindgen_test]
#[ignore = "requires browser WebGPU"]
async fn retained_buffers_and_public_tokens_have_independent_capacities() {
    let target = target().await;
    let desc = BufferDesc { size: 16 };
    let classes = [buffer(desc.size)];
    let set = WorkingSet {
        resources: &classes,
        ..WorkingSet::default()
    };
    let limits = RuntimeLimits {
        resource_capacity: 3,
        range_snapshots: 1,
        frames: 1,
        results: 1,
        ..Default::default()
    };
    let mut runtime = Runtime::from_webgpu_device(
        target.device(),
        target.queue(),
        WebGpuConfig {
            upload_bytes: 256,
            readback_bytes: 256,
        },
        limits,
        set,
    )
    .await
    .unwrap();

    let first = runtime.register_buffer("registered", desc).unwrap();
    runtime.prepare_working_set(set).unwrap();
    let second = runtime
        .register_buffer("replacement capacity", desc)
        .unwrap();
    assert_ne!(first, second);
    runtime.prepare_working_set(set).unwrap();
    let token = runtime.refresh_buffer(second).unwrap();

    let mut pool = GraphPool::new(limits.graph, 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let output = graph.buffer("new buffer", desc).unwrap();
    let bytes = [37; 16];
    graph.upload_buffer("write", output, 0, &bytes).unwrap();
    graph.export_buffer(output).unwrap();
    let pass = graph.readback_buffer("check", output, 0, 16).unwrap();
    let plan = graph.compile().unwrap();

    let error = runtime
        .submit(&plan)
        .err()
        .expect("the only public token is retained");
    assert_eq!(
        error.capacity().unwrap().resource,
        RuntimeCapacityKind::ImportRanges
    );
    assert_eq!(runtime.pending_frames(), 0);
    drop(token);
    let receipt = runtime.submit(&plan).unwrap();
    let imported = receipt.export_buffer(output).unwrap();
    let mut result = runtime.wait_for_webgpu_frame(receipt.id()).await.unwrap();
    let mut actual = [0; 16];
    result.readback_into(pass, &mut actual).unwrap();
    assert_eq!(actual, bytes);
    assert_eq!(
        runtime
            .refresh_buffer(first)
            .unwrap_err()
            .capacity()
            .unwrap()
            .resource,
        RuntimeCapacityKind::ImportRanges
    );
    drop((receipt, imported, result));
    assert_eq!(runtime.refresh_buffer(first).unwrap().key(), first);
    target.check_device().unwrap();
}

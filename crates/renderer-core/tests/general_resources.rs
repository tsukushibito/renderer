//! Public canonical resource/history contracts through consumer-owned Compute.
#![cfg(all(windows, feature = "vulkan", feature = "d3d12"))]

#[allow(dead_code)]
#[path = "../fixtures/compute_fixture.rs"]
mod fixture;

#[path = "support/general_working_set.rs"]
mod working_set;

use renderer_core::{
    d3d12::{
        D3D12FrameCapacity, D3D12ImageClass, D3D12Runtime, D3D12RuntimeConfig, D3D12ValidationMode,
    },
    graph::{
        Binding, BindingResource, BufferDesc, BufferHandle, CompiledGraph, GraphCapacity,
        GraphPool, ImageDesc, ImageHandle, ImportBuffer, ImportImage, PassHandle, ResourceKey,
    },
    vulkan::{FrameCapacity, ResourceClass, RuntimeConfig, ValidationMode, VulkanRuntime},
};
use std::time::Duration;

#[cfg(feature = "memory-profiling")]
#[global_allocator]
static ALLOCATOR: renderer_core::measurement::CountingAllocator<std::alloc::System> =
    renderer_core::measurement::CountingAllocator::new(std::alloc::System);

fn raster_capacity() -> GraphCapacity {
    GraphCapacity {
        draws: 1,
        raster_constant_words: None,
        attachments: 1,
        ..fixture::capacity()
    }
}

trait Backend {
    type Pending;
    type Result;
    fn submit(&mut self, graph: &CompiledGraph) -> Result<Self::Pending, String>;
    fn exports(
        pending: &Self::Pending,
        buffer: BufferHandle,
        image: ImageHandle,
    ) -> (ImportBuffer, ImportImage);
    fn image(pending: &Self::Pending, image: ImageHandle) -> ImportImage;
    fn buffer(pending: &Self::Pending, buffer: BufferHandle) -> ImportBuffer;
    fn wait(pending: &mut Self::Pending) -> Self::Result;
    fn poll(pending: &mut Self::Pending) -> Option<Self::Result>;
    fn read(result: &mut Self::Result, pass: PassHandle, bytes: &mut [u8]);
    fn delete(&mut self, key: ResourceKey);
    fn fresh(&self, key: ResourceKey) -> ImportImage;
    fn fresh_buffer(&self, key: ResourceKey) -> ImportBuffer;
    fn reconfigure(&mut self, classes: &[ResourceClass]) -> Result<(), String>;
}
impl Backend for VulkanRuntime {
    type Pending = renderer_core::vulkan::Submission;
    type Result = renderer_core::vulkan::SubmissionResult;
    fn submit(&mut self, graph: &CompiledGraph) -> Result<Self::Pending, String> {
        VulkanRuntime::submit(self, graph).map_err(|e| format!("{:?}", e.kind()))
    }
    fn exports(p: &Self::Pending, b: BufferHandle, i: ImageHandle) -> (ImportBuffer, ImportImage) {
        (p.export_buffer(b).unwrap(), p.export_image(i).unwrap())
    }
    fn image(p: &Self::Pending, i: ImageHandle) -> ImportImage {
        p.export_image(i).unwrap()
    }
    fn buffer(p: &Self::Pending, b: BufferHandle) -> ImportBuffer {
        p.export_buffer(b).unwrap()
    }
    fn wait(p: &mut Self::Pending) -> Self::Result {
        p.wait(Duration::from_secs(30)).unwrap().unwrap()
    }
    fn poll(p: &mut Self::Pending) -> Option<Self::Result> {
        p.wait(Duration::ZERO).unwrap()
    }
    fn read(r: &mut Self::Result, p: PassHandle, bytes: &mut [u8]) {
        r.readback_into(p, bytes).unwrap();
    }
    fn delete(&mut self, key: ResourceKey) {
        VulkanRuntime::delete(self, key).unwrap();
    }
    fn fresh(&self, key: ResourceKey) -> ImportImage {
        self.refresh_image(key).unwrap()
    }
    fn fresh_buffer(&self, key: ResourceKey) -> ImportBuffer {
        self.refresh_buffer(key).unwrap()
    }
    fn reconfigure(&mut self, classes: &[ResourceClass]) -> Result<(), String> {
        self.prepare_working_set(
            FrameCapacity {
                graph: raster_capacity(),
                ..Default::default()
            },
            classes,
        )
        .map_err(|error| format!("{:?}", error.kind()))?;
        self.prepare_compute_programs(&[fixture::storage_program()])
            .unwrap();
        self.prepare_raster_programs(&[renderer_core::program::ManagedRasterProgram::mesh_hdr()])
            .unwrap();
        Ok(())
    }
}
impl Backend for D3D12Runtime {
    type Pending = renderer_core::d3d12::D3D12Submission;
    type Result = renderer_core::d3d12::D3D12SubmissionResult;
    fn submit(&mut self, graph: &CompiledGraph) -> Result<Self::Pending, String> {
        D3D12Runtime::submit(self, graph).map_err(|e| format!("{:?}", e.kind()))
    }
    fn exports(p: &Self::Pending, b: BufferHandle, i: ImageHandle) -> (ImportBuffer, ImportImage) {
        (p.export_buffer(b).unwrap(), p.export_image(i).unwrap())
    }
    fn image(p: &Self::Pending, i: ImageHandle) -> ImportImage {
        p.export_image(i).unwrap()
    }
    fn buffer(p: &Self::Pending, b: BufferHandle) -> ImportBuffer {
        p.export_buffer(b).unwrap()
    }
    fn wait(p: &mut Self::Pending) -> Self::Result {
        p.wait(Duration::from_secs(30)).unwrap().unwrap()
    }
    fn poll(p: &mut Self::Pending) -> Option<Self::Result> {
        p.wait(Duration::ZERO).unwrap()
    }
    fn read(r: &mut Self::Result, p: PassHandle, bytes: &mut [u8]) {
        r.readback_into(p, bytes).unwrap();
    }
    fn delete(&mut self, key: ResourceKey) {
        D3D12Runtime::delete(self, key).unwrap();
    }
    fn fresh(&self, key: ResourceKey) -> ImportImage {
        self.refresh_image(key).unwrap()
    }
    fn fresh_buffer(&self, key: ResourceKey) -> ImportBuffer {
        self.refresh_buffer(key).unwrap()
    }
    fn reconfigure(&mut self, classes: &[ResourceClass]) -> Result<(), String> {
        let images: Vec<_> = classes
            .iter()
            .filter_map(|class| match *class {
                ResourceClass::Image { desc, count } => Some(D3D12ImageClass { desc, count }),
                _ => None,
            })
            .collect();
        self.prepare_graph_working_set(
            None,
            &images,
            &[renderer_core::program::ManagedRasterProgram::mesh_hdr().into()],
            &[fixture::storage_program()],
        )
        .map_err(|error| format!("{:?}", error.kind()))
    }
}

fn seed(
    pool: &mut GraphPool,
    desc: ImageDesc,
    value: f32,
) -> (CompiledGraph, BufferHandle, ImageHandle) {
    let mut graph = pool.begin().unwrap();
    let buffer = graph
        .buffer("persistent offset", BufferDesc { size: 16 })
        .unwrap();
    graph
        .upload_buffer("offset", buffer, 0, &fixture::vec4_bytes([0.5; 4]))
        .unwrap();
    let image = graph.image("persistent history", desc).unwrap();
    let bytes = fixture::image_bytes(&[[value; 4]; fixture::PIXELS]);
    graph
        .upload_image(
            "initial/reset",
            image,
            &bytes[..desc.byte_len().unwrap() as usize],
        )
        .unwrap();
    graph.export_buffer(buffer).unwrap();
    graph.export_image(image).unwrap();
    (graph.compile().unwrap(), buffer, image)
}

fn step(
    pool: &mut GraphPool,
    buffer: &ImportBuffer,
    image: &ImportImage,
    generation: usize,
) -> (CompiledGraph, ImageHandle, PassHandle) {
    let mut graph = pool.begin().unwrap();
    let input = graph.import_image("history", image).unwrap();
    assert_eq!(
        input,
        graph.import_image("same physical identity", image).unwrap()
    );
    let offset = graph.import_buffer("offset", buffer).unwrap();
    assert_eq!(
        offset,
        graph.import_buffer("same buffer identity", buffer).unwrap()
    );
    let output = graph.image("next history", image.desc()).unwrap();
    // Exercise arbitrary owned bindings, rather than a fixed effect operation.
    graph
        .dispatch(
            "consumer storage add",
            &fixture::storage_program(),
            &[
                Binding {
                    group: 0,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(input, 0, 0).unwrap()),
                },
                Binding {
                    group: 0,
                    binding: 1,
                    resource: BindingResource::Buffer {
                        buffer: offset,
                        offset: 0,
                        size: 16,
                    },
                },
                Binding {
                    group: 1,
                    binding: 0,
                    resource: BindingResource::ImageView(graph.image_view(output, 0, 0).unwrap()),
                },
            ],
        )
        .unwrap();
    if generation % 2 == 1 {
        let unused = graph.image("culled HDR", image.desc()).unwrap();
        graph.copy_image("unused copy", output, unused).unwrap();
    }
    graph.export_image(output).unwrap();
    let pass = graph.readback_image("history readback", output).unwrap();
    (graph.compile().unwrap(), output, pass)
}

fn history<B: Backend>(backend: &mut B, measure: bool) -> Vec<Vec<u8>> {
    let mut pool = GraphPool::new(fixture::capacity(), 12).unwrap();
    let mut all = Vec::new();
    let mut timeouts = 0;
    for (epoch, desc) in [
        fixture::hdr(),
        fixture::hdr(),
        ImageDesc::rgba16_float(2, 2),
    ]
    .into_iter()
    .enumerate()
    {
        let (plan, b, i) = seed(&mut pool, desc, epoch as f32 + 1.0);
        let mut producer = backend.submit(&plan).unwrap();
        let (offset, mut previous) = B::exports(&producer, b, i);
        drop(plan);
        // Producer has not been waited. Three consumers enter the same queue.
        let mut pending = Vec::with_capacity(3);
        let mut passes = Vec::with_capacity(3);
        for frame in 0..3 {
            let (plan, output, pass) = step(&mut pool, &offset, &previous, frame);
            let mut submitted = backend.submit(&plan).unwrap();
            let next = B::image(&submitted, output);
            backend.delete(previous.key());
            previous = next;
            let completed = B::poll(&mut submitted);
            timeouts += usize::from(completed.is_none());
            pending.push((submitted, completed));
            passes.push(pass);
        }
        // Delete while all work is still unobserved. Results must remain valid.
        backend.delete(previous.key());
        backend.delete(offset.key());
        drop(B::wait(&mut producer));
        for (frame, ((mut submitted, completed), pass)) in
            pending.into_iter().zip(passes).enumerate()
        {
            let mut result = completed.unwrap_or_else(|| B::wait(&mut submitted));
            let mut actual = vec![0; desc.byte_len().unwrap() as usize];
            B::read(&mut result, pass, &mut actual);
            let expected = fixture::image_bytes(
                &[[epoch as f32 + 1.0 + (frame + 1) as f32 * 0.5; 4]; fixture::PIXELS],
            );
            assert_eq!(actual, &expected[..actual.len()]);
            all.push(actual);
        }
    }
    // A separate changing-input steady-state window keeps all reporting outside
    // the measurement and includes result reads, deletes and Graph slot return.
    let (plan, b, i) = seed(&mut pool, fixture::hdr(), 1.0);
    let mut producer = backend.submit(&plan).unwrap();
    let (offset, mut previous) = B::exports(&producer, b, i);
    drop(B::wait(&mut producer));
    drop(producer);
    drop(plan);
    #[cfg(feature = "memory-profiling")]
    let mut events = renderer_core::measurement::Snapshot::default();
    for frame in 0..48 {
        #[cfg(feature = "memory-profiling")]
        let before = renderer_core::measurement::Snapshot::now();
        let (plan, output, pass) = step(&mut pool, &offset, &previous, frame);
        let mut pending = backend.submit(&plan).unwrap();
        let next = B::image(&pending, output);
        backend.delete(previous.key());
        previous = next;
        let mut result = B::poll(&mut pending).unwrap_or_else(|| {
            timeouts += 1;
            B::wait(&mut pending)
        });
        let mut bytes = [0; fixture::IMAGE_BYTES];
        B::read(&mut result, pass, &mut bytes);
        let expected =
            fixture::image_bytes(&[[1.0 + (frame + 1) as f32 * 0.5; 4]; fixture::PIXELS]);
        assert_eq!(bytes, expected);
        drop((plan, pending, result));
        #[cfg(feature = "memory-profiling")]
        if frame >= 24 {
            events.accumulate(renderer_core::measurement::Snapshot::now().since(before));
        }
    }
    backend.delete(previous.key());
    backend.delete(offset.key());
    #[cfg(feature = "memory-profiling")]
    if measure {
        assert!(
            events.is_allocation_free(),
            "general-resource memory: {events:?}"
        );
        println!("GENERAL_RESOURCE_MEMORY {events:?}");
    }
    #[cfg(not(feature = "memory-profiling"))]
    let _ = measure;
    println!("GENERAL_RESOURCE_TIMEOUTS={timeouts}");
    all
}

fn stale_and_cancel<B: Backend>(backend: &mut B) {
    let mut pool = GraphPool::new(fixture::capacity(), 8).unwrap();
    let (plan, b, i) = seed(&mut pool, fixture::hdr(), 2.0);
    let mut first = backend.submit(&plan).unwrap();
    let (buffer, image) = B::exports(&first, b, i);
    let (reader, _, _) = step(&mut pool, &buffer, &image, 0);
    // Read-only use may change native state but must preserve content revision.
    let mut readonly = pool.begin().unwrap();
    let source = readonly.import_image("readonly", &image).unwrap();
    let pass = readonly.readback_image("readonly bytes", source).unwrap();
    let readonly = readonly.compile().unwrap();
    let mut first_read = backend.submit(&readonly).unwrap();
    let mut second_read = backend.submit(&readonly).unwrap();
    for pending in [&mut first_read, &mut second_read] {
        let mut result = B::wait(pending);
        let mut bytes = [0; fixture::IMAGE_BYTES];
        B::read(&mut result, pass, &mut bytes);
        assert_eq!(bytes, fixture::image_bytes(&[[2.0; 4]; fixture::PIXELS]));
    }
    let mut writer = pool.begin().unwrap();
    let update_buffer = writer.import_buffer("update buffer", &buffer).unwrap();
    writer
        .upload_buffer(
            "replace buffer",
            update_buffer,
            0,
            &fixture::vec4_bytes([1.0; 4]),
        )
        .unwrap();
    writer.export_buffer(update_buffer).unwrap();
    let target = writer.import_image("update", &image).unwrap();
    writer
        .upload_image(
            "replace",
            target,
            &fixture::image_bytes(&[[4.0; 4]; fixture::PIXELS]),
        )
        .unwrap();
    writer.export_image(target).unwrap();
    let mut update = backend.submit(&writer.compile().unwrap()).unwrap();
    assert_eq!(backend.submit(&reader).err().unwrap(), "StaleResource");
    let fresh = backend.fresh(image.key());
    let buffer = backend.fresh_buffer(buffer.key());
    assert_eq!(fresh.key(), image.key());
    let (new, output, pass) = step(&mut pool, &buffer, &fresh, 0);
    let mut pending = backend.submit(&new).unwrap();
    let exported = B::image(&pending, output);
    backend.delete(fresh.key());
    backend.delete(buffer.key());
    backend.delete(exported.key());
    let mut result = B::wait(&mut pending);
    let mut bytes = [0; fixture::IMAGE_BYTES];
    B::read(&mut result, pass, &mut bytes);
    assert_eq!(bytes, fixture::image_bytes(&[[5.0; 4]; fixture::PIXELS]));
    assert_eq!(backend.submit(&new).err().unwrap(), "StaleResource");
    drop(B::wait(&mut first));
    drop(B::wait(&mut update));
}

fn recording_transactions(dx: &mut D3D12Runtime) {
    let mut pool = GraphPool::new(fixture::capacity(), 6).unwrap();
    let (seed, b, i) = seed(&mut pool, fixture::hdr(), 3.0);
    let mut producer = dx.submit(&seed).unwrap();
    let (buffer, image) = <D3D12Runtime as Backend>::exports(&producer, b, i);
    drop(<D3D12Runtime as Backend>::wait(&mut producer));
    drop(producer);
    let (read, _, _) = step(&mut pool, &buffer, &image, 0);
    let prepared = dx.prepare(&read).unwrap();
    let mut write = pool.begin().unwrap();
    let target = write
        .import_image("update after recording", &image)
        .unwrap();
    write
        .upload_image(
            "replace",
            target,
            &fixture::image_bytes(&[[4.0; 4]; fixture::PIXELS]),
        )
        .unwrap();
    write.export_image(target).unwrap();
    let mut update = dx.submit(&write.compile().unwrap()).unwrap();
    assert_eq!(
        dx.submit_prepared(prepared).err().unwrap().kind(),
        renderer_core::d3d12::D3D12RuntimeErrorKind::StaleResource
    );
    // A stale recording must return all its native/metadata leases immediately.
    let fresh = dx.refresh_image(image.key()).unwrap();
    let (read, _, _) = step(&mut pool, &buffer, &fresh, 0);
    drop(dx.prepare(&read).unwrap());
    dx.delete(image.key()).unwrap();
    dx.delete(buffer.key()).unwrap();
    drop(<D3D12Runtime as Backend>::wait(&mut update));
}

fn raster_imports<B: Backend>(backend: &mut B, reconfigure: bool) {
    use renderer_core::graph::{Attachment, LoadOp, RasterDraw, RasterParameters, StoreOp};
    let mut pool = GraphPool::new(raster_capacity(), 3).unwrap();
    let mut seed = pool.begin().unwrap();
    let vertices = seed
        .buffer("persistent vertices", BufferDesc { size: 84 })
        .unwrap();
    let indices = seed
        .buffer("persistent indices", BufferDesc { size: 12 })
        .unwrap();
    let attributes: [f32; 21] = [
        -1.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0, 3.0, -1.0, 0.5, 1.0, 1.0, 1.0, 1.0, -1.0, 3.0, 0.5,
        1.0, 1.0, 1.0, 1.0,
    ];
    let mut bytes = [0; 84];
    for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(attributes) {
        out.copy_from_slice(&value.to_le_bytes());
    }
    seed.upload_buffer("vertices", vertices, 0, &bytes).unwrap();
    let mut bytes = [0; 12];
    for (out, value) in bytes.as_chunks_mut::<4>().0.iter_mut().zip([0_u32, 1, 2]) {
        out.copy_from_slice(&value.to_le_bytes());
    }
    seed.upload_buffer("indices", indices, 0, &bytes).unwrap();
    let color = seed.image("persistent attachment", fixture::hdr()).unwrap();
    seed.upload_image(
        "attachment initialized",
        color,
        &fixture::image_bytes(&[[0.0; 4]; fixture::PIXELS]),
    )
    .unwrap();
    seed.export_buffer(vertices).unwrap();
    seed.export_buffer(indices).unwrap();
    seed.export_image(color).unwrap();
    let producer = backend.submit(&seed.compile().unwrap()).unwrap();
    let vertices = B::buffer(&producer, vertices);
    let indices = B::buffer(&producer, indices);
    let color = B::image(&producer, color);
    let mut producer = Some(producer);
    if reconfigure {
        drop(B::wait(producer.as_mut().unwrap()));
        drop(producer.take());
        backend
            .reconfigure(&[
                ResourceClass::Buffer {
                    desc: BufferDesc { size: 512 },
                    count: 2,
                },
                ResourceClass::Image {
                    desc: ImageDesc::rgba16_float(2, 2),
                    count: 1,
                },
            ])
            .unwrap();
    }
    let mut graph = pool.begin().unwrap();
    let v = graph.import_buffer("vertices", &vertices).unwrap();
    let i = graph.import_buffer("indices", &indices).unwrap();
    let target = graph.import_image("attachment Load", &color).unwrap();
    let view = graph.image_view(target, 0, 0).unwrap();
    renderer::passes::opaque(
        &mut graph,
        "imported draw",
        [RasterDraw {
            vertices: v,
            indices: i,
            first_index: 0,
            index_count: 3,
            instance_count: 1,
            bindings: None,
            parameters: RasterParameters::default().into(),
        }],
        Attachment {
            view,
            load: LoadOp::Load,
            store: StoreOp::Store,
        },
        None,
        renderer::passes::OpaqueDepth::Write,
    )
    .unwrap();
    let pass = graph.readback_image("draw pixels", target).unwrap();
    let plan = graph.compile().unwrap();
    let mut draw = backend.submit(&plan).unwrap();
    backend.delete(vertices.key());
    backend.delete(indices.key());
    backend.delete(color.key());
    let mut result = B::wait(&mut draw);
    let mut bytes = [0; fixture::IMAGE_BYTES];
    B::read(&mut result, pass, &mut bytes);
    assert_eq!(bytes, fixture::image_bytes(&[[1.0; 4]; fixture::PIXELS]));
    if let Some(producer) = &mut producer {
        drop(B::wait(producer));
    }
}

fn failed_export_capture(strict: bool) {
    let mut dx = D3D12Runtime::new(D3D12RuntimeConfig {
        validation: if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
        capacity: D3D12FrameCapacity {
            executions: 1,
            results: 1,
            resources: 4,
            operations: 8,
            buffers: 2,
            buffer_bytes: 16,
            upload_bytes: 64,
            readback_bytes: 64,
            scratch_bytes: 16,
            range_snapshots: 1,
            ..Default::default()
        },
    })
    .unwrap();
    let mut pool = GraphPool::new(fixture::capacity(), 2).unwrap();
    let mut graph = pool.begin().unwrap();
    for _ in 0..2 {
        let buffer = graph.buffer("buffer", BufferDesc { size: 16 }).unwrap();
        graph.upload_buffer("upload", buffer, 0, &[7; 16]).unwrap();
        graph.export_buffer(buffer).unwrap();
    }
    // The second token fails after both native operands and one export token
    // have been acquired. Cancellation must roll all three owners back.
    let plan = graph.compile().unwrap();
    assert_eq!(
        dx.prepare(&plan).err().unwrap().kind(),
        renderer_core::d3d12::D3D12RuntimeErrorKind::CapacityExhausted
    );
    drop(plan);
    let mut graph = pool.begin().unwrap();
    let buffer = graph.buffer("recovered", BufferDesc { size: 16 }).unwrap();
    graph.upload_buffer("bytes", buffer, 0, &[9; 16]).unwrap();
    graph.export_buffer(buffer).unwrap();
    let pass = graph.readback_buffer("read", buffer, 0, 16).unwrap();
    let mut pending = dx.submit(&graph.compile().unwrap()).unwrap();
    let key = pending.export_buffer(buffer).unwrap().key();
    let mut result = <D3D12Runtime as Backend>::wait(&mut pending);
    let mut bytes = [0; 16];
    result.readback_into(pass, &mut bytes).unwrap();
    assert_eq!(bytes, [9; 16]);
    dx.delete(key).unwrap();
    drop((pending, result, pool));
    dx.shutdown().unwrap();
}

fn cross_backend_owners(vk: &mut VulkanRuntime, dx: &mut D3D12Runtime) {
    let mut pool = GraphPool::new(fixture::capacity(), 2).unwrap();
    let (plan, b, i) = seed(&mut pool, fixture::hdr(), 1.0);
    let mut vp = vk.submit(&plan).unwrap();
    let mut dp = dx.submit(&plan).unwrap();
    let (vb, vi) = <VulkanRuntime as Backend>::exports(&vp, b, i);
    let (db, di) = <D3D12Runtime as Backend>::exports(&dp, b, i);
    fn rejects<B: Backend>(backend: &mut B, pool: &mut GraphPool, image: &ImportImage) {
        let mut graph = pool.begin().unwrap();
        let image = graph.import_image("foreign backend", image).unwrap();
        graph.readback_image("foreign contents", image).unwrap();
        assert_eq!(
            backend.submit(&graph.compile().unwrap()).err().unwrap(),
            "WrongOwner"
        );
    }
    rejects(dx, &mut pool, &vi);
    rejects(vk, &mut pool, &di);
    vk.delete(vb.key()).unwrap();
    vk.delete(vi.key()).unwrap();
    dx.delete(db.key()).unwrap();
    dx.delete(di.key()).unwrap();
    drop(<VulkanRuntime as Backend>::wait(&mut vp));
    drop(<D3D12Runtime as Backend>::wait(&mut dp));
}

fn run(strict: bool) {
    const EXECUTIONS: usize = 4;
    let mut vk = VulkanRuntime::new_with_validation(
        RuntimeConfig {
            execution_slots: 4,
            ..Default::default()
        },
        if strict {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
    )
    .unwrap();
    let mut dx = D3D12Runtime::new(D3D12RuntimeConfig {
        validation: if strict {
            D3D12ValidationMode::Required
        } else {
            D3D12ValidationMode::Disabled
        },
        capacity: D3D12FrameCapacity {
            executions: EXECUTIONS,
            resources: 16,
            operations: 32,
            buffers: 2,
            buffer_bytes: 512,
            upload_bytes: 16384,
            readback_bytes: 16384,
            scratch_bytes: 512,
            ..Default::default()
        },
    })
    .unwrap();
    assert_eq!(vk.device_info().device_luid, Some(dx.device_info().luid));
    let vk_info = vk.device_info();
    let dx_info = dx.device_info();
    let vm = vk.validation_monitor();
    let dm = dx.validation_monitor();
    vk.prepare_working_set(
        FrameCapacity {
            graph: raster_capacity(),
            work_slots: 4,
            ..Default::default()
        },
        &[
            ResourceClass::Buffer {
                desc: BufferDesc { size: 16 },
                count: 4,
            },
            ResourceClass::Buffer {
                desc: BufferDesc { size: 84 },
                count: 2,
            },
            ResourceClass::Buffer {
                desc: BufferDesc { size: 12 },
                count: 2,
            },
            ResourceClass::Image {
                desc: fixture::hdr(),
                count: 8,
            },
            ResourceClass::Image {
                desc: ImageDesc::rgba16_float(2, 2),
                count: 8,
            },
        ],
    )
    .unwrap();
    dx.prepare_graph_working_set(
        None,
        &[
            D3D12ImageClass {
                desc: fixture::hdr(),
                count: 3 * EXECUTIONS,
            },
            D3D12ImageClass {
                desc: ImageDesc::rgba16_float(2, 2),
                count: 3 * EXECUTIONS,
            },
        ],
        &[renderer_core::program::ManagedRasterProgram::mesh_hdr().into()],
        &[fixture::storage_program()],
    )
    .unwrap();
    vk.prepare_compute_programs(&[fixture::storage_program()])
        .unwrap();
    vk.prepare_raster_programs(&[renderer_core::program::ManagedRasterProgram::mesh_hdr()])
        .unwrap();
    raster_imports(&mut vk, false);
    let a = history(&mut vk, !strict);
    stale_and_cancel(&mut vk);
    let b = history(&mut dx, !strict);
    raster_imports(&mut dx, false);
    stale_and_cancel(&mut dx);
    recording_transactions(&mut dx);
    failed_export_capture(strict);
    working_set::metadata(strict);
    working_set::reconfiguration(strict);
    cross_backend_owners(&mut vk, &mut dx);
    assert_eq!(a, b);
    // Unsubmitted recordings never publish contents or registration.
    let mut pool = GraphPool::new(fixture::capacity(), 2).unwrap();
    let (plan, b, i) = seed(&mut pool, fixture::hdr(), 3.0);
    drop(dx.prepare(&plan).unwrap());
    let mut pending = dx.submit(&plan).unwrap();
    let (b, i) = <D3D12Runtime as Backend>::exports(&pending, b, i);
    dx.delete(b.key()).unwrap();
    dx.delete(i.key()).unwrap();
    drop(<D3D12Runtime as Backend>::wait(&mut pending));
    drop(pending);
    drop((plan, pool));
    drop(vk);
    dx.shutdown().unwrap();
    let vr = vm.report();
    let dr = dm.snapshot();
    assert!(vr.is_clean(), "{vr:?}");
    assert!(dr.is_clean(), "{dr:?}");
    if strict {
        assert!(vr.core_validation == Some(true) && vr.synchronization_validation == Some(true));
        assert!(dr.debug_layer_enabled && dr.gpu_based_validation_enabled && dr.teardown_checked);
    }
    println!(
        "GENERAL_RESOURCE_REPORT={}",
        serde_json::json!({
            "strict": strict, "outputs": a,
            "adapter": { "vulkan": vk_info.name, "d3d12": dx_info.name, "luid": dx_info.luid, "driver_raw": vk_info.driver_version_raw },
            "vulkan": { "core": vr.core_validation, "sync": vr.synchronization_validation,
                "warnings": vr.warnings, "errors": vr.errors, "callback_failures": vr.callback_failures, "dropped_problems": vr.dropped_problems },
            "d3d12": { "debug_layer": dr.debug_layer_enabled, "gbv": dr.gpu_based_validation_enabled,
                "warnings": dr.warnings, "errors": dr.errors, "corruption": dr.corruption,
                "callback_failures": dr.callback_failures, "dropped_messages": dr.dropped_messages, "teardown": dr.teardown_checked }
        })
    );
}

#[test]
#[ignore = "same-adapter Vulkan/D3D12 hardware with Core/SyncVal and Debug Layer/GBV"]
#[cfg(feature = "development-validation")]
fn general_resources_strict() {
    run(true);
}

#[test]
#[ignore = "optimized two-API hardware with Renderer/native diagnostics excluded"]
#[cfg(not(feature = "development-validation"))]
fn general_resources_shipping() {
    run(false);
}

use renderer::graph::{BufferDesc, BufferHandle, Graph, GraphCapacity, GraphPool};
use renderer_pipeline::pbr_forward_plus::{
    TILE_INITIALIZATION_CAPACITY, TileInit, append_tile_initialization,
};

const CAMERA: BufferDesc = BufferDesc { size: 64 };
const GRAPH: GraphCapacity = GraphCapacity {
    passes: TILE_INITIALIZATION_CAPACITY.passes + 2,
    accesses: TILE_INITIALIZATION_CAPACITY.accesses + 3,
    upload_bytes: TILE_INITIALIZATION_CAPACITY.upload_bytes + CAMERA.size as usize,
    name_bytes: TILE_INITIALIZATION_CAPACITY.name_bytes + 64,
    ..TILE_INITIALIZATION_CAPACITY
};

fn camera(graph: &mut Graph) -> BufferHandle {
    let camera = graph.buffer("camera", CAMERA).unwrap();
    let mut bytes = [0; CAMERA.size as usize];
    for word in [0, 5, 10, 15] {
        let offset = word * 4;
        bytes[offset..offset + 4].copy_from_slice(&1.0_f32.to_le_bytes());
    }
    graph
        .upload_buffer("camera upload", camera, 0, &bytes)
        .unwrap();
    camera
}

fn initialize(graph: &mut Graph) -> TileInit {
    let camera = camera(graph);
    append_tile_initialization(graph, camera).unwrap()
}

#[test]
fn tile_startup_establishes_the_full_storage_range() {
    let mut pool = GraphPool::new(GRAPH, 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let _init = initialize(&mut graph);
    assert_eq!(
        graph
            .compile()
            .unwrap()
            .ordered_pass_names()
            .collect::<Vec<_>>(),
        [
            "camera upload",
            "local point light update",
            "Forward+ tile light lists"
        ],
    );
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod native {
    use super::*;
    use renderer::{
        capacity::{ResourceClass, WorkingSet},
        runtime::{ExecutionErrorKind, Runtime, RuntimeConfig, RuntimeLimits},
    };
    use renderer_pipeline::pbr_forward_plus::{
        LIGHT_BUFFER_BYTES, PointLight, TILE_BUFFER_BYTES, TileInitError, append_culling,
        compute_program, light_bytes,
    };
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(30);
    const LIGHTS: BufferDesc = BufferDesc {
        size: LIGHT_BUFFER_BYTES,
    };
    const TILES: BufferDesc = BufferDesc {
        size: TILE_BUFFER_BYTES,
    };

    fn test_tile_registration(config: RuntimeConfig) {
        let mut runtime = Runtime::new(
            config,
            RuntimeLimits {
                graph: GRAPH,
                frames: 3,
                results: 3,
                ..RuntimeLimits::default()
            },
            WorkingSet {
                resources: &[
                    ResourceClass::Buffer {
                        desc: CAMERA,
                        count: 2,
                    },
                    ResourceClass::Buffer {
                        desc: LIGHTS,
                        count: 2,
                    },
                    ResourceClass::Buffer {
                        desc: TILES,
                        count: 1,
                    },
                ],
                raster: &[],
                compute: &[compute_program()],
            },
        )
        .unwrap();
        #[cfg(feature = "vulkan")]
        if let Some(vulkan) = runtime.as_vulkan() {
            assert_ne!(vulkan.device_info().device_type, "CPU");
        }
        let mut pool = GraphPool::new(GRAPH, 3).unwrap();
        let mut graph = pool.begin().unwrap();
        let mut init = initialize(&mut graph);
        let ready = runtime.submit(&graph.compile().unwrap()).unwrap();
        let tiles = init.accept(&ready).unwrap();
        assert!(matches!(
            init.accept(&ready),
            Err(TileInitError::AlreadyAccepted)
        ));
        let initial = tiles.snapshot(&runtime).unwrap();

        let mut graph = pool.begin().unwrap();
        let camera = camera(&mut graph);
        let lights = graph.buffer("lights", LIGHTS).unwrap();
        let point = PointLight {
            position: [0.0, 0.0, 0.5],
            radius: 1.0,
            color: [1.0; 3],
            intensity: 1.0,
        };
        let bytes = light_bytes(16, 16, &[point]).unwrap();
        graph
            .upload_buffer("light upload", lights, 0, &bytes)
            .unwrap();
        let target = graph.import_buffer("tiles", &initial).unwrap();
        append_culling(&mut graph, 16, 16, camera, lights, target).unwrap();
        graph.readback_buffer("updated tile", target, 0, 4).unwrap();
        let updated = runtime.submit(&graph.compile().unwrap()).unwrap();

        let mut stale_graph = pool.begin().unwrap();
        let stale = stale_graph.import_buffer("stale tiles", &initial).unwrap();
        stale_graph
            .readback_buffer("stale prefix", stale, 0, 4)
            .unwrap();
        let error = runtime
            .submit(&stale_graph.compile().unwrap())
            .err()
            .unwrap();
        assert_eq!(error.kind(), ExecutionErrorKind::StaleResource);

        let latest = tiles.snapshot(&runtime).unwrap();
        let mut graph = pool.begin().unwrap();
        let target = graph.import_buffer("updated tiles", &latest).unwrap();
        let head = graph.readback_buffer("new prefix", target, 0, 4).unwrap();
        let tail = graph
            .readback_buffer("retained tail", target, TILE_BUFFER_BYTES - 4, 4)
            .unwrap();
        let result = runtime.submit(&graph.compile().unwrap()).unwrap();
        tiles.delete(&mut runtime).unwrap();
        drop(
            runtime
                .wait_for_frame(ready.id(), TIMEOUT)
                .unwrap()
                .unwrap(),
        );
        drop(
            runtime
                .wait_for_frame(updated.id(), TIMEOUT)
                .unwrap()
                .unwrap(),
        );
        let mut result = runtime
            .wait_for_frame(result.id(), TIMEOUT)
            .unwrap()
            .unwrap();
        let mut prefix = [0; 4];
        let mut suffix = [0xff; 4];
        result.readback_into(head, &mut prefix).unwrap();
        result.readback_into(tail, &mut suffix).unwrap();
        assert_eq!(u32::from_le_bytes(prefix), 1);
        assert_eq!(suffix, [0; 4]);
        drop(result);

        let strict = cfg!(feature = "development-validation");
        match config {
            #[cfg(feature = "vulkan")]
            RuntimeConfig::Vulkan(_) => {
                let monitor = runtime.as_vulkan().unwrap().validation_monitor();
                drop(runtime);
                let report = monitor.report();
                assert!(report.is_clean(), "{report:#?}");
                if strict {
                    assert_eq!(report.core_validation, Some(true));
                    assert_eq!(report.synchronization_validation, Some(true));
                }
            }
            #[cfg(all(feature = "d3d12", windows))]
            RuntimeConfig::D3D12(_) => {
                let monitor = runtime.as_d3d12().unwrap().validation_monitor();
                drop(runtime);
                let report = monitor.snapshot();
                assert!(report.is_clean(), "{report:#?}");
                assert_eq!(report.debug_layer_enabled, strict);
                assert_eq!(report.gpu_based_validation_enabled, strict);
            }
        }
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a real Vulkan device"]
    fn vulkan_imported_tile_write_updates_the_scheduled_contents() {
        test_tile_registration(RuntimeConfig::Vulkan(Default::default()));
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a real D3D12 device"]
    fn d3d12_imported_tile_write_updates_the_scheduled_contents() {
        use renderer::runtime::D3D12Config;
        test_tile_registration(RuntimeConfig::D3D12(D3D12Config {
            max_buffer_bytes: TILE_BUFFER_BYTES,
            ..D3D12Config::default()
        }));
    }
}

use renderer::graph::{GraphCapacity, GraphErrorKind, GraphPool};
use renderer_pipeline::pbr::{AlphaMode, MaterialFactors, MaterialUniform};

#[test]
fn cooked_material_rejects_invalid_modes_factors_and_padding() {
    let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
    let mut graph = pool.begin().unwrap();
    let encoded = MaterialFactors::default().encode().unwrap();
    for (offset, invalid) in [(28, f32::NAN.to_bits()), (48, 3), (52, 2), (60, 1)] {
        let mut bytes = encoded;
        bytes[offset..offset + 4].copy_from_slice(&invalid.to_le_bytes());
        assert_eq!(
            MaterialUniform::upload_encoded(&mut graph, "invalid material", &bytes)
                .unwrap_err()
                .kind(),
            GraphErrorKind::InvalidInput,
        );
    }
    let material = MaterialUniform::upload_encoded(&mut graph, "valid material", &encoded).unwrap();
    assert_eq!(material.alpha_mode(), AlphaMode::Opaque);
    material.export(&mut graph).unwrap();
    assert_eq!(graph.compile().unwrap().pass_count(), 1);
}

#[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
mod native {
    use super::*;
    use renderer::{
        capacity::Preparation,
        runtime::{Runtime, RuntimeConfig, RuntimeLimits},
    };
    use std::{num::NonZeroUsize, time::Duration};

    fn accept_and_retire(config: RuntimeConfig) {
        let limits = RuntimeLimits::default();
        let mut preparation = Preparation::new(NonZeroUsize::MIN);
        preparation
            .reserve_buffer(MaterialUniform::DESC, 1)
            .unwrap();
        let mut runtime = Runtime::new(config, limits, preparation.working_set()).unwrap();
        #[cfg(feature = "vulkan")]
        if let Some(vulkan) = runtime.as_vulkan() {
            assert_ne!(vulkan.device_info().device_type, "CPU");
        }
        let mut pool = GraphPool::new(limits.graph, 3).unwrap();
        let mut graph = pool.begin().unwrap();
        let factors = MaterialFactors {
            alpha_mode: AlphaMode::Mask,
            ..MaterialFactors::default()
        };
        let uniform = MaterialUniform::upload(&mut graph, "material", factors).unwrap();
        uniform.export(&mut graph).unwrap();
        let submitted = runtime.submit(&graph.compile().unwrap()).unwrap();
        let mut resident = uniform.accept(&submitted).unwrap();
        let startup = submitted.id();
        drop(submitted);

        let mut graph = pool.begin().unwrap();
        assert_eq!(resident.alpha_mode(), AlphaMode::Mask);
        let imported = resident.import(&mut graph, "material").unwrap();
        let readback = imported.readback(&mut graph, "factors").unwrap();
        let submitted = runtime.submit(&graph.compile().unwrap()).unwrap();
        assert!(uniform.accept(&submitted).is_err());
        resident.release(&mut runtime).unwrap();
        let mut graph = pool.begin().unwrap();
        assert!(resident.import(&mut graph, "released material").is_err());
        drop(graph);

        let timeout = Duration::from_secs(10);
        drop(runtime.wait_for_frame(startup, timeout).unwrap().unwrap());
        let mut completed = runtime
            .wait_for_frame(submitted.id(), timeout)
            .unwrap()
            .unwrap();
        let expected = factors.encode().unwrap();
        let mut bytes = [0; renderer_pipeline::passes::pbr::MATERIAL_BYTES];
        completed.readback_into(readback, &mut bytes).unwrap();
        assert_eq!(bytes, expected);
        drop((completed, submitted));

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

    #[test]
    #[ignore = "requires real Vulkan or D3D12 devices"]
    fn material_factors_survive_unregistration_while_a_consumer_is_pending() {
        #[cfg(feature = "vulkan")]
        accept_and_retire(RuntimeConfig::Vulkan(Default::default()));
        #[cfg(all(feature = "d3d12", windows))]
        accept_and_retire(RuntimeConfig::D3D12(Default::default()));
    }
}

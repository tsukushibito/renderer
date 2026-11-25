use sashiko_renderer::{
    material::{Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{LocalLight, PbrVertex, Projection, Transform, ViewInput},
    pipeline::{
        CameraGroup, CameraView, PipelineResult, RenderObject, RendererSetup, SceneAssets,
        StandardConfig, Technique, ViewId,
    },
    unlit_forward::IDENTITY,
};
use sashiko_renderer_core::{graph::ImageDesc, runtime::RuntimeLimits};

fn camera(id: u64) -> PipelineResult<CameraView> {
    Ok(CameraView {
        id: ViewId(id),
        extent: [64, 32],
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    })
}

#[test]
fn plan_counts_shared_assets_and_per_view_work_separately() -> PipelineResult<()> {
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let mut materials = Materials::default();
    let vertex = PbrVertex::new([0.; 3], [0., 0., 1.], [1., 0., 0., 1.], [0.; 2])?;
    let mesh = meshes.create(
        MeshData::pbr(&[vertex; 3], &[0, 1, 2])?.with_lightmap_uvs(&[
            [0., 0.],
            [1., 0.],
            [0., 1.],
        ])?,
    )?;
    let mat = materials.create_pbr(PbrParams::color([1.; 4]))?;
    let model = Transform::new([[1., 0., 0., 1.], [0., 1., 0., 0.], [0., 0., 1., 0.]])?;
    let instance = RenderObject::new(mesh, mat).with_transform(model);
    let objects = [
        instance.with_lightmap(12),
        RenderObject {
            casts_shadow: false,
            ..instance
        },
    ];
    let mut lights = [LocalLight::Point(
        sashiko_renderer::pbr::PointLight::from_candela([0.; 3], 1., [1.; 3], 1.).unwrap(),
    ); 257];
    lights[0] = LocalLight::shadowed_rect(sashiko_renderer::pbr::RectLight::from_lumens(
        [0.; 3],
        [1., 0., 0.],
        [0., 1., 0.],
        [1.; 3],
        1.,
        5.,
    )?);
    let mut group = CameraGroup::standard(
        camera(1)?,
        Technique::ForwardPlus,
        StandardConfig::new([64, 32]),
    );
    group.add_view(camera(2)?)?;
    let mut setup = RendererSetup::new(group);
    setup.additional_objects = 3;
    setup.additional_lights = 2;
    setup.additional_shadowed_rect_lights = 1;
    let mut plan = setup
        .prepare(SceneAssets::new(&mut meshes, &mut materials, &objects).with_lights(&lights))?;
    let limits = plan.scene_limits();
    assert_eq!(limits.max_lightmapped_draws, 4);
    assert_eq!(limits.max_shadowed_rect_lights, 2);
    assert_eq!(
        (
            limits.max_draws,
            limits.max_materials,
            limits.max_transforms,
            limits.max_shadow_casters,
            limits.max_local_lights
        ),
        (5, 4, 5, 4, 260)
    );
    let before = plan.preparation().graph_requirements();
    let texture = ImageDesc::rgba8(1, 4096);
    plan.reserve_texture("albedo", texture)?;
    plan.reserve_readback("pixels", texture)?;
    assert_eq!(
        plan.preparation().graph_requirements().upload_bytes - before.upload_bytes,
        4 * 4096
    );
    assert!(
        plan.runtime_limits(RuntimeLimits::default())?
            .graph
            .upload_bytes
            >= 4 * 4096
    );
    #[cfg(all(windows, feature = "d3d12"))]
    {
        let mut backend = sashiko_renderer_core::runtime::RuntimeConfig::D3D12(
            sashiko_renderer_core::runtime::D3D12Config {
                upload_bytes: 1,
                readback_bytes: 1,
                ..Default::default()
            },
        );
        plan.fit_backend(&mut backend)?;
        assert!(
            matches!(backend, sashiko_renderer_core::runtime::RuntimeConfig::D3D12(config)
            if config.upload_bytes >= 256 * 4096 && config.readback_bytes >= 256 * 4096)
        );
    }
    Ok(())
}

#[test]
fn empty_scene_and_invalid_cold_inputs_need_no_device() -> PipelineResult<()> {
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let mut materials = Materials::default();
    let c = camera(1)?;
    let mut plan =
        RendererSetup::standard(c).prepare(SceneAssets::new(&mut meshes, &mut materials, &[]))?;
    assert_eq!(plan.scene_limits().max_draws, 0);
    assert!(
        plan.reserve_texture("depth", ImageDesc::depth32(2, 2))
            .is_err()
    );
    assert!(
        plan.reserve_readback(
            "mips",
            ImageDesc {
                mip_levels: 2,
                ..ImageDesc::rgba8(8, 8)
            }
        )
        .is_err()
    );
    drop(plan);
    let mut duplicate = RendererSetup::standard(c);
    duplicate.add_group(CameraGroup::standard(
        c,
        Technique::Deferred,
        StandardConfig::new(c.extent),
    ));
    assert!(
        duplicate
            .prepare(SceneAssets::new(&mut meshes, &mut materials, &[]))
            .is_err()
    );
    let mut tiny = RendererSetup::standard(c);
    tiny.mesh_upload_bytes = 3;
    assert!(
        tiny.prepare(SceneAssets::new(&mut meshes, &mut materials, &[]))
            .is_err()
    );
    Ok(())
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod candidate_gpu {
    use super::*;
    use sashiko_renderer::pipeline::{
        AssetCandidate, AssetPreparation, RetireAssets, SceneInput, UploadBudget,
    };
    use sashiko_renderer_core::runtime::RuntimeConfig;
    use std::{
        num::NonZeroUsize,
        time::{Duration, Instant},
    };

    mod validation {
        include!("../examples/support/validation.rs");
    }

    fn exercise(backend: RuntimeConfig) -> PipelineResult<()> {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 3,
            retired_mesh_versions: 1,
        })?;
        let mut materials = Materials::default();
        let v = PbrVertex::new([0.; 3], [0., 0., 1.], [1., 0., 0., 1.], [0.; 2])?;
        let mesh = || MeshData::pbr(&[v; 3], &[0, 1, 2]);
        let old = RenderObject::new(
            meshes.create(mesh()?)?,
            materials.create_pbr(PbrParams::color([1.; 4]))?,
        );
        let mut setup = RendererSetup::standard(camera(9)?);
        setup.frames_in_flight = NonZeroUsize::new(2).unwrap();
        let objects = [old];
        let mut plan = setup.prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
        plan.reserve_mesh_replacement(old.mesh)?;
        plan.preparation_mut()
            .reserve_buffer(sashiko_renderer::pbr::MaterialUniform::DESC, 1)?;
        let mut renderer = plan.build(backend)?;
        let group = renderer.parts_mut().0.pipeline_for_view(ViewId(9))?;
        let new = RenderObject::new(
            meshes.create(mesh()?)?,
            materials.create_pbr(PbrParams::color([0.5; 4]))?,
        );
        let mut change = AssetCandidate::new(&meshes, &materials, &[new])?;
        let deadline = Instant::now() + Duration::from_secs(30);
        let budget = UploadBudget {
            bytes: 64,
            submissions: 1,
        };
        loop {
            let state =
                renderer.poll_candidate(&mut change, &mut meshes, &mut materials, budget)?;
            let frame = renderer.begin_frame()?;
            assert!(
                frame
                    .bind_scene(group, SceneInput::new(&meshes, &materials, &[old]))
                    .is_ok()
            );
            assert!(
                frame
                    .bind_scene(group, SceneInput::new(&meshes, &materials, &[new]))
                    .is_err()
            );
            if state.is_ready() {
                break;
            }
            assert!(Instant::now() < deadline, "equipment preparation timed out");
            std::thread::yield_now();
        }
        // A full retire queue must fail atomically even though the candidate is ready.
        let spare = meshes.create(mesh()?)?;
        meshes.remove(spare)?;
        let retire = RetireAssets {
            meshes: &[old.mesh],
            materials: &[old.material],
            pairs: &[(old.mesh, old.material)],
        };
        assert!(
            renderer
                .commit_candidate(&mut change, &mut meshes, &mut materials, retire)
                .is_err()
        );
        assert!(
            renderer
                .begin_frame()?
                .bind_scene(group, SceneInput::new(&meshes, &materials, &[old]))
                .is_ok()
        );
        renderer.collect_assets(&mut meshes, &mut materials)?;
        renderer.commit_candidate(&mut change, &mut meshes, &mut materials, retire)?;
        assert!(
            renderer
                .begin_frame()?
                .bind_scene(group, SceneInput::new(&meshes, &materials, &[new]))
                .is_ok()
        );
        assert!(materials.get(old.material).is_err());
        renderer.collect_assets(&mut meshes, &mut materials)?;
        let abandoned = RenderObject::new(meshes.create(mesh()?)?, new.material);
        let mut pending = AssetCandidate::new(&meshes, &materials, &[abandoned])?;
        assert!(
            renderer
                .poll_candidate(
                    &mut pending,
                    &mut meshes,
                    &mut materials,
                    UploadBudget {
                        bytes: 4,
                        submissions: 1
                    },
                )?
                .is_pending()
        );
        renderer.cancel_candidate(
            &mut pending,
            &mut meshes,
            &mut materials,
            RetireAssets {
                meshes: &[abandoned.mesh],
                ..Default::default()
            },
        )?;
        while renderer.collect_assets(&mut meshes, &mut materials)? != 0 {
            assert!(
                Instant::now() < deadline,
                "cancelled transfer did not retire"
            );
            std::thread::yield_now();
        }
        assert!(
            renderer
                .begin_frame()?
                .bind_scene(group, SceneInput::new(&meshes, &materials, &[new]))
                .is_ok()
        );
        let mut runtime = renderer.into_runtime()?;
        meshes.release_gpu(&mut runtime)?;
        materials.clear(&mut runtime)?;
        validation::check_validation(runtime)
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a real Vulkan device"]
    fn vulkan_switches_ready_asset_after_retirement_is_reserved() -> PipelineResult<()> {
        exercise(RuntimeConfig::Vulkan(Default::default()))
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a real D3D12 device"]
    fn d3d12_switches_ready_asset_after_retirement_is_reserved() -> PipelineResult<()> {
        exercise(RuntimeConfig::D3D12(Default::default()))
    }

    fn shared_uploads(mut backend: RuntimeConfig) -> PipelineResult<()> {
        use sashiko_renderer_core::runtime::{Runtime, RuntimeLimits};
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 2,
            retired_mesh_versions: 1,
        })?;
        let mut materials = Materials::default();
        let v = PbrVertex::new([0.; 3], [0., 0., 1.], [1., 0., 0., 1.], [0.; 2])?;
        let triangle = || MeshData::pbr(&[v; 3], &[0, 1, 2]);
        let old = RenderObject::new(
            meshes.create(triangle()?)?,
            materials.create_pbr(PbrParams::color([1.; 4]))?,
        );
        let mut setup = RendererSetup::standard(camera(1)?);
        setup.frames_in_flight = NonZeroUsize::MIN;
        let initial = [old];
        let mut plan = setup.prepare(SceneAssets::new(&mut meshes, &mut materials, &initial))?;
        plan.reserve_mesh_replacement(old.mesh)?;
        plan.preparation_mut()
            .reserve_buffer(sashiko_renderer::pbr::MaterialUniform::DESC, 1)?;
        plan.fit_backend(&mut backend)?;
        let limits = plan.runtime_limits(RuntimeLimits {
            frames: 1,
            ..Default::default()
        })?;
        let runtime = Runtime::new(backend, limits, plan.preparation().working_set())?;
        let mut renderer = plan.begin_with_runtime(runtime)?.wait()?;
        let next = RenderObject::new(
            meshes.create(triangle()?)?,
            materials.create_pbr(PbrParams::color([0.5; 4]))?,
        );
        let budget = UploadBudget {
            bytes: 4096,
            submissions: 2,
        };
        let mut producer = AssetCandidate::new(&meshes, &materials, &[next])?;
        // With one execution slot the mesh is accepted, then the uniform upload fails.
        let error = renderer
            .poll_candidate(&mut producer, &mut meshes, &mut materials, budget)
            .expect_err("the second submission must hit backpressure");
        assert!(error.capacity().is_some(), "{error}");
        assert_eq!(renderer.runtime().pending_frames(), 1);
        renderer.cancel_candidate(
            &mut producer,
            &mut meshes,
            &mut materials,
            RetireAssets::default(),
        )?;
        let deadline = Instant::now() + Duration::from_secs(30);
        while renderer.collect_assets(&mut meshes, &mut materials)? != 0 {
            assert!(Instant::now() < deadline);
        }
        let mut first_user = AssetCandidate::new(&meshes, &materials, &[next])?;
        assert!(
            renderer
                .poll_candidate(&mut first_user, &mut meshes, &mut materials, budget)?
                .is_pending()
        );
        // Duplicating the resident material shares the same in-flight uniform revision.
        let alias = RenderObject::new(next.mesh, materials.duplicate(next.material)?);
        let mut second_user = AssetCandidate::new(&meshes, &materials, &[alias])?;
        while renderer
            .poll_candidate(&mut second_user, &mut meshes, &mut materials, budget)?
            .is_pending()
        {
            assert!(Instant::now() < deadline);
        }
        assert_eq!(renderer.runtime().pending_frames(), 0);

        // A ready consumer must recheck if a different producer re-uploads its
        // unchanged mesh revision between Ready and commit.
        meshes.release_gpu(renderer.runtime())?;
        let mut reload = AssetCandidate::new(&meshes, &materials, &[next])?;
        assert!(
            renderer
                .poll_candidate(&mut reload, &mut meshes, &mut materials, budget)?
                .is_pending()
        );
        assert!(
            renderer
                .commit_candidate(
                    &mut second_user,
                    &mut meshes,
                    &mut materials,
                    RetireAssets::default()
                )
                .is_err()
        );
        while renderer
            .poll_candidate(&mut second_user, &mut meshes, &mut materials, budget)?
            .is_pending()
        {
            assert!(Instant::now() < deadline);
        }
        renderer.commit_candidate(
            &mut second_user,
            &mut meshes,
            &mut materials,
            RetireAssets::default(),
        )?;
        let mut runtime = renderer.into_runtime()?;
        meshes.release_gpu(&mut runtime)?;
        materials.clear(&mut runtime)?;
        validation::check_validation(runtime)
    }

    #[cfg(feature = "vulkan")]
    #[test]
    #[ignore = "requires a real Vulkan device"]
    fn vulkan_shared_candidate_waits_for_the_producing_submission() -> PipelineResult<()> {
        shared_uploads(RuntimeConfig::Vulkan(Default::default()))
    }

    #[cfg(all(windows, feature = "d3d12"))]
    #[test]
    #[ignore = "requires a real D3D12 device"]
    fn d3d12_shared_candidate_waits_for_the_producing_submission() -> PipelineResult<()> {
        shared_uploads(RuntimeConfig::D3D12(Default::default()))
    }
}

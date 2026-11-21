use renderer::{graph::ImageDesc, runtime::RuntimeLimits};
use renderer_pipeline::{
    material::{Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{PbrVertex, PointLight, Projection, Transform, ViewInput},
    pipeline::{
        CameraGroup, CameraView, PipelineResult, RenderObject, RendererSetup, SceneAssets,
        StandardConfig, Technique, ViewId,
    },
    unlit_forward::IDENTITY,
};

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
    let mesh = meshes.create(MeshData::pbr(&[vertex; 3], &[0, 1, 2])?)?;
    let mat = materials.create_pbr(PbrParams::color([1.; 4]))?;
    let model = Transform::new([[1., 0., 0., 1.], [0., 1., 0., 0.], [0., 0., 1., 0.]])?;
    let instance = RenderObject::new(mesh, mat).with_transform(model);
    let objects = [
        instance,
        RenderObject {
            casts_shadow: false,
            ..instance
        },
    ];
    let lights = [PointLight {
        position: [0.; 3],
        radius: 1.,
        color: [1.; 3],
        intensity: 1.,
    }; 257];
    let mut group = CameraGroup::standard(
        camera(1)?,
        Technique::ForwardPlus,
        StandardConfig::new([64, 32]),
    );
    group.add_view(camera(2)?)?;
    let mut setup = RendererSetup::new(group);
    setup.additional_objects = 3;
    setup.additional_lights = 2;
    let mut plan = setup
        .prepare(SceneAssets::new(&mut meshes, &mut materials, &objects).with_lights(&lights))?;
    let limits = plan.scene_limits();
    assert_eq!(
        (
            limits.max_draws,
            limits.max_materials,
            limits.max_transforms,
            limits.max_casters_per_cascade,
            limits.max_point_lights
        ),
        (5, 4, 5, 4, 259)
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
        let mut backend = renderer::runtime::RuntimeConfig::D3D12(renderer::runtime::D3D12Config {
            upload_bytes: 1,
            readback_bytes: 1,
            ..Default::default()
        });
        plan.fit_backend(&mut backend)?;
        assert!(
            matches!(backend, renderer::runtime::RuntimeConfig::D3D12(config)
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

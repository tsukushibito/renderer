//! Compare individual and instanced PBR draws in two views of the same posed meshes.
use super::*;
use sashiko_renderer::{
    core as sashiko_renderer_core,
    material::{MaterialInstance, Materials, PbrParams},
    mesh::{MeshCapacity, Meshes},
    pbr::{self, Defaults, Draw, IndexedDraw, MaterialFactors, PbrDrawInput, Transform},
    pipeline::*,
    unlit_forward::IDENTITY,
};
use sashiko_renderer_core::{
    capacity::Preparation,
    graph::{Graph, GraphError, ImageDesc, SampleCount},
    runtime::{FrameId, Runtime, RuntimeConfig},
};
use std::{num::NonZeroUsize, time::Duration};
mod validation {
    use super::sashiko_renderer_core;
    include!("../../../../crates/sashiko-renderer/examples/support/validation.rs");
}

const SIDE: u32 = 96;
const COUNT: usize = 16;
struct CrowdView {
    recipe: GeometryPipeline,
    draws: Vec<Draw>,
    defaults: pbr::PreparedDefaults,
    resident: Option<pbr::DefaultResources>,
}
impl ViewPipeline for CrowdView {
    type State = ();
    type Update = ();
    fn create_view(&self, _: &CameraView) -> PipelineResult<()> {
        Ok(())
    }
    fn check_object(&self, scene: SceneInput<'_>, object: &RenderObject) -> Result<(), GraphError> {
        scene.meshes.check_layout(
            object.mesh,
            &sashiko_renderer::passes::pbr::opaque_program(),
        )
    }
    fn reserve(
        &self,
        prep: &mut Preparation,
        _: [u32; 2],
        views: NonZeroUsize,
    ) -> Result<(), GraphError> {
        Defaults::Materials.prepare(prep)?;
        for _ in 0..views.get() {
            let recipe = self.recipe.preparation();
            prep.add_requirements(recipe.graph_requirements(), recipe.working_set())?;
            prep.reserve_buffer(pbr::MaterialUniform::DESC, 1)?;
            Meshes::reserve_imports(prep, COUNT)?;
        }
        Ok(())
    }
    fn initialize(&mut self, runtime: &mut Runtime) -> PipelineResult<Option<FrameId>> {
        let (resources, frame) = self.defaults.initialize(runtime)?;
        self.resident = Some(resources);
        Ok(Some(frame))
    }
    fn release(&mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        if let Some(mut defaults) = self.resident.take() {
            defaults.clear(runtime)?;
        }
        Ok(())
    }
    fn record(
        &mut self,
        graph: &mut Graph,
        _: &mut (),
        camera: &CameraView,
        scene: PreparedScene<'_, Self>,
    ) -> Result<(ViewImages, ()), GraphError> {
        let scene = scene.input();
        let defaults = self.resident.as_ref().unwrap().import(graph)?;
        let view = self.recipe.prepare_view(
            camera.camera,
            scene.sun,
            scene.bounds,
            defaults.environment()?,
        )?;
        let MaterialInstance::Pbr(material) = scene.materials.get(scene.objects[0].material)?
        else {
            unreachable!()
        };
        let materials = [material.resolve(graph, "figure material", Some(defaults))?];
        let transforms: [_; COUNT] = std::array::from_fn(|i| scene.objects[i].transform);
        let draws = &mut self.draws;
        draws.clear();
        // The reference camera submits each instance separately. The other camera
        // consumes the same transforms in contiguous groups of equal skin pose.
        let mut first = 0;
        while first < COUNT {
            let obj = &scene.objects[first];
            let count = if camera.id == ViewId(0) {
                1
            } else {
                scene.objects[first..]
                    .iter()
                    .take_while(|o| o.pose == obj.pose && o.mesh == obj.mesh)
                    .count()
            };
            let mesh = scene.import_mesh(graph, obj)?;
            draws.push(Draw::Instanced {
                geometry: IndexedDraw {
                    vertices: mesh.vertices,
                    indices: mesh.indices,
                    first_index: mesh.first_index,
                    index_count: mesh.index_count,
                    material: 0,
                },
                first_transform: first as u32,
                instance_count: count as u32,
            });
            first += count;
        }
        let phases = scene.objects.chunk_by(|a, b| a.pose == b.pose).count();
        assert_eq!(
            draws.len(),
            if camera.id == ViewId(0) {
                COUNT
            } else {
                phases
            }
        );
        let mut input = PbrDrawInput::new(&view, &materials, draws, &[], draws, defaults)?;
        input.transforms = &transforms;
        let images = self.recipe.draw_hdr(graph, input)?;
        assert!(images.shadows.is_some());
        Ok((
            ViewImages {
                color: images.hdr,
                depth: images.depth,
            },
            (),
        ))
    }
}
fn frames(api: RuntimeConfig, compute: bool) -> PipelineResult<Vec<Vec<u8>>> {
    let catalog = catalog()?;
    let model = catalog.load(catalog.id("figure").unwrap())?;
    let mut palettes = model.prepare_palette_batch(&[], 1, 1)?;
    let jobs = JobSystem::inline().into();
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 0,
    })?;
    let data = model.model().mesh_data()?;
    let mesh = meshes.create(if compute {
        data.with_compute_skin()?
    } else {
        data
    })?;
    let mut materials = Materials::default();
    let mat = materials.create_pbr(PbrParams {
        factors: MaterialFactors {
            emissive_nits: [1., 0.1, 0.],
            double_sided: true,
            ..Default::default()
        },
        ..Default::default()
    })?;
    let mut actors = Vec::new();
    for offset in 0..4 {
        let mut actor = character(&catalog, Some(&model))?;
        let mut root_x = 0.;
        for tick in 1..=offset {
            root_x += actor
                .advance(tick, 4., requests(tick), &mut [EMPTY; 8])?
                .translation[0];
        }
        actors.push((actor, meshes.create_pose(mesh)?, root_x));
    }
    let mut local = LocalBuffer::new(actors[0].0.locomotion().skeleton().unwrap());
    let mut objects = [RenderObject::new(mesh, mat); COUNT];
    for (i, object) in objects.iter_mut().enumerate() {
        object.pose = Some(actors[i / 4].1);
    }
    let camera = CameraView {
        id: ViewId(0),
        extent: [SIDE; 2],
        camera: ViewInput::new(
            IDENTITY,
            [0., 0., -1.],
            [0., 0., 1.],
            [1., 2.],
            Projection::Orthographic,
        )?,
    };
    let other = CameraView {
        id: ViewId(1),
        ..camera
    };
    let mut group = CameraGroup::custom(camera, |extent, limits, _| {
        Ok(CrowdView {
            draws: Vec::with_capacity(COUNT),
            resident: None,
            defaults: Defaults::Materials
                .prepare(&mut Preparation::new(limits.frames_in_flight))?,
            recipe: GeometryPipeline::new(
                Technique::Forward,
                pbr::Config {
                    extent,
                    clear_color: [0., 0., 0., 1.],
                    shadows: Some(pbr::ShadowSettings {
                        resolution: 32,
                        ..Default::default()
                    }),
                    output: None,
                    samples: SampleCount::X1,
                    camera_storage: pbr::CameraStorage::Transient,
                },
                limits,
            )?,
        })
    });
    group.add_view(other)?;
    let mut setup = RendererSetup::new(group).prepare(SceneAssets::new(
        &mut meshes,
        &mut materials,
        &objects,
    ))?;
    for _ in 0..2 {
        setup.reserve_readback("camera", ImageDesc::rgba8(SIDE, SIDE))?;
    }
    let mut build = setup.begin_build(api)?;
    while build.poll()?.is_pending() {
        std::thread::yield_now();
    }
    let mut renderer = build.finish()?;
    let mut captured = vec![];
    // A single shared pose, then four independent phases through the interrupt and recovery.
    for frame_index in 0..10 {
        let phases = if frame_index == 0 { 1 } else { 4 };
        for (actor, pose, root_x) in &mut actors {
            let tick = actor.tick() + 1;
            *root_x += actor
                .advance(tick, 4., requests(tick), &mut [EMPTY; 8])?
                .translation[0];
            actor.compose(&mut local)?;
            let palette = palettes
                .evaluate(
                    &[PaletteRequest::Sample(0)],
                    &[local.sample()],
                    rate(),
                    &jobs,
                )?
                .next()
                .unwrap();
            meshes.set_joint_transforms(*pose, palette)?;
        }
        for (i, obj) in objects.iter_mut().enumerate() {
            let (_, pose, root_x) = &actors[i * phases / COUNT];
            obj.pose = Some(*pose);
            let x = (i % 4) as f32 * 0.5 - 0.80 + *root_x as f32 * 0.12;
            let y = (i / 4) as f32 * 0.5 - 0.75;
            obj.transform =
                Transform::new([[0.28, 0., 0., x], [0., 0.28, 0., y], [0., 0., 1., 0.]])?;
        }
        let scene = SceneInput::new(&meshes, &materials, &objects)
            .with_bounds(Bounds::new([-2.; 3], [2.; 3])?)
            .with_sun(pbr::Sun::from_lux([0.4, 1., -0.3], [1.; 3], 2.)?);
        let mut frame = renderer.begin_frame()?;
        let mut reads = Vec::new();
        for v in [&camera, &other] {
            let hdr = frame.draw_scene(v, scene)?.color;
            let image = frame.write_output(v.id, hdr, 1., None)?.raw_image();
            reads.push(frame.graph().readback_image("camera", image)?);
        }
        let plan = frame.compile()?;
        assert_eq!(
            plan.ordered_pass_names()
                .filter(|&n| n == "compute skin vertices")
                .count(),
            if compute { phases } else { 0 }
        );
        let receipt = plan.submit()?;
        let mut result = renderer
            .runtime()
            .wait_for_frame(receipt.id(), Duration::from_secs(10))?
            .ok_or("crowd timeout")?;
        let mut images = vec![];
        for read in reads {
            let mut pixels = vec![0; (SIDE * SIDE * 4) as usize];
            result.readback_into(read, &mut pixels)?;
            // Each of the sixteen world-space placements must actually contribute fragments.
            for tile in 0..16 {
                let lit = (0..24).any(|row| {
                    (0..24).any(|col| {
                        let index =
                            ((tile / 4 * 24 + row) * SIDE as usize + tile % 4 * 24 + col) * 4;
                        pixels[index].saturating_sub(pixels[index + 2]) > 10
                    })
                });
                assert!(lit, "empty cell {tile}");
            }
            images.push(pixels);
        }
        assert_eq!(
            images[0], images[1],
            "instancing changed frame {frame_index}"
        );
        captured.push(images.pop().unwrap());
    }
    assert!(captured.windows(2).any(|w| w[0] != w[1]));
    let mut runtime = renderer.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    validation::check_validation(runtime)?;
    Ok(captured)
}
fn check(api: RuntimeConfig) -> PipelineResult<()> {
    let cpu = frames(api, false)?;
    let compute = frames(api, true)?;
    assert!(
        cpu.iter()
            .flatten()
            .zip(compute.iter().flatten())
            .all(|(a, b)| a.abs_diff(*b) <= 1)
    );
    Ok(())
}
#[cfg(feature = "vulkan")]
#[test]
#[ignore = "requires real Vulkan hardware"]
fn vulkan_shared_skin() -> PipelineResult<()> {
    check(RuntimeConfig::Vulkan(Default::default()))
}
#[cfg(feature = "d3d12")]
#[test]
#[ignore = "requires real D3D12 hardware"]
fn d3d12_shared_skin() -> PipelineResult<()> {
    check(RuntimeConfig::D3D12(Default::default()))
}

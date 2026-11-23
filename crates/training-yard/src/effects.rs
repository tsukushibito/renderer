//! A glass pane, an opaque barrier and a two-frame tracer in the firing lane.
use renderer::{
    canvas::{Layer, Paint},
    core::{graph::RasterRect, runtime::Runtime},
    material::{AlphaMode, Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{PbrVertex, Transform},
    pipeline::{PipelineResult, RenderObject, SceneAssets, SceneInput},
};

pub(crate) const FRAMES: usize = 5;
pub(crate) fn reduced(step: usize) -> bool {
    matches!(step, 2 | 3)
}
pub(crate) const HUD: Layer = Layer {
    paint: Paint::Color([1.; 4]),
    rect: RasterRect::new(1, 1, 1, 1),
    clip: None,
};

pub(crate) struct FiringLane {
    meshes: Meshes,
    materials: Materials,
    objects: [RenderObject; 3],
}

/// The host keeps its device and records the lane plus its UI into a single Graph.
pub(crate) fn host(mut runtime: Runtime) -> PipelineResult<Runtime> {
    use renderer::{
        canvas,
        core::{
            ImageFormat,
            graph::{GraphPool, ImageDesc, LoadOp, SampleCount},
        },
        passes::output::{self, SdrOutput},
        pipeline::{CameraGroup, RendererSetup, StandardConfig, Technique},
    };
    use std::time::Duration;
    let full = crate::camera(1, [48, 24])?;
    let small = crate::camera(2, [24, 12])?;
    let group = |camera: renderer::pipeline::CameraView| {
        let mut group = CameraGroup::standard(
            camera,
            Technique::Forward,
            StandardConfig::new(camera.extent),
        );
        group.output = None;
        group
    };
    let mut setup = RendererSetup::new(group(full));
    setup.add_group(group(small));
    let mut lane = FiringLane::new()?;
    let mut plan = setup.prepare(lane.assets())?;
    let output = ImageDesc::rgba8(48, 24);
    plan.reserve_readback("lane capture", output)?;
    let prep = plan.preparation_mut();
    for cam in [full, small] {
        let [w, h] = cam.extent;
        prep.reserve_transient_image("tone mapped", ImageDesc::rgba16_float(w, h))?;
    }
    prep.reserve_raster_pass("exposure", SampleCount::X1, |p| {
        p.attachment()?;
        p.draws(&output::tone_map_neutral_pipeline().program, 1)
    })?;
    prep.reserve_raster_pass("canvas", SampleCount::X1, |p| {
        p.transient_attachment("host viewport", ImageDesc::rgba16_float(48, 24))?;
        for program in canvas::programs(ImageFormat::Rgba16Float)? {
            p.draws(&program, 1)?;
        }
        Ok(())
    })?;
    let encoding = SdrOutput::capture(false);
    prep.reserve_raster_pass("encode", SampleCount::X1, |p| {
        p.transient_attachment("encoded", output)?;
        p.draws(&encoding.pipeline().program, 1)
    })?;
    plan.prepare_runtime(&mut runtime)?;
    let mut pool = GraphPool::new(runtime.limits().graph, 1)?;
    let mut drawing = plan.begin_with_runtime(runtime)?.wait()?;
    let mut pixels = [0; 48 * 24 * 4];
    for step in 0..FRAMES {
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        let before = renderer::core::measurement::Snapshot::now();
        let camera = if reduced(step) { small } else { full };
        let (mut views, runtime) = drawing.begin_host_frame(pool.begin()?)?;
        let hdr = views.draw_scene(&camera, lane.input(step)?)?.color;
        let (mut graph, mut histories) = views.finish_recording()?;
        let source = graph.image_view(hdr, 0, 0)?;
        let [w, h] = camera.extent;
        let linear = graph.image("tone mapped", ImageDesc::rgba16_float(w, h))?;
        output::tone_map_neutral(&mut graph, "exposure", source, linear, 1.)?;
        let layer = Layer {
            paint: Paint::Image(graph.image_view(linear, 0, 0)?),
            rect: RasterRect::new(0, 0, 48, 24),
            clip: None,
        };
        let viewport = graph.image("host viewport", ImageDesc::rgba16_float(48, 24))?;
        canvas::record(&mut graph, viewport, LoadOp::Clear([0.; 4]), &[layer, HUD])?;
        let source = graph.image_view(viewport, 0, 0)?;
        let encoded = graph.image("encoded", output)?;
        output::write_sdr(&mut graph, "encode", source, encoded, encoding)?;
        let read = graph.readback_image("lane capture", encoded)?;
        let ticket = runtime.submit(&graph.compile()?)?;
        histories.publish(&ticket)?;
        runtime
            .wait_for_frame(ticket.id(), Duration::from_secs(15))?
            .ok_or("lane timed out")?
            .readback_into(read, &mut pixels)?;
        drop(ticket);
        verify(&pixels, full.extent, camera.extent, step)?;
        #[cfg(all(feature = "memory-profiling", not(feature = "development-validation")))]
        assert!(
            renderer::core::measurement::Snapshot::now()
                .since(before)
                .is_allocation_free(),
            "firing lane step {step}"
        );
    }
    let mut runtime = drawing.into_runtime()?;
    lane.release(&mut runtime)?;
    Ok(runtime)
}

impl FiringLane {
    pub fn new() -> PipelineResult<Self> {
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 1,
            retired_mesh_versions: 1,
        })?;
        let vertices = [[-1., -1.], [1., -1.], [-1., 1.], [1., 1.]].map(|[x, y]| {
            PbrVertex::new([x, y, 0.], [0., 0., -1.], [1., 0., 0., 1.], [0.; 2]).unwrap()
        });
        let mesh = meshes.create(MeshData::pbr(&vertices, &[0, 1, 2, 1, 3, 2])?)?;
        let mut materials = Materials::default();
        let mut quad = |rgb, alpha, sx, sy, x, y, z| -> PipelineResult<RenderObject> {
            let mut material = PbrParams::color([0., 0., 0., alpha]);
            material.factors.double_sided = true;
            material.factors.emissive_nits = rgb;
            if alpha < 1. {
                material.factors.alpha_mode = AlphaMode::Blend;
            }
            let transform = Transform::new([[sx, 0., 0., x], [0., sy, 0., y], [0., 0., 1., z]])?;
            let mut object =
                RenderObject::new(mesh, materials.create_pbr(material)?).with_transform(transform);
            object.casts_shadow = false;
            Ok(object)
        };
        let glass = quad([0., 0., 0.25], 0.5, 1., 1., 0., 0., 0.55)?;
        let barrier = quad([0., 0.25, 0.], 1., 0.4, 1., -0.6, 0., 0.4)?;
        let tracer = quad([0.25, 0., 0.], 1., 1., 0.2, 0., 0., 0.8)?;
        Ok(Self {
            meshes,
            materials,
            objects: [glass, barrier, tracer],
        })
    }
    pub fn assets(&mut self) -> SceneAssets<'_> {
        SceneAssets::new(&mut self.meshes, &mut self.materials, &self.objects)
    }
    pub fn input(&mut self, step: usize) -> PipelineResult<SceneInput<'_>> {
        let depth = if step == 2 { 0.25 } else { 0.8 };
        self.objects[2].transform =
            Transform::new([[1., 0., 0., 0.], [0., 0.2, 0., 0.], [0., 0., 1., depth]])?;
        let count = if matches!(step, 1 | 2) { 3 } else { 2 };
        Ok(SceneInput::new(
            &self.meshes,
            &self.materials,
            &self.objects[..count],
        ))
    }
    pub fn release(mut self, runtime: &mut Runtime) -> PipelineResult<()> {
        self.meshes.release_gpu(runtime)?;
        self.materials.clear(runtime)?;
        Ok(())
    }
}

/// Reference values are sRGB encodings of linear 0.25 (137) and 0.125 (99).
/// Test projected pixel centers in the actual 3D grid, then the full-size HUD.
pub(crate) fn verify(
    pixels: &[u8],
    output: [u32; 2],
    scene: [u32; 2],
    step: usize,
) -> PipelineResult<()> {
    let [w, h] = output;
    let [sw, sh] = scene;
    for (i, pixel) in pixels.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = (i as u32 % w, i as u32 / w);
        let grid = |p, full, low| ((p as f32 + 0.5) * low as f32 / full as f32).floor() + 0.5;
        let nx = grid(x, w, sw) * 2. / sw as f32 - 1.;
        let ny = grid(y, h, sh) * 2. / sh as f32 - 1.;
        let tracer = ny.abs() < 0.2;
        let expected: [u8; 4] = if [x, y] == [1, 1] {
            [255; 4]
        } else if step == 2 && tracer {
            [137, 0, 0, 255]
        } else if nx < -0.2 {
            [0, 137, 0, 255]
        } else if step == 1 && tracer {
            [99, 0, 99, 255]
        } else {
            [0, 0, 99, 255]
        };
        if pixel
            .iter()
            .zip(expected)
            .any(|(&value, reference)| value.abs_diff(reference) > 1)
        {
            return Err(format!(
                "firing lane step {step} ({x},{y}) expected {expected:?}, got {pixel:?}"
            )
            .into());
        }
    }
    Ok(())
}

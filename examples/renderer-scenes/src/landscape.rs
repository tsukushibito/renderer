//! Render the glTF emitted by tools/landscape/prepare.py. Geometry is registered
//! once per source mesh. Every placement borrows that same mesh handle.
use crate::{Error, shared::camera::Camera};
use sashiko_renderer::{
    material::{AlphaMode, Materials, PbrParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::{Bounds, PbrVertex, Projection, ShadowSettings, Sun, Technique, Transform, ViewInput},
    pipeline::{
        AssetPreparation, CameraGroup, CameraView, RenderObject, RendererSetup, SceneAssets,
        SceneInput, StandardConfig, ViewId,
    },
};
use sashiko_renderer_core::{
    graph::ImageDesc,
    runtime::{RuntimeConfig, VulkanConfig},
    vulkan::ValidationMode,
};
use std::{
    fs::File,
    io::{BufReader, BufWriter},
    path::Path,
    time::{Duration, Instant},
};

const EXTENT: [u32; 2] = [960, 540];
const WAIT: Duration = Duration::from_secs(120);

fn load_meshes(
    doc: &gltf::Document,
    buffer: &[u8],
) -> Result<(Meshes, Vec<(sashiko_renderer::mesh::MeshHandle, usize)>), Error> {
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: doc.meshes().len(),
        retired_mesh_versions: 1,
    })?;
    let mut ids = Vec::new();
    for mesh in doc.meshes() {
        let mut primitives = mesh.primitives();
        let prim = primitives.next().ok_or("empty Landscape mesh")?;
        if primitives.next().is_some() || prim.mode() != gltf::mesh::Mode::Triangles {
            return Err("expected one triangle primitive per Landscape mesh".into());
        }
        let read = prim.reader(|b| (b.index() == 0).then_some(buffer));
        let positions = read.read_positions().ok_or("missing positions")?;
        let normals = read.read_normals().ok_or("missing normals")?;
        let uv = read
            .read_tex_coords(0)
            .ok_or("missing texture coordinates")?
            .into_f32();
        if positions.len() != normals.len() || positions.len() != uv.len() {
            return Err("inconsistent Landscape attributes".into());
        }
        let vertices = positions
            .zip(normals)
            .zip(uv)
            .map(|((p, n), uv)| {
                // No normal maps in this diffuse approximation. Complete the local frame.
                let [x, y, z] = n;
                let t = if x.abs() > z.abs() {
                    [-y, x, 0.]
                } else {
                    [0., -z, y]
                };
                let len = t.iter().map(|v| v * v).sum::<f32>().sqrt();
                let [tx, ty, tz] = if len > 0. {
                    t.map(|v| v / len)
                } else {
                    [1., 0., 0.]
                };
                PbrVertex::new(p, n, [tx, ty, tz, 1.], uv)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let indices: Vec<_> = read
            .read_indices()
            .ok_or("missing indices")?
            .into_u32()
            .collect();
        ids.push((
            meshes.create(MeshData::pbr(&vertices, &indices)?)?,
            prim.material().index().ok_or("missing material")?,
        ));
    }
    Ok((meshes, ids))
}

fn camera(name: &str) -> Result<CameraView, Error> {
    let (position, forward, fov) = match name {
        "cover" => (
            [-8.99305, 1.51107, -19.81427],
            [0.39362455, 0.04013179, 0.9183949],
            44.0959,
        ),
        "meadow" => ([0., 0.83, 1.8], [0., -0.26, -0.9656], 55.),
        "panorama" => ([-4., 7., -10.], [0.40, -0.14, 0.90], 58.),
        _ => return Err("unknown Landscape camera".into()),
    };
    let near_far = [0.02, 1500.];
    let aspect = EXTENT[0] as f32 / EXTENT[1] as f32;
    let cam = Camera {
        position,
        target: std::array::from_fn(|i| position[i] + forward[i]),
        fov,
    };
    let mut matrix = cam.matrix(aspect, near_far[0], near_far[1])?;
    if name == "cover" {
        // PBRT's perspective camera looks along +Z with screen-right along +X.
        for column in &mut matrix {
            column[0] = -column[0];
        }
    }
    Ok(CameraView {
        id: ViewId(0),
        extent: EXTENT,
        camera: ViewInput::new(
            matrix,
            position,
            forward.map(f64::from),
            near_far,
            Projection::SymmetricPerspective {
                vertical_fov_degrees: fov,
                aspect,
            },
        )?,
    })
}

pub fn run() -> Result<(), Error> {
    let mut args = std::env::args().skip(1);
    const USAGE: &str = "landscape SCENE_DIRECTORY OUTPUT.png [frames] [panorama|cover|meadow]";
    let input = args.next().ok_or(USAGE)?;
    if input == "--help" || input == "-h" {
        println!("{USAGE}");
        return Ok(());
    }
    let output = args.next().ok_or("missing output PNG path")?;
    let frames: usize = args.next().as_deref().unwrap_or("30").parse()?;
    let cam = camera(&args.next().unwrap_or_else(|| "panorama".to_string()))?;
    if frames == 0 || args.next().is_some() {
        return Err("expected a positive frame count and a known camera".into());
    }
    let root = Path::new(&input);
    let doc = gltf::Gltf::open(root.join("landscape.gltf"))?.document;
    let (mut meshes, ids) = load_meshes(&doc, &std::fs::read(root.join("mesh.bin"))?)?;
    let mut materials = Materials::default();
    let mut paints = Vec::new();
    for mat in doc.materials() {
        let mut p = PbrParams::color(mat.pbr_metallic_roughness().base_color_factor());
        p.factors.double_sided = mat.double_sided();
        p.factors.alpha_mode = if mat.alpha_mode() == gltf::material::AlphaMode::Mask {
            AlphaMode::Mask
        } else {
            AlphaMode::Opaque
        };
        p.factors.roughness = mat.pbr_metallic_roughness().roughness_factor();
        let texture = mat
            .pbr_metallic_roughness()
            .base_color_texture()
            .map(|t| t.texture().source().index());
        paints.push((materials.create_pbr(p.clone())?, p, texture));
    }
    let mut objects = Vec::new();
    let mut minimum = [f32::INFINITY; 3];
    let mut maximum = [f32::NEG_INFINITY; 3];
    for node in doc
        .default_scene()
        .ok_or("missing Landscape scene")?
        .nodes()
    {
        let source = node.mesh().ok_or("expected a placed mesh")?;
        let (mesh, material) = ids[source.index()];
        let bounds = source
            .primitives()
            .next()
            .ok_or("empty Landscape mesh")?
            .bounding_box();
        let m = node.transform().matrix();
        for corner in 0..8 {
            let point: [f32; 3] = std::array::from_fn(|i| {
                if corner & (1 << i) == 0 {
                    bounds.min[i]
                } else {
                    bounds.max[i]
                }
            });
            for axis in 0..3 {
                let world = m[3][axis] + (0..3).map(|i| m[i][axis] * point[i]).sum::<f32>();
                minimum[axis] = minimum[axis].min(world);
                maximum[axis] = maximum[axis].max(world);
            }
        }
        let t = Transform::new(std::array::from_fn(|r| std::array::from_fn(|c| m[c][r])))?;
        objects.push(RenderObject::new(mesh, paints[material].0).with_transform(t));
    }
    let mut config = StandardConfig::new(EXTENT);
    let bounds = Bounds::new(minimum, maximum)?;
    config.shadows = Some(ShadowSettings {
        resolution: 1024,
        distance: 80.,
        ..Default::default()
    });
    config.clear_color = [0.43, 0.61, 0.85, 1.];
    let mut group = CameraGroup::standard(cam, Technique::Forward, config);
    group.output = Some(sashiko_renderer::passes::output::SdrOutput::capture(true));
    let setup = RendererSetup::new(group);
    let mut plan = setup.prepare(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    let sky = crate::shared::ibl::Environment::fixture();
    let sky_desc = |im: &crate::shared::ibl::Image| ImageDesc::rgba16_float(im.width, im.height);
    for image in sky.images() {
        plan.reserve_texture("landscape skylight", sky_desc(image))?;
    }
    let mut textures = Vec::new();
    for source in doc.images() {
        let path = root.join(format!("color-{}.png", source.index()));
        let reader = png::Decoder::new(BufReader::new(File::open(&path)?)).read_info()?;
        if reader.info().color_type != png::ColorType::Rgba
            || reader.info().bit_depth != png::BitDepth::Eight
        {
            return Err("expected RGBA8 Landscape texture".into());
        }
        let desc = ImageDesc::rgba8_srgb(reader.info().width, reader.info().height);
        plan.reserve_texture("landscape color", desc)?;
        textures.push((path, desc));
    }
    drop(doc);
    plan.reserve_readback("landscape capture", ImageDesc::rgba8(EXTENT[0], EXTENT[1]))?;
    let mut backend = RuntimeConfig::Vulkan(VulkanConfig {
        validation: if cfg!(feature = "development-validation") {
            ValidationMode::Required
        } else {
            ValidationMode::Disabled
        },
        ..Default::default()
    });
    plan.fit_backend(&mut backend)?;
    let mut renderer = plan.build(backend)?;
    let mut sky_images = Vec::new();
    for im in sky.images() {
        let (id, image) = renderer.upload_texture(
            "landscape skylight",
            sky_desc(im),
            &im.bytes[im.mips[0].clone()],
        )?;
        renderer
            .runtime()
            .wait_for_frame(id, WAIT)?
            .ok_or("skylight upload timed out")?;
        sky_images.push(image);
    }
    let environment = sashiko_renderer::pipeline::EnvironmentMaps {
        irradiance: sky_images[0].snapshot().clone(),
        reflection: sky_images[1].snapshot().clone(),
        brdf_lut: sky_images[2].snapshot().clone(),
        yaw_radians: 0.,
        max_prefilter_lod: 0,
        nits_per_unit: 30.,
    };
    let mut resident = Vec::new();
    for (path, desc) in textures {
        let mut reader = png::Decoder::new(BufReader::new(File::open(path)?)).read_info()?;
        let mut rgba = vec![0; reader.output_buffer_size().ok_or("invalid PNG extent")?];
        reader.next_frame(&mut rgba)?;
        let (id, image) = renderer.upload_texture("landscape color", desc, &rgba)?;
        renderer
            .runtime()
            .wait_for_frame(id, WAIT)?
            .ok_or("texture upload timed out")?;
        resident.push(image);
    }
    for (handle, mut p, texture) in paints {
        if let Some(index) = texture {
            p.textures.base_color = Some(resident[index].snapshot().clone().into());
            materials.update_pbr(handle, p)?;
        }
    }
    renderer.prepare_assets(SceneAssets::new(&mut meshes, &mut materials, &objects))?;
    let sun = Sun::from_lux([0.55, 0.78, 0.30], [1., 0.96, 0.87], 6.)?;
    let mut render = |capture| -> Result<(), Error> {
        let mut frame = renderer.begin_frame()?;
        let mut input = SceneInput::new(&meshes, &materials, &objects)
            .with_sun(sun)
            .with_bounds(bounds);
        input.environment = Some(&environment);
        let scene = frame.bind_for_view(cam.id, input)?;
        let view = frame.draw_view(scene, &cam)?;
        let image = frame
            .write_output(cam.id, view.color, 1., None)?
            .raw_image();
        let read = if capture {
            Some(frame.graph().readback_image("landscape capture", image)?)
        } else {
            frame.graph().export_frame_image(image)?;
            None
        };
        let receipt = frame.compile()?.submit()?;
        let mut done = renderer
            .runtime()
            .wait_for_frame(receipt.id(), WAIT)?
            .ok_or("frame timed out")?;
        if let Some(handle) = read {
            let mut rgba = vec![0; done.readback_len(handle)?];
            done.readback_into(handle, &mut rgba)?;
            let mut encoder =
                png::Encoder::new(BufWriter::new(File::create(&output)?), EXTENT[0], EXTENT[1]);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.write_header()?.write_image_data(&rgba)?;
        }
        Ok(())
    };
    for _ in 0..5 {
        render(false)?;
    }
    let start = Instant::now();
    for _ in 0..frames {
        render(false)?;
    }
    let elapsed = start.elapsed();
    render(true)?;
    println!(
        "{} meshes, {} placed primitives; {} completed offscreen frames in {:.3}s ({:.1} FPS)",
        ids.len(),
        objects.len(),
        frames,
        elapsed.as_secs_f64(),
        frames as f64 / elapsed.as_secs_f64()
    );
    let native = renderer.runtime().as_vulkan().ok_or("expected Vulkan")?;
    println!(
        "{} memory after capture: {}",
        native.device_name(),
        crate::shared::memory::vulkan(native)?
    );
    let mut runtime = renderer.into_runtime()?;
    meshes.release_gpu(&mut runtime)?;
    materials.clear(&mut runtime)?;
    drop((resident, environment, sky_images));
    let monitor = runtime
        .as_vulkan()
        .ok_or("expected Vulkan")?
        .validation_monitor();
    drop(runtime);
    let report = monitor.report();
    if !report.is_clean()
        || (cfg!(feature = "development-validation")
            && (report.core_validation != Some(true)
                || report.synchronization_validation != Some(true)))
    {
        return Err(format!("{report:?}").into());
    }
    Ok(())
}

//! Bistro's adapter from its validated, world-space cook to offline GI inputs.
mod provenance;
use crate::{
    Error,
    shared::{asset::Scene, camera::Look},
};
use sashiko_lighting_bake::{
    BakeScene, Lighting, Material, ProbeSettings, ProbeShOrder, Texture, Triangle,
};
use sha2::{Digest, Sha256};
use std::{collections::HashMap, io::Write, path::PathBuf};

fn linear(channel: u8) -> f32 {
    let v = channel as f32 / 255.;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn import(scene: &Scene) -> Result<BakeScene, Error> {
    let mut textures = Vec::new();
    let mut ids = HashMap::new();
    let mut materials = Vec::new();
    let mut texture = |slot: usize| -> Result<usize, Error> {
        Ok(match ids.entry(slot) {
            std::collections::hash_map::Entry::Occupied(entry) => *entry.get(),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let image = &scene.images[slot];
                // Use a filtered cook mip for diffuse transport, keeping working
                // memory independent of full-resolution GPU material textures.
                let (mip, bytes) = image.bake_pixels()?;
                let pixels = bytes
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|&[r, g, b, a]| {
                        let decode = |x| {
                            if image.srgb {
                                linear(x)
                            } else {
                                x as f32 / 255.
                            }
                        };
                        [decode(r), decode(g), decode(b), a as f32 / 255.]
                    })
                    .collect();
                let id = textures.len();
                textures.push(Texture::new(
                    (image.width >> mip).max(1) as usize,
                    (image.height >> mip).max(1) as usize,
                    pixels,
                )?);
                entry.insert(id);
                id
            }
        })
    };
    for m in &scene.materials {
        let factors = sashiko_renderer::pbr::MaterialFactors::decode(&m.uniform)?;
        materials.push(Material {
            samplers: m.samplers,
            diffuse: [
                factors.base_color[0],
                factors.base_color[1],
                factors.base_color[2],
            ],
            emissive_nits: factors.emissive_nits,
            base_texture: Some(texture(m.textures[0])?),
            orm_texture: (factors.metallic > 0.)
                .then(|| texture(m.textures[2]))
                .transpose()?,
            emission_texture: (factors.emissive_nits != [0.; 3])
                .then(|| texture(m.textures[3]))
                .transpose()?,
            opacity: factors.base_color[3],
            metallic: factors.metallic,
            alpha_cutoff: matches!(
                factors.alpha_mode,
                sashiko_renderer::material::AlphaMode::Mask
            )
            .then_some(factors.alpha_cutoff),
        });
    }
    let mut triangles = Vec::new();
    for prim in &scene.primitives {
        let m = &scene.materials[prim.material];
        if m.alpha_zero || m.blend {
            continue;
        }
        let vertices = &scene.vertices[prim.vertices.clone()];
        for face in scene.indices[prim.indices.clone()].as_chunks::<12>().0 {
            let vertices = std::array::from_fn::<_, 3, _>(|c| {
                let i = u32::from_le_bytes(face[c * 4..c * 4 + 4].try_into().unwrap()) as usize;
                let v = &vertices[i * 48..i * 48 + 48];
                let f = |k: usize| f32::from_le_bytes(v[k * 4..k * 4 + 4].try_into().unwrap());
                ([f(0), f(1), f(2)], [f(10), f(11)])
            });
            triangles.push(Triangle {
                positions: vertices.map(|v| v.0),
                uv: vertices.map(|v| v.1),
                material: prim.material,
            });
        }
    }
    eprintln!("Building transport BVH for {} triangles", triangles.len());
    Ok(BakeScene::new(triangles, materials, textures)?)
}

fn receivers(scene: &Scene) -> Result<Vec<sashiko_lighting_bake::StaticReceiver>, Error> {
    let mut result = Vec::new();
    for (id, p) in scene.primitives.iter().enumerate() {
        let m = &scene.materials[p.material];
        if m.blend || m.alpha_zero {
            continue;
        }
        let (range, extent) = p.lightmap.as_ref().ok_or_else(|| {
            format!("placement {id} needs cooked lightmap UVs; recook the source scene")
        })?;
        let vertices = scene.vertices[p.vertices.clone()]
            .as_chunks::<48>()
            .0
            .iter()
            .zip(scene.lightmap_uv[range.clone()].as_chunks::<8>().0)
            .map(|(v, uv)| {
                let f = |i: usize| f32::from_le_bytes(v[i * 4..i * 4 + 4].try_into().unwrap());
                sashiko_lighting_bake::LightmapVertex {
                    position: [f(0), f(1), f(2)],
                    normal: [f(3), f(4), f(5)],
                    material_uv: [f(10), f(11)],
                    lightmap_uv: [
                        f32::from_le_bytes(uv[..4].try_into().unwrap()),
                        f32::from_le_bytes(uv[4..].try_into().unwrap()),
                    ],
                }
            })
            .collect();
        result.push(sashiko_lighting_bake::StaticReceiver {
            placement: u32::try_from(id)?,
            extent: *extent,
            material: p.material,
            double_sided: sashiko_renderer::pbr::MaterialFactors::decode(&m.uniform)?.double_sided,
            vertices,
            indices: scene.indices[p.indices.clone()]
                .as_chunks::<4>()
                .0
                .iter()
                .copied()
                .map(u32::from_le_bytes)
                .collect(),
        });
    }
    Ok(result)
}

fn publish<const N: usize>(
    output: &std::path::Path,
    identity: &str,
    layout: &sashiko_lighting_bake::SurfaceLayout<'_>,
    scene: &BakeScene,
    lights: [Lighting; N],
    settings: ProbeSettings,
) -> Result<(), Error> {
    use crate::shared::baked_assets::{LightingPublisher, object_name};
    use sashiko_renderer::pbr::SurfaceLightmapIndex;

    let publisher = LightingPublisher::new(output)?;
    let rays = settings.rays_per_probe;
    let surface = scene.prepare_surface_bake(lights, settings.backend, rays)?;
    let surface_execution = surface.execution().clone();
    let selected: Vec<_> = (0..layout.gpu_page_count()).collect();
    let mut records = Vec::with_capacity(selected.len());
    layout.bake_gpu_pages(surface, &selected, |id, record, bytes| {
        if id != records.len() || publisher.store(&bytes)? != *record.sha256() {
            return Err("surface page order or SHA-256 mismatch".into());
        }
        records.push(record);
        Ok(())
    })?;

    let prepared = scene.prepare_probe_bake(lights, settings)?;
    let probe_execution = prepared.execution().clone();
    let mut probe_bytes = Vec::new();
    prepared.bake()?.write(&mut probe_bytes)?;
    let probe_hash = publisher.store(&probe_bytes)?;
    let index = SurfaceLightmapIndex::new(N, records, probe_hash)?;
    let index_hash = publisher.store(&index.to_bytes())?;
    let build = provenance::record(
        identity,
        lights,
        settings,
        rays,
        &surface_execution,
        &probe_execution,
        &object_name(&index_hash),
    )?;
    publisher.publish(identity, &index_hash, build)
}

fn encode_reflections(
    atlas: sashiko_renderer::pbr::ReflectionProbes,
    compressed: bool,
) -> Result<sashiko_renderer::pbr::ReflectionProbes, Error> {
    use std::process::{Command, Stdio};
    if !compressed {
        return Ok(atlas);
    }
    let [width, height] = atlas.extent();
    let mut process = Command::new("python")
        .args([
            "-c",
            include_str!("../../../tools/bistro/reflection_codec.py"),
            &width.to_string(),
            &height.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    let input = process
        .stdin
        .take()
        .expect("piped encoder input")
        .write_all(atlas.pixels());
    let output = process.wait_with_output()?;
    if !output.status.success() {
        return Err(
            "BC6H encoding failed; use the Python environment from tools/bistro/requirements.txt"
                .into(),
        );
    }
    input?;
    Ok(sashiko_renderer::pbr::ReflectionProbes::new(
        atlas.probes().to_vec(),
        atlas.scenario_count(),
        sashiko_renderer_core::ImageFormat::Bc6hRgbUfloat,
        output.stdout,
    )?)
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<(), Error> {
    #[derive(serde::Deserialize, serde::Serialize)]
    #[serde(deny_unknown_fields)]
    struct Placement {
        name: String,
        position: [f32; 3],
        min: [f32; 3],
        max: [f32; 3],
        blend_distance: f32,
    }
    let mut layout = None;
    let mut reflection_format = None;
    let mut placement_spacing = None;
    let mut placement_method = None;
    let mut repair_placement = false;
    let mut max_probes = None;
    let mut reference = None;
    let mut diffuse_reference = None;
    let mut probe_sh_reference = None;
    let mut probe_ray_output = None;
    let mut ibl_manifest = None;
    let mut look = Look::LumberyardDay;
    let mut day_only = false;
    let mut endpoints_only = false;
    let mut manifest = None;
    let mut fixture = false;
    let mut output = None;
    let mut settings = ProbeSettings::default();
    let mut texel_budget = None;
    let wall_start = std::time::Instant::now();
    let mut benchmark = None;
    let mut estimate = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--probe-ray-output" => {
                probe_ray_output =
                    Some(PathBuf::from(args.next().ok_or("missing ray output path")?));
            }
            "--probe-sh-reference" => {
                probe_sh_reference = Some(PathBuf::from(
                    args.next().ok_or("missing probe positions file")?,
                ));
            }
            "--diffuse-reference" => {
                diffuse_reference = Some(PathBuf::from(
                    args.next().ok_or("missing diffuse receiver file")?,
                ))
            }
            "--reflection-reference" => {
                reference = Some(PathBuf::from(
                    args.next().ok_or("missing surface sample file")?,
                ));
            }
            "--look" => look = args.next().ok_or("missing lighting preset")?.parse()?,
            "--day-only" => day_only = true,
            "--endpoints-only" => endpoints_only = true,
            "--ibl-manifest" => {
                ibl_manifest = Some(PathBuf::from(args.next().ok_or("missing IBL manifest")?))
            }
            "--reflection-format" => {
                reflection_format = Some(args.next().ok_or("missing reflection format")?)
            }
            "--reflection-probes" => {
                layout = Some(PathBuf::from(
                    args.next().ok_or("missing reflection layout")?,
                ))
            }
            "--place-reflection-probes" => {
                placement_spacing = Some(
                    args.next()
                        .ok_or("missing placement spacing in metres")?
                        .parse()?,
                );
            }
            "--repair-placement" => repair_placement = true,
            "--placement-method" => {
                use sashiko_lighting_bake::ReflectionPlacementMethod;
                placement_method = Some(match args.next().as_deref() {
                    Some("floor") => ReflectionPlacementMethod::Floor,
                    Some("remedy") => ReflectionPlacementMethod::Remedy,
                    _ => return Err("expected placement method floor or remedy".into()),
                });
            }
            "--max-probes" => {
                max_probes = Some(args.next().ok_or("missing probe budget")?.parse()?)
            }
            "--manifest" => manifest = Some(PathBuf::from(args.next().ok_or("missing manifest")?)),
            "--fixture" => fixture = true,
            "--output" => output = Some(PathBuf::from(args.next().ok_or("missing output")?)),
            "--spacing" => settings.spacing = args.next().ok_or("missing spacing")?.parse()?,
            "--max-depth" => settings.max_depth = args.next().ok_or("missing depth")?.parse()?,
            "--probe-sh" => {
                settings.sh_order = match args.next().as_deref() {
                    Some("l1") => ProbeShOrder::L1,
                    Some("l2") => ProbeShOrder::L2,
                    _ => return Err("--probe-sh expects l1 or l2".into()),
                }
            }
            "--backend" => {
                settings.backend = match args.next().as_deref() {
                    Some("vulkan-rt") => sashiko_lighting_bake::BakeBackend::VulkanRayQuery,
                    Some("vulkan-software") => sashiko_lighting_bake::BakeBackend::VulkanSoftware,
                    Some("vulkan") => sashiko_lighting_bake::BakeBackend::Vulkan,
                    Some("d3d12") => sashiko_lighting_bake::BakeBackend::D3D12,
                    Some("cpu") => sashiko_lighting_bake::BakeBackend::Cpu,
                    Some("auto") => sashiko_lighting_bake::BakeBackend::Auto,
                    _ => return Err("unknown bake backend".into()),
                }
            }
            "--benchmark-seed" => {
                benchmark = Some(args.next().ok_or("missing seed")?.parse::<u64>()?)
            }
            "--estimate" => estimate = true,
            "--lightmap-texels" => {
                texel_budget = Some(args.next().ok_or("missing texel budget")?.parse()?)
            }
            "--samples" => {
                settings.rays_per_probe = args.next().ok_or("missing samples")?.parse()?
            }
            "--help" => {
                println!(
                    "renderer-scenes-bake (--fixture | --manifest FILE) --output FILE [--look lumberyard-day|o3de-atrium] [--day-only | --endpoints-only] [--ibl-manifest FILE] [--spacing METRES] [--max-depth N] [--probe-sh l1|l2] [--samples N] [--backend auto|vulkan|vulkan-rt|vulkan-software|d3d12|cpu] [--estimate] [--benchmark-seed N --day-only] [--lightmap-texels N] [--place-reflection-probes METRES | --reflection-probes LAYOUT.json [--reflection-format bc6h|rgba32f] | --reflection-reference RECEIVERS.json | --diffuse-reference RECEIVERS.json | --probe-sh-reference POSITIONS.json [--probe-ray-output FILE]]"
                );
                println!(
                    "Placement options: --placement-method floor|remedy --max-probes N (1..32) [--repair-placement (daylight quality report on stdout)]"
                );
                println!(
                    "--probe-sh-reference also accepts --probe-ray-output FILE: little-endian f32 triples; one direction per sample, then one RGB radiance per probe and sample."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown bake option {arg}").into()),
        }
    }
    if benchmark.is_some()
        && (!day_only
            || estimate
            || endpoints_only
            || reference.is_some()
            || diffuse_reference.is_some()
            || probe_sh_reference.is_some()
            || layout.is_some()
            || placement_spacing.is_some())
    {
        return Err("benchmark requires a daylight surface bake".into());
    }
    if placement_spacing.is_none()
        && (placement_method.is_some() || max_probes.is_some() || repair_placement)
    {
        return Err("placement options require --place-reflection-probes".into());
    }
    if day_only && endpoints_only {
        return Err("choose either day-only or endpoints-only".into());
    }
    let reference_count = [&reference, &diffuse_reference, &probe_sh_reference]
        .into_iter()
        .filter(|path| path.is_some())
        .count();
    if probe_ray_output.is_some() && probe_sh_reference.is_none() {
        return Err("--probe-ray-output requires --probe-sh-reference".into());
    }
    if reference_count > 0
        && (layout.is_some()
            || reference_count > 1
            || estimate
            || endpoints_only
            || ibl_manifest.is_none()
            || (reference.is_some() || probe_sh_reference.is_some())
                && !matches!(
                    settings.backend,
                    sashiko_lighting_bake::BakeBackend::Auto
                        | sashiko_lighting_bake::BakeBackend::Cpu
                ))
    {
        return Err("lighting references require daylight, an IBL manifest and no probe layout; specular and probe SH references require CPU".into());
    }

    if reflection_format.is_some() && layout.is_none() {
        return Err("--reflection-format requires --reflection-probes".into());
    }
    if repair_placement && endpoints_only {
        return Err("--repair-placement evaluates daylight only".into());
    }
    let scene = match (manifest, fixture) {
        (None, true) => Scene::fixture(false),
        (Some(path), false) => Scene::load(&path, false)?,
        _ => return Err("select --manifest or --fixture".into()),
    };
    if placement_spacing.is_some() && (layout.is_some() || reference_count > 0 || estimate) {
        return Err("choose one reflection layout, reference, estimate or bake operation".into());
    }
    fn write_layout(
        output: &std::path::Path,
        probes: Vec<sashiko_renderer::pbr::ReflectionProbeDesc>,
    ) -> Result<(), Error> {
        let placements: Vec<_> = probes
            .into_iter()
            .enumerate()
            .map(|(i, p)| Placement {
                name: format!("Probe {}", i + 1),
                position: p.position,
                min: p.min,
                max: p.max,
                blend_distance: p.blend_distance,
            })
            .collect();
        let bytes = serde_json::to_vec_pretty(&placements)?;
        std::fs::File::create_new(output)?.write_all(&bytes)?;
        eprintln!("Generated {} reflection probe placements", placements.len());
        Ok(())
    }
    let placement =
        placement_spacing.map(
            |spacing| sashiko_lighting_bake::ReflectionPlacementSettings {
                spacing,
                method: placement_method.unwrap_or_default(),
                max_probes: max_probes
                    .unwrap_or(sashiko_renderer::pbr::ReflectionProbes::MAX_COUNT),
                ..Default::default()
            },
        );
    if let Some(settings) = placement
        && !repair_placement
    {
        let output = output
            .as_deref()
            .ok_or("missing --output for generated reflection layout")?;
        write_layout(output, import(&scene)?.place_reflection_probes(settings)?)?;
        return Ok(());
    }
    let surface_bake = placement.is_none() && reference_count == 0 && layout.is_none();
    let receivers = if surface_bake {
        receivers(&scene)?
    } else {
        Vec::new()
    };
    let keys = if day_only {
        1
    } else if endpoints_only {
        2
    } else {
        3
    };
    let surfaces = sashiko_lighting_bake::SurfaceLayout::new(&receivers, 2048, keys, texel_budget)?;
    if surface_bake {
        eprintln!(
            "selected limit {}, {} resident RGBA32F bytes, {} material/page groups",
            surfaces.page_limit(),
            surfaces.resident_bytes(),
            surfaces.material_page_groups()
        );
        eprintln!(
            "{} static placements, {} receiver triangles, {} sides in {} pages, {} allocated texels/scenario",
            receivers.len(),
            receivers.iter().map(|r| r.indices.len() / 3).sum::<usize>(),
            surfaces.regions().count(),
            surfaces.pages().len(),
            surfaces.texels()
        );
        eprintln!(
            "{} RGB32 bytes for {keys} lighting scenario(s), at most {} hemisphere rays per scenario",
            surfaces.texels() * 12 * keys as u64,
            surfaces.texels() * u64::from(settings.rays_per_probe)
        );
    }
    if estimate {
        return Ok(());
    }
    let output = output.ok_or("missing --output")?;
    // Surface generations can replace an earlier manifest after all dependencies are ready.
    if output.exists()
        && (repair_placement
            || layout.is_some()
            || benchmark.is_some()
            || reference.is_some()
            || diffuse_reference.is_some()
            || probe_sh_reference.is_some())
    {
        return Err("output already exists".into());
    }
    let identity = if fixture {
        format!("{:x}", Sha256::digest(b"builtin-pbr-material-fixture-v1"))
    } else {
        scene.source["cooked_manifest_sha256"]
            .as_str()
            .ok_or("missing scene identity")?
            .to_string()
    };
    let import_start = std::time::Instant::now();
    let mut bake = import(&scene)?;
    let import_elapsed = import_start.elapsed();
    let mut identity = crate::shared::camera::bake_identity(&identity, look);
    let scenarios = look.scenarios();
    let sky_strength = if look == Look::O3deAtrium {
        bake.set_rect_lights(crate::shared::lighting::sponza_rectangles().to_vec());
        1.
    } else {
        crate::shared::camera::DAY_IBL_STRENGTH
    };
    let mut panorama = None;
    if let Some(path) = ibl_manifest {
        let mut env = crate::shared::ibl::Environment::load(&path)?;
        env.nits_per_unit = sky_strength;
        identity = crate::shared::camera::environment_bake_identity(&identity, &env)
            .ok_or("missing IBL identity")?;
        // Level zero is the sharp linear HDR map used by runtime reflections.
        let image = env.specular;
        let texels = image.bytes[image.mips[0].clone()]
            .as_chunks::<8>()
            .0
            .iter()
            .map(|pixel| {
                std::array::from_fn(|i| {
                    half::f16::from_le_bytes([pixel[i * 2], pixel[i * 2 + 1]]).to_f32()
                })
            })
            .collect();
        panorama = Some(bake.add_sky_map(
            Texture::new(image.width as usize, image.height as usize, texels)?,
            env.yaw_degrees.to_radians(),
        )?);
    }
    let lighting =
        |s: crate::shared::camera::BakeScenario| -> Result<_, sashiko_renderer::core::graph::GraphError> {
            Ok(Lighting {
                environment: panorama,
                sky_rgb_nits: if panorama.is_some() {
                    std::array::from_fn(|i| sky_strength * s.sky[i] / scenarios[0].sky[i])
                } else {
                    s.sky
                },
                sun: sashiko_renderer::pbr::Sun::from_rgb_lux(s.direction, s.sun)?,
            })
        };
    let lights = [lighting(scenarios[0])?, lighting(scenarios[1])?];
    if let Some(settings) = placement {
        let mut probes = bake.place_reflection_probes(settings)?;
        let started = std::time::Instant::now();
        let quality = bake.repair_reflection_placement(&mut probes, settings.spacing, lights[0])?;
        let seconds = started.elapsed().as_secs_f64();
        let metrics = |m: sashiko_lighting_bake::QualityMetrics| {
            serde_json::json!({
                "samples": m.samples, "mean_rgb_error": m.mean_rgb_error, "p90_rgb_error": m.p90_rgb_error,
                "worst_position": m.worst_position, "worst_rgb_error": m.worst_rgb_error,
            })
        };
        write_layout(&output, probes)?;
        println!(
            "{}",
            serde_json::json!({
                "decision": format!("{:?}", quality.decision), "proposed": quality.proposed.map(metrics), "before": metrics(quality.before), "after": metrics(quality.after), "quality_seconds": seconds,
                "training_samples": quality.training_samples, "candidates_tested": quality.candidates_tested,
                "replacement_captures": quality.replacement_captures, "changed_probe_index": quality.changed_probe,
                "moved_capture": quality.moved_capture,
            })
        );
        return Ok(());
    }
    if let Some(path) = probe_sh_reference {
        let positions: Vec<[f32; 3]> = serde_json::from_slice(&std::fs::read(path)?)?;
        #[derive(serde::Serialize)]
        struct Projection {
            identity: String,
            samples: u32,
            coefficients: Vec<[[f32; 3]; 9]>,
        }
        let mut rays = probe_ray_output
            .as_ref()
            .map(std::fs::File::create_new)
            .transpose()?
            .map(std::io::BufWriter::new);
        let mut created_json = false;
        let result = (|| -> Result<(), Error> {
            let mut file = std::io::BufWriter::new(std::fs::File::create_new(&output)?);
            created_json = true;
            let coefficients = bake.probe_sh_reference(
                &positions,
                lights[0],
                settings.rays_per_probe,
                rays.as_mut().map(|file| file as &mut dyn Write),
            )?;
            if let Some(file) = &mut rays {
                file.flush()?;
            }
            serde_json::to_writer(
                &mut file,
                &Projection {
                    identity,
                    samples: settings.rays_per_probe,
                    coefficients,
                },
            )?;
            file.flush()?;
            Ok(())
        })();
        drop(rays);
        if result.is_err() {
            if created_json {
                let _ = std::fs::remove_file(&output);
            }
            if let Some(path) = probe_ray_output {
                let _ = std::fs::remove_file(path);
            }
        }
        return result;
    }
    if let Some(path) = diffuse_reference {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Receiver {
            position: [f32; 3],
            normal: [f32; 3],
        }
        let points: Vec<Receiver> = serde_json::from_slice(&std::fs::read(path)?)?;
        let samples: Vec<_> = points
            .into_iter()
            .map(|r| sashiko_lighting_bake::SurfaceSample {
                position: r.position,
                normal: r.normal,
            })
            .collect();
        let irradiance = if matches!(
            settings.backend,
            sashiko_lighting_bake::BakeBackend::Auto | sashiko_lighting_bake::BakeBackend::Cpu
        ) {
            bake.bake_irradiance(&samples, lights[0], settings.rays_per_probe)?
        } else {
            bake.sample_surface_irradiance(&samples, lights[0], settings)?
        };
        let mut file = std::io::BufWriter::new(std::fs::File::create_new(output)?);
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({"identity":identity,
            "samples":settings.rays_per_probe,"irradiance":irradiance}),
        )?;
        file.flush()?;
        return Ok(());
    }
    if let Some(path) = reference {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Input {
            position: [f32; 3],
            geometric_normal: [f32; 3],
            shading_normal: [f32; 3],
            view: [f32; 3],
            f0: [f32; 3],
            roughness: f32,
        }
        let receivers: Vec<Input> = serde_json::from_slice(&std::fs::read(path)?)?;
        let mut results = Vec::with_capacity(receivers.len());
        for s in receivers {
            let r = bake.reflection_reference(
                &sashiko_lighting_bake::ReflectionReceiver {
                    position: s.position,
                    geometric_normal: s.geometric_normal,
                    shading_normal: s.shading_normal,
                    view: s.view,
                    f0: s.f0,
                    roughness: s.roughness,
                },
                lights[0],
                settings.rays_per_probe,
            )?;
            results.push(
                serde_json::json!({"sky":r.from_sky,"geometry":r.from_geometry,
                "unobstructed":r.unobstructed,"sky_rays":r.sky_rays,"surface_rays":r.surface_rays}),
            );
        }
        let mut file = std::io::BufWriter::new(std::fs::File::create_new(output)?);
        serde_json::to_writer(
            &mut file,
            &serde_json::json!({"identity":identity,
            "samples":settings.rays_per_probe,"results":results}),
        )?;
        file.flush()?;
        return Ok(());
    }
    if let Some(layout) = layout {
        if !matches!(
            settings.backend,
            sashiko_lighting_bake::BakeBackend::Auto | sashiko_lighting_bake::BakeBackend::Cpu
        ) {
            return Err("reflection capture currently uses the CPU bake backend".into());
        }
        let placements: Vec<Placement> = serde_json::from_slice(&std::fs::read(layout)?)?;
        let probes = placements
            .into_iter()
            .map(|p| {
                let desc = sashiko_lighting_bake::ReflectionProbeDesc {
                    position: p.position,
                    min: p.min,
                    max: p.max,
                    blend_distance: p.blend_distance,
                };
                desc.validate().map_err(|e| format!("{}: {e}", p.name))?;
                Ok(desc)
            })
            .collect::<Result<Vec<_>, Error>>()?;
        let format = reflection_format
            .as_deref()
            .or_else(|| {
                scene
                    .source
                    .get("reflection_probe_format")
                    .and_then(|f| f.as_str())
            })
            .unwrap_or("bc6h");
        let compressed = match format {
            "bc6h" => true,
            "rgba32f" => false,
            _ => return Err("unknown reflection probe format".into()),
        };
        drop(scene);
        let atlas = if day_only {
            bake.bake_reflection_probes(&probes, lights[0])?
        } else {
            bake.bake_reflection_probes(&probes, lights)?
        };
        let atlas = encode_reflections(atlas, compressed)?;
        let mut file = std::io::BufWriter::new(std::fs::File::create_new(output)?);
        file.write_all(identity.as_bytes())?;
        atlas.write(&mut file)?;
        file.flush()?;
        eprintln!("Baked {} local reflection probes", probes.len());
        return Ok(());
    }
    drop(scene);
    let started = std::time::Instant::now();
    if let Some(seed) = benchmark {
        // Exercise real output I/O without publishing the partially populated asset.
        let scratch = output.with_extension(format!("{}.scratch", std::process::id()));
        let file = std::fs::File::create_new(&scratch)?;
        let measured = (|| -> Result<_, Error> {
            let mut writer = std::io::BufWriter::new(file);
            writer.write_all(identity.as_bytes())?;
            let pages = surfaces.benchmark_to(&bake, lights[0], settings, &mut writer, seed)?;
            writer.flush()?;
            Ok(pages)
        })();
        let cleanup = std::fs::remove_file(scratch);
        let pages = measured?;
        cleanup?;
        let elapsed = wall_start.elapsed().as_secs_f64();
        let mut estimate = elapsed;
        let mut records = Vec::new();
        for p in pages.pages {
            let seconds = p.elapsed.as_secs_f64();
            if !p.irradiance.is_empty() {
                estimate += seconds * (p.receivers as f64 / p.irradiance.len() as f64 - 1.);
            }
            records.push(serde_json::json!({
                "receivers": p.receivers, "sampled": p.irradiance.len(),
                "seconds": seconds, "gpu_seconds": p.gpu.map(|v| v.as_secs_f64()),
                "host_wait_seconds": p.host_wait.as_secs_f64(),
                "host_transfer_seconds": p.host_transfer.as_secs_f64(),
                "raster_seconds": p.raster.as_secs_f64(), "irradiance": p.irradiance,
            }));
        }
        let mut report = std::io::BufWriter::new(std::fs::File::create_new(output)?);
        serde_json::to_writer(
            &mut report,
            &serde_json::json!({
                "seed": seed, "rays": settings.rays_per_probe, "identity": identity,
                "elapsed_seconds": elapsed, "estimated_seconds": estimate,
                "load_layout_seconds": import_start.duration_since(wall_start).as_secs_f64(),
                "cpu_scene_bvh_seconds": import_elapsed.as_secs_f64(),
                "transport_setup_seconds": pages.transport_setup.as_secs_f64(),
                "write_pages_seconds": pages.write_pages.as_secs_f64(),
                "probe_seconds": pages.probes.as_secs_f64(), "pages": records,
            }),
        )?;
        report.flush()?;
        eprintln!("Partial bake {elapsed:.2}s; estimated daylight bake {estimate:.2}s");
        return Ok(());
    }
    if day_only {
        publish(&output, &identity, &surfaces, &bake, [lights[0]], settings)?;
    } else if endpoints_only {
        publish(&output, &identity, &surfaces, &bake, lights, settings)?;
    } else {
        let keys = [
            lights[0],
            lighting(crate::shared::camera::BakeScenario::for_look(0.5, look))?,
            lights[1],
        ];
        identity = crate::shared::camera::daylight_sequence_identity(&identity, look);
        publish(&output, &identity, &surfaces, &bake, keys, settings)?;
    }
    eprintln!("Baked lighting in {:.2}s", started.elapsed().as_secs_f64());
    Ok(())
}

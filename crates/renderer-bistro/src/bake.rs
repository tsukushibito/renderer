//! Bistro's adapter from its validated, world-space cook to offline GI inputs.
use crate::{
    Error,
    shared::{asset::Scene, camera::Look},
};
use renderer_bake::{
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
        let factors = renderer::pbr::MaterialFactors::decode(&m.uniform)?;
        materials.push(Material {
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
            alpha_cutoff: matches!(factors.alpha_mode, renderer::material::AlphaMode::Mask)
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

fn receivers(scene: &Scene) -> Result<Vec<renderer_bake::StaticReceiver>, Error> {
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
                renderer_bake::LightmapVertex {
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
        result.push(renderer_bake::StaticReceiver {
            placement: u32::try_from(id)?,
            extent: *extent,
            material: p.material,
            double_sided: renderer::pbr::MaterialFactors::decode(&m.uniform)?.double_sided,
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
    layout: &renderer_bake::SurfaceLayout<'_>,
    scene: &BakeScene,
    lights: [Lighting; N],
    settings: ProbeSettings,
) -> Result<(), Error> {
    let mut staging = output.as_os_str().to_owned();
    staging.push(format!(".{}.partial", std::process::id()));
    let staging = PathBuf::from(staging);
    let file = std::fs::File::create_new(&staging)?;
    let result = (|| -> Result<(), Error> {
        let mut writer = std::io::BufWriter::new(file);
        writer.write_all(identity.as_bytes())?;
        let counts = layout.bake_to(
            scene,
            lights,
            settings,
            settings.rays_per_probe,
            &mut writer,
        )?;
        eprintln!(
            "{} traced surface samples per scenario",
            counts.iter().sum::<usize>()
        );
        writer.flush()?;
        drop(writer);
        std::fs::rename(&staging, output)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(staging);
    }
    result
}

fn encode_reflections(
    atlas: renderer::pbr::ReflectionProbes,
    compressed: bool,
) -> Result<renderer::pbr::ReflectionProbes, Error> {
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
    Ok(renderer::pbr::ReflectionProbes::new(
        atlas.probes().to_vec(),
        atlas.scenario_count(),
        renderer_core::ImageFormat::Bc6hRgbUfloat,
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
    let mut max_probes = None;
    let mut reference = None;
    let mut diffuse_reference = None;
    let mut ibl_manifest = None;
    let mut look = Look::LumberyardDay;
    let mut day_only = false;
    let mut endpoints_only = false;
    let mut manifest = None;
    let mut fixture = false;
    let mut output = None;
    let mut settings = ProbeSettings::default();
    let mut texel_budget = None;
    let mut estimate = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
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
            "--placement-method" => {
                use renderer_bake::ReflectionPlacementMethod;
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
                    Some("vulkan") => renderer_bake::BakeBackend::Vulkan,
                    Some("d3d12") => renderer_bake::BakeBackend::D3D12,
                    Some("cpu") => renderer_bake::BakeBackend::Cpu,
                    Some("auto") => renderer_bake::BakeBackend::Auto,
                    _ => return Err("unknown bake backend".into()),
                }
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
                    "bistro-bake (--fixture | --manifest FILE) --output FILE [--look lumberyard-day|o3de-atrium] [--day-only | --endpoints-only] [--ibl-manifest FILE] [--spacing METRES] [--max-depth N] [--probe-sh l1|l2] [--samples N] [--backend auto|vulkan|d3d12|cpu] [--estimate] [--lightmap-texels N] [--place-reflection-probes METRES | --reflection-probes LAYOUT.json [--reflection-format bc6h|rgba32f] | --reflection-reference RECEIVERS.json | --diffuse-reference RECEIVERS.json]"
                );
                println!(
                    "Placement options: --placement-method floor|remedy --max-probes N (1..32)"
                );
                return Ok(());
            }
            _ => return Err(format!("unknown bake option {arg}").into()),
        }
    }
    if placement_spacing.is_none() && (placement_method.is_some() || max_probes.is_some()) {
        return Err("placement options require --place-reflection-probes".into());
    }
    if day_only && endpoints_only {
        return Err("choose either day-only or endpoints-only".into());
    }
    if (reference.is_some() || diffuse_reference.is_some())
        && (layout.is_some()
            || (reference.is_some() && diffuse_reference.is_some())
            || endpoints_only
            || ibl_manifest.is_none()
            || !matches!(
                settings.backend,
                renderer_bake::BakeBackend::Auto | renderer_bake::BakeBackend::Cpu
            ))
    {
        return Err("lighting references require the CPU daylight path, an IBL manifest and no probe layout".into());
    }

    if reflection_format.is_some() && layout.is_none() {
        return Err("--reflection-format requires --reflection-probes".into());
    }
    let scene = match (manifest, fixture) {
        (None, true) => Scene::fixture(false),
        (Some(path), false) => Scene::load(&path, false)?,
        _ => return Err("select --manifest or --fixture".into()),
    };
    if let Some(spacing) = placement_spacing {
        if layout.is_some() || reference.is_some() || diffuse_reference.is_some() || estimate {
            return Err(
                "choose one reflection layout, reference, estimate or bake operation".into(),
            );
        }
        let output = output.ok_or("missing --output for generated reflection layout")?;
        let probes = import(&scene)?.place_reflection_probes(
            renderer_bake::ReflectionPlacementSettings {
                spacing,
                method: placement_method.unwrap_or_default(),
                max_probes: max_probes.unwrap_or(renderer::pbr::ReflectionProbes::MAX_COUNT),
                ..Default::default()
            },
        )?;
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
        return Ok(());
    }
    let receivers = if reference.is_none() && diffuse_reference.is_none() && layout.is_none() {
        receivers(&scene)?
    } else {
        Vec::new()
    };
    let surfaces = renderer_bake::SurfaceLayout::new(&receivers, 2048, texel_budget)?;
    if reference.is_none() && diffuse_reference.is_none() && layout.is_none() {
        eprintln!(
            "{} static placements, {} receiver triangles, {} sides in {} pages, {} allocated texels/scenario",
            receivers.len(),
            receivers.iter().map(|r| r.indices.len() / 3).sum::<usize>(),
            surfaces.regions().count(),
            surfaces.pages().len(),
            surfaces.texels()
        );
        let keys = if day_only {
            1
        } else if endpoints_only {
            2
        } else {
            3
        };
        eprintln!(
            "{} RGB32 bytes for {keys} lighting scenario(s), at most {} hemisphere rays per scenario",
            surfaces.texels() * 12 * keys,
            surfaces.texels() * u64::from(settings.rays_per_probe)
        );
    }
    if estimate {
        return Ok(());
    }
    let output = output.ok_or("missing --output")?;
    if output.exists() {
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
    let mut bake = import(&scene)?;
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
        |s: crate::shared::camera::BakeScenario| -> Result<_, renderer::core::graph::GraphError> {
            Ok(Lighting {
                environment: panorama,
                sky_rgb_nits: if panorama.is_some() {
                    std::array::from_fn(|i| sky_strength * s.sky[i] / scenarios[0].sky[i])
                } else {
                    s.sky
                },
                sun: renderer::pbr::Sun::from_rgb_lux(s.direction, s.sun)?,
            })
        };
    let lights = [lighting(scenarios[0])?, lighting(scenarios[1])?];
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
            .map(|r| renderer_bake::SurfaceSample {
                position: r.position,
                normal: r.normal,
            })
            .collect();
        let irradiance = bake.bake_irradiance(&samples, lights[0], settings.rays_per_probe)?;
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
                &renderer_bake::ReflectionReceiver {
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
            renderer_bake::BakeBackend::Auto | renderer_bake::BakeBackend::Cpu
        ) {
            return Err("reflection capture currently uses the CPU bake backend".into());
        }
        let placements: Vec<Placement> = serde_json::from_slice(&std::fs::read(layout)?)?;
        let probes = placements
            .into_iter()
            .map(|p| {
                let desc = renderer_bake::ReflectionProbeDesc {
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

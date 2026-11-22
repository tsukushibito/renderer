//! Bistro's adapter from its validated, world-space cook to offline GI inputs.
use crate::{
    Error,
    shared::{asset::Scene, camera::BAKE_SCENARIOS},
};
use renderer_bake::{BakeScene, Lighting, Material, ProbeSettings, Texture, Triangle};
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
                let mip = (0..image.mips.len())
                    .find(|&i| (image.width.max(image.height) >> i) <= 128)
                    .unwrap_or(image.mips.len() - 1);
                let bytes = image.load_bytes()?;
                let pixels = bytes[image.mips[mip].clone()]
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
            emission: factors.emissive,
            base_texture: Some(texture(m.textures[0])?),
            orm_texture: (factors.metallic > 0.)
                .then(|| texture(m.textures[2]))
                .transpose()?,
            emission_texture: (factors.emissive != [0.; 3])
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

// Static architectural receivers around the street camera. Select whole mesh
// placements within the sample's texel budget; other objects use the volume.
fn receivers(
    scene: &Scene,
    settings: renderer_bake::LightmapSettings,
    budget: usize,
) -> Result<(Vec<u32>, Vec<renderer_bake::ReceiverTriangle>), Error> {
    let mut candidates = Vec::new();
    for (id, p) in scene.primitives.iter().enumerate() {
        let m = &scene.materials[p.material];
        if m.blend || m.alpha_zero || m.uniform[48] != 0 || p.index_count > 1200 {
            continue;
        }
        let vertices = &scene.vertices[p.vertices.clone()];
        let distance = p
            .center
            .into_iter()
            .zip([-13., 3., 0.])
            .map(|(a, b)| (a - b) * (a - b))
            .sum::<f32>();
        let mut tris = Vec::new();
        let mut cost = 0;
        for face in scene.indices[p.indices.clone()].as_chunks::<12>().0 {
            let v = std::array::from_fn::<_, 3, _>(|j| {
                let index = u32::from_le_bytes(face[j * 4..j * 4 + 4].try_into().unwrap()) as usize;
                let f = |k: usize| {
                    f32::from_le_bytes(
                        vertices[index * 48 + k * 4..index * 48 + k * 4 + 4]
                            .try_into()
                            .unwrap(),
                    )
                };
                ([f(0), f(1), f(2)], [f(3), f(4), f(5)])
            });
            let tri = renderer_bake::ReceiverTriangle {
                positions: v.map(|v| v.0),
                normals: v.map(|v| v.1),
            };
            let [w, h] = tri.chart_extent(settings)?;
            cost += usize::from(w) * usize::from(h);
            tris.push(tri);
        }
        // Prefer broad surfaces per chart; tessellated details receive probes.
        if !tris.is_empty() && cost <= budget {
            let priority = cost as f32 / (tris.len() as f32 * (1. + distance * 0.01));
            candidates.push((priority, id, cost, tris));
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut selected = Vec::new();
    let mut remaining = budget;
    for (_, id, cost, tris) in candidates {
        if cost <= remaining {
            remaining -= cost;
            selected.push((id, tris));
        }
    }
    selected.sort_by_key(|(id, _)| *id);
    let mut entries = vec![];
    let mut triangles = vec![];
    for (id, tris) in selected {
        entries.push(u32::try_from(id)?);
        triangles.extend(tris);
    }
    Ok((entries, triangles))
}

pub fn run(mut args: impl Iterator<Item = String>) -> Result<(), Error> {
    let mut manifest = None;
    let mut fixture = false;
    let mut output = None;
    let mut settings = ProbeSettings::default();
    let mut texel_budget = 16384;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--manifest" => manifest = Some(PathBuf::from(args.next().ok_or("missing manifest")?)),
            "--fixture" => fixture = true,
            "--output" => output = Some(PathBuf::from(args.next().ok_or("missing output")?)),
            "--spacing" => settings.spacing = args.next().ok_or("missing spacing")?.parse()?,
            "--max-depth" => settings.max_depth = args.next().ok_or("missing depth")?.parse()?,
            "--backend" => {
                settings.backend = match args.next().as_deref() {
                    Some("vulkan") => renderer_bake::BakeBackend::Vulkan,
                    Some("d3d12") => renderer_bake::BakeBackend::D3D12,
                    Some("cpu") => renderer_bake::BakeBackend::Cpu,
                    Some("auto") => renderer_bake::BakeBackend::Auto,
                    _ => return Err("unknown bake backend".into()),
                }
            }
            "--lightmap-texels" => {
                texel_budget = args.next().ok_or("missing texel budget")?.parse()?
            }
            "--samples" => {
                settings.rays_per_probe = args.next().ok_or("missing samples")?.parse()?
            }
            "--help" => {
                println!(
                    "bistro-bake (--fixture | --manifest FILE) --output FILE [--spacing METRES] [--max-depth N] [--samples N] [--backend auto|vulkan|d3d12|cpu] [--lightmap-texels N]"
                );
                return Ok(());
            }
            _ => return Err(format!("unknown bake option {arg}").into()),
        }
    }
    let output = output.ok_or("missing --output")?;
    if output.exists() {
        return Err("output already exists".into());
    }
    let scene = match (manifest, fixture) {
        (None, true) => Scene::fixture(false),
        (Some(path), false) => Scene::load(&path, false)?,
        _ => return Err("select --manifest or --fixture".into()),
    };
    let identity = if fixture {
        format!("{:x}", Sha256::digest(b"builtin-pbr-material-fixture-v1"))
    } else {
        scene.source["cooked_manifest_sha256"]
            .as_str()
            .ok_or("missing scene identity")?
            .to_string()
    };
    let bake = import(&scene)?;
    let surface_settings = renderer_bake::LightmapSettings {
        backend: settings.backend,
        rays_per_texel: settings.rays_per_probe,
        ..Default::default()
    };
    let (placements, triangles) = receivers(&scene, surface_settings, texel_budget)?;
    drop(scene);
    let started = std::time::Instant::now();
    let lights = BAKE_SCENARIOS.map(|s| Lighting {
        sky: s.sky,
        sun: s.sun,
        direction_to_sun: s.direction,
    });
    let volume = bake.bake_probes(lights, settings)?;
    let maps = bake.bake_lightmaps(&triangles, lights, surface_settings)?;
    eprintln!(
        "Baked {} surface charts on {} placements",
        maps.len(),
        placements.len()
    );
    let volume = volume.with_lightmaps(&maps)?;
    let mut file = std::fs::File::create_new(output)?;
    file.write_all(crate::shared::camera::bake_identity(&identity).as_bytes())?;
    file.write_all(&u32::try_from(placements.len())?.to_le_bytes())?;
    for id in placements {
        file.write_all(&id.to_le_bytes())?;
    }
    volume.write(file)?;
    eprintln!(
        "Baked {:?} probe atlas in {:.2}s",
        volume.extent(),
        started.elapsed().as_secs_f64()
    );
    Ok(())
}

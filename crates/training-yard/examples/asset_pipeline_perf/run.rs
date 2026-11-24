use renderer::{Renderer, pipeline::RenderObject};
use renderer_asset::{AssetId, Catalog, Digest, Generation};
use renderer_core::{
    measurement::Snapshot,
    runtime::{ManagedImage, RuntimeConfig},
};
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    num::NonZeroUsize,
    path::Path,
    time::{Duration, Instant},
};
use training_yard::Yard;
#[path = "textures.rs"]
mod textures;
#[path = "../../../renderer-core/tests/support/validation.rs"]
mod validation;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const EXTENT: [u32; 2] = [48, 24];
const WAIT: Duration = Duration::from_secs(30);

pub fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let [api, fixture, id, scratch] = args.as_slice() else {
        return Err("usage: asset_pipeline_perf <vulkan|d3d12> <distribution> <equipment-id> <new-output-dir>".into());
    };
    let config = match api.as_str() {
        #[cfg(feature = "vulkan")]
        "vulkan" => RuntimeConfig::Vulkan(Default::default()),
        #[cfg(all(windows, feature = "d3d12"))]
        "d3d12" => RuntimeConfig::D3D12(Default::default()),
        _ => return Err("backend unavailable".into()),
    };
    let scratch = Path::new(scratch);
    fs::create_dir(scratch)?;
    let id: AssetId = id.parse()?;
    let mut cases = Vec::new();
    let mut reference = None;
    for (entries, owners) in [(1000, 1), (10000, 1), (10000, 100)] {
        let root = scratch.join(format!("scale-{entries}-{owners}"));
        expand_catalog(Path::new(fixture), &root, id, entries)?;
        let (case, counts) = scale(config, &root, id, owners)?;
        if let Some(expected) = reference
            && counts != expected
        {
            return Err(format!(
                "changed work for the same dependency set: {counts:?} vs {expected:?}"
            )
            .into());
        }
        reference = Some(counts);
        cases.push(json!({"entries":entries, "owners":owners, "measurement":case}));
    }
    let mut streams = Vec::new();
    for mode in ["baseline", "concurrent", "throttled", "retained"] {
        streams.push(textures::run(config, Path::new(fixture), scratch, mode)?);
    }
    let report = json!({"api": api,"strict":cfg!(feature="development-validation"),"scale":cases,"streaming":streams});
    fs::write(
        scratch.join("report.json"),
        serde_json::to_vec_pretty(&report)?,
    )?;
    println!("{report}");
    Ok(())
}

fn expand_catalog(source: &Path, root: &Path, selected: AssetId, entries: usize) -> Result<()> {
    let generation = Generation::open(source)?;
    let equipment = generation.equipment(&selected)?.clone();
    fs::create_dir_all(root.join("catalogs"))?;
    fs::create_dir_all(root.join("objects"))?;
    for file in fs::read_dir(source.join("objects"))? {
        let file = file?;
        fs::copy(file.path(), root.join("objects").join(file.file_name()))?;
    }
    // All aliases have the same dependencies. An update should be independent
    // of the number of unused entries in this already indexed generation.
    let mut assets = std::collections::BTreeMap::from([(selected, equipment.clone())]);
    let mut seq = 1;
    while assets.len() < entries {
        assets.insert(AssetId::from_u128(seq), equipment.clone());
        seq += 1;
    }
    let bytes = serde_json::to_vec(&Catalog { version: 1, assets })?;
    let digest = Digest::of(&bytes).to_string();
    fs::write(root.join("catalogs").join(&digest), bytes)?;
    fs::write(root.join("current"), digest)?;
    Ok(())
}

fn scale(
    config: RuntimeConfig,
    root: &Path,
    id: AssetId,
    owners: usize,
) -> Result<(Value, [u64; 5])> {
    let init = Instant::now();
    let generation = Generation::open(root)?;
    let index_us = init.elapsed().as_micros();
    drop(generation);
    let mut yard = Yard::with_asset_catalog(root, owners.max(2))?;
    let mut device = renderer(config, &mut yard, 2)?;
    let start = Instant::now();
    for slot in 0..owners {
        yard.request_equipment(slot, id)?;
    }
    let mut scratch = Vec::with_capacity(16);
    while yard.pending_equipment() {
        yard.advance_equipment(&mut device)?;
        if let Some(err) = yard.load_error() {
            return Err(err.to_string().into());
        }
        frame(&mut device, &yard, &mut scratch, &[])?;
        if start.elapsed() > WAIT {
            return Err("equipment load timed out".into());
        }
    }
    let ready_us = start.elapsed().as_micros();
    for slot in 0..owners {
        if yard.saved_equipment(slot) != Some(id) {
            return Err("owner failed to adopt equipment".into());
        }
    }
    let io = yard.equipment_store().stats();
    let gpu = yard.equipment_stats();
    let counts = [
        io.reads,
        io.bytes,
        gpu.image_uploads,
        gpu.mesh_registrations,
        gpu.material_registrations,
    ];
    if counts.contains(&0) || yard.equipment_lookups() != 1 {
        return Err("expected one indexed lookup and nonempty shared resources".into());
    }
    let value = json!({
        "index_open_us":index_us,
        "ready_us":ready_us,
        "catalog_lookups":yard.equipment_lookups(),
        "reads":io.reads, "bytes_read":io.bytes,
        "image_uploads":gpu.image_uploads, "image_upload_bytes":gpu.image_bytes,
        "meshes":gpu.mesh_registrations, "materials":gpu.material_registrations
    });
    finish(device, yard)?;
    Ok((value, counts))
}

fn renderer(config: RuntimeConfig, yard: &mut Yard, slots: usize) -> Result<Renderer> {
    Renderer::headless_with(
        config,
        training_yard::camera(1, EXTENT)?,
        yard.assets(),
        NonZeroUsize::new(slots).unwrap(),
        Yard::reserve_equipment,
    )
}

fn finish(device: Renderer, yard: Yard) -> Result<()> {
    let mut runtime = device.into_runtime()?;
    yard.release(&mut runtime)?;
    validation::check_after_teardown(runtime);
    Ok(())
}

fn frame(
    device: &mut Renderer,
    yard: &Yard,
    objects: &mut Vec<RenderObject>,
    tiles: &[textures::Tile],
) -> Result<()> {
    use renderer::{
        canvas::{Layer, Paint},
        core::graph::RasterRect,
    };
    let mut frame = device.begin_frame()?;
    let view = *frame.scene_view();
    frame.draw_scene(yard.extract(&view, objects), 1.)?;
    let mut layers = [Layer {
        paint: Paint::Color([0.; 4]),
        rect: RasterRect::new(0, 0, 1, 1),
        clip: None,
    }; textures::COUNT];
    for (i, tile) in tiles.iter().enumerate() {
        let g = frame.graph();
        let image = g.import_image("stream", tile.image.snapshot())?;
        layers[i] = Layer {
            paint: Paint::Image(g.image_view(image, tile.mip, 0)?),
            rect: RasterRect::new(i as u32 * 8, 0, 8, 8),
            clip: None,
        };
    }
    if !tiles.is_empty() {
        frame.draw_overlay(&layers[..tiles.len()])?;
    }
    let capture = frame.capture()?;
    let mut pixels = [0; 48 * 24 * 4];
    if !device.read_capture(&capture, &mut pixels, WAIT)? {
        return Err("frame timeout".into());
    }
    for (i, tile) in tiles.iter().enumerate() {
        let offset = (4 * 48 + i * 8 + 4) * 4;
        if pixels[offset + tile.channel] < 240 {
            return Err(format!("unexpected tile {i}: {:?}", &pixels[offset..offset + 4]).into());
        }
    }
    Ok(())
}

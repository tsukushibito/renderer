//! Opt-in consumer acceptance at explicit change boundaries, outside frame ROI.
//!
//! Pending means that the caller has not observed completion: a fast GPU may
//! already have finished. Native validation and retained owners, not a timing
//! assumption, establish the lifetime contract.

use super::{Error, asset::Scene};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use super::{
    Options, backend,
    camera::{Camera, Conditions},
    pipeline::{self, Budget, Captures, Resident, Workspace},
    program,
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer::passes::output::{SdrEncoding, SdrOutput, write_sdr};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use renderer_core::{
    capacity::CapacityRecovery,
    graph::{CompiledGraph, GraphCapacity, GraphPool, ImageDesc, ImportImage},
    runtime::{ExecutionErrorKind, FrameId, ManagedImage, Runtime},
};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use serde_json::{Value, json};
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
use std::path::Path;

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) const MIP_PROBE_SDR: SdrOutput = SdrOutput {
    encoding: SdrEncoding::Srgb,
    fxaa: false,
};

/// The fixed Bistro cook preserves glTF material order: material 4 is the Vespa
/// shown by the audited material-detail camera. Other fixtures prefer a frequently
/// used opaque material. The GPU pixel-change gate rejects an invisible choice.
pub(super) fn selection(scene: &Scene) -> Result<(usize, usize), Error> {
    if scene.mesh_count == 1296 && scene.primitives.len() == 1591 && scene.materials.len() == 132 {
        let material = &scene.materials[4];
        if material.alpha_zero || material.uniform[48..52] != 0u32.to_le_bytes() {
            return Err("audited Bistro replacement material is no longer opaque".into());
        }
        return Ok((4, material.textures[0]));
    }
    let mut counts = vec![0usize; scene.materials.len()];
    for primitive in &scene.primitives {
        counts[primitive.material] += 1;
    }
    let material = (0..scene.materials.len())
        .filter(|&index| {
            let material = &scene.materials[index];
            !material.alpha_zero && material.uniform[48..52] == 0u32.to_le_bytes()
        })
        .max_by_key(|&index| (counts[index], std::cmp::Reverse(index)))
        .filter(|&index| counts[index] != 0)
        .ok_or("resource checks need a used opaque material")?;
    Ok((material, scene.materials[material].textures[0]))
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
struct Frame {
    id: FrameId,
    plan: CompiledGraph,
    passes: Captures,
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
struct Capture {
    bytes: [Vec<u8>; 3],
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
impl Capture {
    fn matches(&self, other: &Self) -> bool {
        self.bytes == other.bytes
    }
    fn changed_pixels(&self, other: &Self, slot: usize) -> usize {
        let stride = [8, 4, 4][slot];
        self.bytes[slot]
            .chunks_exact(stride)
            .zip(other.bytes[slot].chunks_exact(stride))
            .filter(|(a, b)| a != b)
            .count()
    }
    fn save(&self, directory: &Path, options: &Options) -> Result<Value, Error> {
        std::fs::create_dir_all(directory)?;
        let mut report = json!({});
        for (slot, name, format) in [
            (0, "hdr", "rgba16f-le"),
            (1, "sdr", "rgba8-unorm"),
            (2, "depth", "r32f-le"),
        ] {
            let bytes = &self.bytes[slot];
            if bytes.is_empty() {
                continue;
            }
            let path = directory.join(format!("{name}.bin"));
            std::fs::write(&path, bytes)?;
            report[name] = json!({"path":std::fs::canonicalize(path)?,"format":format,
                "width":options.width,"height":options.height,"byte_length":bytes.len(),
                "sha256":super::digest(bytes)});
        }
        Ok(report)
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "one explicit consumer acceptance context"
)]
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn build_frame(
    resident: &Resident,
    scene: &Scene,
    options: &Options,
    conditions: &Conditions,
    camera: Camera,
    pool: &mut GraphPool,
    workspace: &mut Workspace,
) -> Result<pipeline::Built, Error> {
    pipeline::build(
        pool,
        workspace,
        resident,
        scene,
        options,
        &conditions.frame_input(camera, options.width, options.height)?,
        true,
    )
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn accept_frame(
    backend: &mut Runtime,
    resident: &mut Resident,
    built: pipeline::Built,
) -> Result<Frame, Error> {
    let passes = built
        .captures
        .ok_or("resource check frame needs readback")?;
    let plan = built.graph.compile()?;
    let pending = backend.submit(&plan)?;
    resident.accept_camera(&pending, built.camera_update)?;
    let output = pending.export_image(built.output.raw_image())?;
    backend.delete(output.key())?;
    Ok(Frame {
        id: pending.id(),
        plan,
        passes,
    })
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn finish_frame(backend: &mut Runtime, frame: Frame, options: &Options) -> Result<Capture, Error> {
    let mut completed = backend::wait_for(backend, frame.id)?;
    let pixels = options.width as usize * options.height as usize;
    let mut bytes = [vec![0; pixels * 8], vec![0; pixels * 4], Vec::new()];
    backend::readback(&mut completed, frame.passes.hdr, &mut bytes[0])?;
    backend::readback(&mut completed, frame.passes.sdr, &mut bytes[1])?;
    if let Some(pass) = frame.passes.depth {
        bytes[2].resize(pixels * 4, 0);
        backend::readback(&mut completed, pass, &mut bytes[2])?;
    }
    drop((completed, frame.plan));
    Ok(Capture { bytes })
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn require_stale(backend: &mut Runtime, plan: &CompiledGraph) -> Result<(), Error> {
    match backend.submit(plan) {
        Err(error) if error.kind() == ExecutionErrorKind::StaleResource => Ok(()),
        Err(error) => Err(format!("expected typed stale-resource rejection: {error}").into()),
        Ok(pending) => {
            drop(backend::wait_for(backend, pending.id())?);
            Err("stale resource plan was accepted".into())
        }
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn stale_plans(
    image: &ImportImage,
    buffer: &renderer::pbr::ResidentMaterialUniform,
) -> Result<(GraphPool, CompiledGraph, CompiledGraph), Error> {
    let mut pool = GraphPool::new(GraphCapacity::default(), 2)?;
    let mut graph = pool.begin()?;
    let source = graph.import_image("old material texture", image)?;
    // Export keeps all source mips live without relying on a one-mip readback.
    graph.export_image(source)?;
    let image = graph.compile()?;
    let mut graph = pool.begin()?;
    buffer
        .import(&mut graph, "old material factors")?
        .readback(&mut graph, "old factors")?;
    let buffer = graph.compile()?;
    Ok((pool, image, buffer))
}

fn mip_color(level: usize) -> [u8; 3] {
    [
        24 + (level as u8) * 7,
        172 - (level as u8) * 5,
        80 + (level as u8) * 3,
    ]
}

fn changed_texture(scene: &Scene, image: usize, original: &[u8]) -> Vec<u8> {
    let mut changed = original.to_vec();
    for (level, range) in scene.images[image].mips.iter().enumerate() {
        for rgba in changed[range.clone()].as_chunks_mut::<4>().0 {
            rgba[..3].copy_from_slice(&mip_color(level));
        }
    }
    changed
}

#[cfg(any(test, feature = "vulkan", all(windows, feature = "d3d12")))]
fn expected_encoded(code: u8, srgb: bool) -> u8 {
    if srgb {
        code
    } else {
        let value = f64::from(code) / 255.;
        let encoded = if value <= 0.0031308 {
            12.92 * value
        } else {
            1.055 * value.powf(1. / 2.4) - 0.055
        };
        (encoded * 255.).round() as u8
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "explicit mip-probe inputs outside normal frame processing"
)]
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn mip_probes(
    backend: &mut Runtime,
    resident: &Resident,
    scene: &Scene,
    image_index: usize,
    pool: &mut GraphPool,
    directory: &Path,
) -> Result<Vec<Value>, Error> {
    let desc = pipeline::image_desc(&scene.images[image_index]);
    let mut reports = Vec::new();
    for mip in 0..desc.mip_levels {
        let width = (desc.width >> mip).max(1);
        let height = (desc.height >> mip).max(1);
        let mut graph = pool.begin()?;
        let source = graph.import_image(
            "replacement mip chain",
            resident.images[image_index].snapshot(),
        )?;
        let source = graph.image_view(source, mip, 0)?;
        let output = graph.image("mip probe output", ImageDesc::rgba8(width, height))?;
        write_sdr(&mut graph, "sample_mip", source, output, MIP_PROBE_SDR)?;
        let pass = graph.readback_image("mip texels", output)?;
        let plan = graph.compile()?;
        let pending = backend.submit(&plan)?;
        let mut completed = backend::wait_for(backend, pending.id())?;
        let mut bytes = vec![0; width as usize * height as usize * 4];
        backend::readback(&mut completed, pass, &mut bytes)?;
        let expected = mip_color(mip as usize)
            .map(|code| expected_encoded(code, scene.images[image_index].srgb));
        let mut max_error = 0u8;
        for rgba in bytes.as_chunks::<4>().0 {
            for (actual, expected) in rgba[..3].iter().zip(expected) {
                max_error = max_error.max(actual.abs_diff(expected));
            }
            if rgba[3] != 255 {
                return Err("mip probe did not write opaque output".into());
            }
        }
        if max_error > 1 {
            return Err(format!("replacement mip {mip} pixel mismatch: {max_error} codes").into());
        }
        std::fs::create_dir_all(directory)?;
        let path = directory.join(format!("mip-{mip}.bin"));
        std::fs::write(&path, &bytes)?;
        reports.push(json!({"level":mip,"width":width,"height":height,"format":"rgba8-unorm",
            "pixel_count":u64::from(width)*u64::from(height),"expected_rgb":expected,"max_code_error":max_error,
            "path":std::fs::canonicalize(path)?,"byte_length":bytes.len(),"sha256":super::digest(&bytes),"passed":true}));
    }
    Ok(reports)
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn upload_texture(
    backend: &mut Runtime,
    pool: &mut GraphPool,
    desc: ImageDesc,
    bytes: &[u8],
) -> Result<ManagedImage, Error> {
    let (frame, texture) = backend.upload_texture(pool, "material texture version", desc, bytes)?;
    drop(backend::wait_for(backend, frame)?);
    Ok(texture)
}

#[allow(
    clippy::too_many_arguments,
    reason = "opt-in acceptance shares the prepared full-scene context"
)]
#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
pub(super) fn execute(
    backend: &mut Runtime,
    resident: &mut Resident,
    scene: &Scene,
    options: &Options,
    conditions: &Conditions,
    budget: &Budget,
    workspace: &mut Workspace,
    stages: Value,
) -> Result<Value, Error> {
    let directory = options.output.join("resource-checks");
    let cameras = if options.fixture {
        let first = Camera::fixture();
        let mut second = first;
        second.fov *= 0.8;
        [first, second]
    } else {
        [
            conditions.camera("street", 0)?,
            conditions.camera("material-detail", 0)?,
        ]
    };
    let names = if options.fixture {
        ["fixture", "fixture-narrow"]
    } else {
        ["street", "material-detail"]
    };
    let mut pool = GraphPool::new(budget.graph, options.inflight + 3)?;
    let mut references = Vec::new();
    let mut reference_reports = Vec::new();
    for index in 0..2 {
        let built = build_frame(
            resident,
            scene,
            options,
            conditions,
            cameras[index],
            &mut pool,
            workspace,
        )?;
        let frame = accept_frame(backend, resident, built)?;
        let capture = finish_frame(backend, frame, options)?;
        reference_reports
            .push(capture.save(&directory.join(format!("serial-{}", names[index])), options)?);
        references.push(capture);
    }
    if references[0].bytes[1] == references[1].bytes[1] {
        return Err("distinct View probes produced identical SDR images".into());
    }
    let mut queued = Vec::new();
    for index in 0..options.inflight {
        let built = build_frame(
            resident,
            scene,
            options,
            conditions,
            cameras[index % 2],
            &mut pool,
            workspace,
        )?;
        queued.push((index % 2, accept_frame(backend, resident, built)?));
    }
    // No poll/wait has run since the first queued submission was accepted.
    let built = build_frame(
        resident, scene, options, conditions, cameras[0], &mut pool, workspace,
    )?;
    let retry_camera = built.camera_update;
    let retry_output = built.output.raw_image();
    let retry_passes = built.captures.unwrap();
    let retry_plan = built.graph.compile()?;
    let error = match backend.submit(&retry_plan) {
        Err(error) => error,
        Ok(pending) => {
            drop(backend::wait_for(backend, pending.id())?);
            return Err("configured execution capacity was silently exceeded".into());
        }
    };
    let capacity = error
        .capacity()
        .ok_or("capacity probe returned an untyped error")?;
    if !matches!(
        capacity.recovery,
        CapacityRecovery::Poll | CapacityRecovery::ReleaseOwners | CapacityRecovery::ReleaseResults
    ) {
        return Err(format!("unexpected capacity recovery: {capacity}").into());
    }
    let (first_view, first) = queued.remove(0);
    let first = finish_frame(backend, first, options)?;
    if !first.matches(&references[first_view]) {
        return Err("overlapping View changed the first camera capture".into());
    }
    let retry = backend.submit(&retry_plan)?;
    resident.accept_camera(&retry, retry_camera)?;
    let output = retry.export_image(retry_output)?;
    backend.delete(output.key())?;
    let id = retry.id();
    drop(retry);
    let retry = Frame {
        id,
        plan: retry_plan,
        passes: retry_passes,
    };
    let mut overlap_reports = vec![first.save(&directory.join("overlap-0"), options)?];
    for (index, (view, frame)) in queued.into_iter().enumerate() {
        let capture = finish_frame(backend, frame, options)?;
        if !capture.matches(&references[view]) {
            return Err("overlapping View differs from its serial reference".into());
        }
        overlap_reports
            .push(capture.save(&directory.join(format!("overlap-{}", index + 1)), options)?);
    }
    let retry = finish_frame(backend, retry, options)?;
    if !retry.matches(&references[0]) {
        return Err("retry after capacity recovery changed pixels".into());
    }
    let retry_report = retry.save(&directory.join("capacity-retry"), options)?;

    let (material_index, image_index) = selection(scene)?;
    let original_texture = scene.images[image_index].load_bytes()?;
    let changed_texture = changed_texture(scene, image_index, &original_texture);
    let original_material = scene.materials[material_index].uniform;
    let mut changed_material = original_material;
    // Strong emission is a sensitivity probe, never a source material override.
    for (word, value) in [4usize, 5, 6].into_iter().zip([1.5_f32, 0.05, 0.5]) {
        changed_material[word * 4..word * 4 + 4].copy_from_slice(&value.to_le_bytes());
    }
    let mut upload_pool = budget.upload_pool()?;
    let mut cycles = Vec::new();
    let mut memory_samples = Vec::new();
    let desc = pipeline::image_desc(&scene.images[image_index]);
    for cycle in 0..4 {
        // Target the material-detail View, with all source assets still resident.
        let built = build_frame(
            resident, scene, options, conditions, cameras[1], &mut pool, workspace,
        )?;
        let old_frame = accept_frame(backend, resident, built)?;
        let old_image = resident.images[image_index].snapshot().clone();
        let (stale_pool, stale_image, stale_buffer) =
            stale_plans(&old_image, &resident.materials[material_index])?;
        backend.delete(old_image.key())?;
        resident.images[image_index] =
            upload_texture(backend, &mut upload_pool, desc, &changed_texture)?;
        require_stale(backend, &stale_image)?;
        resident.images[image_index].refresh(backend)?;
        resident.refresh_surfaces()?;
        // Isolate the texture binding: the original material is still registered
        // and the earlier frame owner has not observed completion. A simultaneous
        // material-factor change must not mask a missing texture-reference update.
        let built = build_frame(
            resident, scene, options, conditions, cameras[1], &mut pool, workspace,
        )?;
        let frame = accept_frame(backend, resident, built)?;
        let texture_only = finish_frame(backend, frame, options)?;
        let texture_hdr_changed = texture_only.changed_pixels(&references[1], 0);
        let texture_sdr_changed = texture_only.changed_pixels(&references[1], 1);
        if texture_hdr_changed == 0 || texture_sdr_changed == 0 {
            return Err("replacement texture binding did not change the diagnostic View".into());
        }
        if texture_only.bytes[2] != references[1].bytes[2] {
            return Err("texture-only replacement changed scene depth".into());
        }
        let texture_only_report = texture_only.save(
            &directory.join(format!("cycle-{cycle}-texture-only")),
            options,
        )?;
        resident.materials[material_index].release(backend)?;
        let mut new_owners = Vec::new();
        resident.materials[material_index] =
            pipeline::upload_material(backend, &mut new_owners, &changed_material)?;
        resident.refresh_surfaces()?;
        require_stale(backend, &stale_buffer)?;
        let pending_memory = backend::memory_snapshot(backend)?;
        let expected_retained_bytes = desc
            .byte_len()
            .and_then(|bytes| bytes.checked_add(program::MATERIAL_BYTES as u64))
            .ok_or("pending replacement logical byte count overflow")?;
        if pending_memory["logical"]["retained_bytes"]
            .as_u64()
            .is_none_or(|bytes| bytes < expected_retained_bytes)
        {
            return Err(
                "old pending texture/material owners are missing from retained memory accounting"
                    .into(),
            );
        }
        let old_capture = finish_frame(backend, old_frame, options)?;
        if !old_capture.matches(&references[1]) {
            return Err("pending old View did not retain the old material and texture".into());
        }
        drop((stale_image, stale_buffer, stale_pool, old_image));
        let built = build_frame(
            resident, scene, options, conditions, cameras[1], &mut pool, workspace,
        )?;
        let frame = accept_frame(backend, resident, built)?;
        let changed = finish_frame(backend, frame, options)?;
        let hdr_changed = changed.changed_pixels(&references[1], 0);
        let sdr_changed = changed.changed_pixels(&references[1], 1);
        let material_hdr_changed = changed.changed_pixels(&texture_only, 0);
        let material_sdr_changed = changed.changed_pixels(&texture_only, 1);
        if material_hdr_changed == 0 || material_sdr_changed == 0 {
            return Err("replacement material binding did not change the texture-only View".into());
        }
        if hdr_changed == 0 || sdr_changed == 0 {
            return Err("replacement material is not visible in the diagnostic View".into());
        }
        if changed.bytes[2] != references[1].bytes[2] {
            return Err("opaque material replacement changed scene depth".into());
        }
        let probes = mip_probes(
            backend,
            resident,
            scene,
            image_index,
            &mut pool,
            &directory.join(format!("cycle-{cycle}-mips")),
        )?;
        let changed_report =
            changed.save(&directory.join(format!("cycle-{cycle}-changed")), options)?;
        backend.delete(resident.images[image_index].snapshot().key())?;
        resident.materials[material_index].release(backend)?;
        resident.images[image_index] =
            upload_texture(backend, &mut upload_pool, desc, &original_texture)?;
        let mut restored_owners = Vec::new();
        resident.materials[material_index] =
            pipeline::upload_material(backend, &mut restored_owners, &original_material)?;
        resident.refresh_surfaces()?;
        // The current material's original owner is released only at this change
        // boundary. Pending work retained its own immutable provenance as needed.
        resident.upload_owners[material_index] = restored_owners.remove(0);
        drop(new_owners);
        let built = build_frame(
            resident, scene, options, conditions, cameras[1], &mut pool, workspace,
        )?;
        let frame = accept_frame(backend, resident, built)?;
        let restored = finish_frame(backend, frame, options)?;
        if !restored.matches(&references[1]) {
            return Err("restored assets differ from their serial reference".into());
        }
        let restored_report =
            restored.save(&directory.join(format!("cycle-{cycle}-restored")), options)?;
        upload_pool.release_idle();
        let memory = backend::memory_snapshot(backend)?;
        memory_samples.push(memory.clone());
        cycles.push(json!({"cycle":cycle,"old_pending_matches":true,"stale_image_rejected":true,"stale_buffer_rejected":true,
            "changed_pixels":{"hdr":hdr_changed,"sdr":sdr_changed},"depth_unchanged":true,"restored_matches":true,
            "texture_changed_pixels":{"hdr":texture_hdr_changed,"sdr":texture_sdr_changed},
            "material_changed_pixels":{"hdr":material_hdr_changed,"sdr":material_sdr_changed},
            "pending_memory":pending_memory,"expected_retained_bytes":expected_retained_bytes,
            "texture_only_captures":texture_only_report,"changed_captures":changed_report,
            "restored_captures":restored_report,"mip_probes":probes,"memory":memory}));
    }
    let plateau = super::memory::plateau(&memory_samples)?;
    let mut stages = stages;
    stages["after_checks"] = backend::memory_snapshot(backend)?;
    Ok(json!({"passed":true,"stages":stages,
        "views":{"names":names,"queued_before_completion_observation":options.inflight,"serial_matches":true,
            "serial_captures":reference_reports,"overlapping_captures":overlap_reports,
            "capacity":{"resource":format!("{:?}",capacity.resource),"recovery":format!("{:?}",capacity.recovery),
                "limit":capacity.limit,"required":capacity.required,"retry_matches":true,"retry_captures":retry_report}},
        "replacement":{"material_index":material_index,"image_index":image_index,"mip_levels":desc.mip_levels,"cycles":cycles},
        "memory_samples":memory_samples,"plateau":plateau,
        "scope":"full resident source set; explicit change boundary outside steady-state ROI; pending means completion not yet observed, not proof GPU is still executing; all-mip RGB sampling probes preserve source alpha and test every texel via nearest sampling; original assets restored"}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacement_choice_ignores_invisible_alpha_materials_and_changes_every_mip_rgb() {
        let scene = Scene::fixture(false);
        let (material, image) = selection(&scene).unwrap();
        assert_eq!(material, 0);
        assert!(!scene.materials[material].alpha_zero);
        let original = scene.images[image].load_bytes().unwrap();
        let changed = changed_texture(&scene, image, &original);
        for (a, b) in original
            .as_chunks::<4>()
            .0
            .iter()
            .zip(changed.as_chunks::<4>().0)
        {
            assert_eq!(a[3], b[3]);
            assert_eq!(&b[..3], &mip_color(0));
        }
        assert_eq!(expected_encoded(128, true), 128);
        assert_eq!(expected_encoded(128, false), 188);
    }

    #[test]
    fn replacement_updates_distinct_mip_contents_without_touching_alpha() {
        let mut scene = Scene::fixture(false);
        let (_, image) = selection(&scene).unwrap();
        scene.images[image].mips = vec![0..16, 16..20];
        let original: Vec<_> = (0..5).flat_map(|index| [0, 0, 0, index]).collect();
        let changed = changed_texture(&scene, image, &original);
        assert_eq!(&changed[..3], &mip_color(0));
        assert_eq!(&changed[16..19], &mip_color(1));
        for (a, b) in original
            .as_chunks::<4>()
            .0
            .iter()
            .zip(changed.as_chunks::<4>().0)
        {
            assert_eq!(a[3], b[3]);
        }
    }
}

use super::{DisplayScene, PerspectiveView};
use crate::{
    BlobRef, ImageBlob, MaterialSlot, ObjectStore,
    scene::{SceneStreaming, StreamDetail},
};
use glam::Vec3;
use sashiko_renderer::{
    core::{
        graph::{BufferDesc, ImageDesc},
        program::SamplerDescriptor,
        runtime::{FrameId, ManagedImage},
    },
    material::{MaterialHandle, MaterialTextures},
    mesh::{MeshData, MeshHandle},
    pbr::Bounds,
    pipeline::{
        AssetCandidate, AssetPreparation, PipelineResult, RenderObject, RetireAssets, SceneAssets,
        UploadBudget,
    },
};
use std::{
    sync::{Arc, Weak},
    task::Poll,
    thread::JoinHandle,
    time::{Duration, Instant},
};

type Pair = (MeshHandle, MaterialHandle);
// Called only when admitting a new immutable version. Counts matter when several
// simultaneously live images or buffers use the same native resource class.
fn covers<T: PartialEq>(prepared: &[T], requested: &[T]) -> bool {
    let mut available = prepared.iter().collect::<Vec<_>>();
    requested.iter().all(|desc| {
        if let Some(index) = available.iter().position(|&item| item == desc) {
            available.swap_remove(index);
            true
        } else {
            false
        }
    })
}
struct Member {
    object: usize,
    center: Vec3,
    diameter: f32,
    near: bool,
}
struct Section {
    members: std::ops::Range<usize>,
    center: Vec3,
    radius: f32,
}
struct Group {
    detail: StreamDetail,
    members: Vec<Member>,
    sections: Vec<Section>,
    base: Option<Pair>,
    live: Option<Resident>,
    weight: f32,
    wanted: bool,
    geometry_cost: u64,
    textures: [Option<usize>; 4],
    owns_mesh: bool,
    retry: Option<Instant>,
}
struct Resident {
    pair: Pair,
    buffers: Vec<BufferDesc>,
    _images: [Option<Arc<ManagedImage>>; 4],
}
struct Decoded {
    mesh: Option<MeshData>,
    buffers: Vec<BufferDesc>,
    maps: [Option<ImageBlob>; 4],
    reserved: bool,
    uploaded: [Option<Arc<ManagedImage>>; 4],
    pending: Option<FrameId>,
}
enum Change {
    Read(usize, JoinHandle<Result<Decoded, String>>),
    Upload(usize, Box<Decoded>),
    Register(usize, Resident, Box<AssetCandidate>),
}
struct SharedTexture {
    owner: Weak<ManagedImage>,
    selected: bool,
    cost: u64,
}
pub(super) struct Stream {
    textures: Vec<SharedTexture>,
    groups: Vec<Group>,
    order: Vec<usize>,
    assets: usize,
    bytes: u64,
    transfer: u64,
    images: Vec<ImageDesc>,
    prepared_buffers: Vec<BufferDesc>,
    prepared_images: Vec<ImageDesc>,
    samplers: Vec<SamplerDescriptor>,
    change: Option<Change>,
    #[cfg(feature = "metrics")]
    metrics: crate::scene::StreamingMetrics,
}

fn retryable(err: &sashiko_renderer::AssetPrepareError) -> bool {
    use sashiko_renderer::CapacityRecovery::{Poll, ReleaseOwners, ReleaseResults};
    err.capacity()
        .is_some_and(|c| matches!(c.recovery(), Poll | ReleaseOwners | ReleaseResults))
}

fn decode(
    store: &ObjectStore,
    detail: &StreamDetail,
    base: [Option<BlobRef>; 4],
    transfer: u64,
    owns_mesh: bool,
    uploaded: [Option<Arc<ManagedImage>>; 4],
) -> PipelineResult<Decoded> {
    let (mesh, buffers) = if owns_mesh {
        let src = store.load(&detail.mesh)?;
        let blob = src.mesh()?;
        let actual = blob.local_bounds()?;
        if (0..3).any(|axis| {
            actual.min()[axis] < detail.bounds[0][axis]
                || actual.max()[axis] > detail.bounds[1][axis]
        }) {
            return Err("stream mesh exceeds its registered bounds".into());
        }
        let mut mesh = blob.mesh_data()?;
        let mut buffers = vec![
            BufferDesc {
                size: blob.vertices.len() as u64
                    * sashiko_renderer::passes::pbr::VERTEX_BYTES as u64,
            },
            BufferDesc {
                size: blob.indices.len() as u64 * 4,
            },
        ];
        if let Some(uv) = &detail.lightmap_uvs {
            let data = store.load(uv)?;
            let (pairs, tail) = data.as_chunks::<8>();
            if !tail.is_empty() || pairs.len() != blob.vertices.len() {
                return Err("invalid streaming lightmap UVs".into());
            }
            let uvs: Vec<_> = pairs
                .iter()
                .map(|b| {
                    [
                        f32::from_le_bytes(b[..4].try_into().unwrap()),
                        f32::from_le_bytes(b[4..].try_into().unwrap()),
                    ]
                })
                .collect();
            mesh = mesh.with_lightmap_uvs(&uvs)?;
            buffers.push(BufferDesc {
                size: data.len() as u64,
            });
        }
        (Some(mesh), buffers)
    } else {
        (None, Vec::new())
    };
    let mut maps = std::array::from_fn(|_| None);
    for slot in MaterialSlot::ALL {
        let i = slot.index();
        if uploaded[i].is_none()
            && detail.images[i] != base[i]
            && let Some(reference) = detail.images[i]
        {
            let owner = store.load(&reference)?;
            let image = owner.image()?;
            image.texture_desc(slot)?;
            let candidate = fitting_mips(image, transfer);
            if candidate.as_ref().is_some_and(|img| {
                base[i].is_none_or(|fallback| img.pixels.len() as u64 + 24 > fallback.size)
                    || image.pixels.len() as u64 <= transfer
            }) {
                maps[i] = candidate;
            }
        }
    }
    Ok(Decoded {
        mesh,
        buffers,
        maps,
        reserved: false,
        uploaded,
        pending: None,
    })
}

// Keep a complete suffix so the sampler can use every coarser level.
// A partial authored chain may have no level small enough for this budget.
fn fitting_mips(image: &ImageBlob, budget: u64) -> Option<ImageBlob> {
    let (mut width, mut height) = (image.width, image.height);
    let mut offset = 0;
    for levels in (1..=image.mip_levels).rev() {
        if (image.pixels.len() - offset) as u64 <= budget {
            return Some(ImageBlob {
                width,
                height,
                mip_levels: levels,
                srgb: image.srgb,
                pixels: image.pixels[offset..].to_vec(),
            });
        }
        offset += width as usize * height as usize * 4;
        width = (width / 2).max(1);
        height = (height / 2).max(1);
    }
    None
}

impl Stream {
    pub(super) fn new(
        config: SceneStreaming,
        primitives: &[crate::Primitive],
    ) -> PipelineResult<Self> {
        let mut groups = Vec::with_capacity(config.details.len());
        let mut texture_ids = std::collections::HashMap::new();
        let mut textures = Vec::new();
        for detail in config.details {
            let base = &primitives[detail.primitive];
            let owns_mesh = base.mesh != detail.mesh;
            let geometry = if owns_mesh {
                detail
                    .mesh
                    .size
                    .checked_add(detail.lightmap_uvs.map_or(0, |uv| uv.size))
                    .ok_or("stream geometry size overflow")?
            } else {
                0
            };
            let geometry_cost = geometry
                .checked_add(64)
                .ok_or("stream material size overflow")?;
            let indices = std::array::from_fn(|slot| {
                let image = detail.images[slot]?;
                if Some(image) == base.images[slot] {
                    return None;
                }
                Some(
                    *texture_ids
                        .entry((image, base.images[slot], slot))
                        .or_insert_with(|| {
                            textures.push(SharedTexture {
                                owner: Weak::new(),
                                selected: false,
                                cost: image.size.min(config.transfer_bytes),
                            });
                            textures.len() - 1
                        }),
                )
            });
            groups.push(Group {
                detail,
                members: Vec::new(),
                sections: Vec::new(),
                base: None,
                live: None,
                weight: 0.,
                wanted: false,
                geometry_cost,
                textures: indices,
                owns_mesh,
                retry: None,
            });
        }
        Ok(Self {
            textures,
            order: (0..groups.len()).collect(),
            assets: config.max_resident.min(groups.len()),
            groups,
            bytes: config.resident_bytes,
            transfer: config.transfer_bytes,
            images: Vec::new(),
            prepared_buffers: config
                .reservation
                .buffers
                .into_iter()
                .map(|size| BufferDesc { size })
                .collect(),
            prepared_images: config
                .reservation
                .images
                .into_iter()
                .map(|img| {
                    let mut desc = if img.srgb {
                        ImageDesc::rgba8_srgb(img.width, img.height)
                    } else {
                        ImageDesc::rgba8(img.width, img.height)
                    };
                    desc.mip_levels = img.levels;
                    desc
                })
                .collect(),
            samplers: Vec::new(),
            change: None,
            #[cfg(feature = "metrics")]
            metrics: Default::default(),
        })
    }
    pub(super) fn bounds(&self, primitive: usize) -> Option<[[f32; 3]; 2]> {
        self.groups
            .iter()
            .find(|g| g.detail.primitive == primitive)
            .map(|g| g.detail.bounds)
    }
    pub(super) fn slots(&self) -> usize {
        self.assets
    }
    pub(super) fn settled(&self) -> bool {
        self.change.is_none() && self.groups.iter().all(|g| g.wanted == g.live.is_some())
    }
    #[cfg(feature = "metrics")]
    pub(super) fn metrics(&self) -> crate::scene::StreamingMetrics {
        let mut metrics = self.metrics;
        for group in self.groups.iter().filter(|g| g.live.is_some()) {
            metrics.resident_groups += 1;
            metrics.charged_bytes += group.geometry_cost;
        }
        metrics.charged_bytes += self
            .textures
            .iter()
            .filter(|t| t.owner.strong_count() > 0)
            .map(|t| t.cost)
            .sum::<u64>();
        metrics
    }
    pub(super) fn finish(
        &mut self,
        runtime: &mut sashiko_renderer::core::runtime::Runtime,
    ) -> PipelineResult<()> {
        match self.change.take() {
            Some(Change::Read(_, reader)) => {
                let _ = reader.join().map_err(|_| "scene detail reader panicked")?;
            }
            Some(Change::Upload(_, data)) => {
                if let Some(frame) = data.pending {
                    runtime
                        .wait_for_frame(frame, Duration::from_secs(120))?
                        .ok_or("timed out releasing stream upload")?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    pub(super) fn placement(
        &mut self,
        primitive: usize,
        object: usize,
        bounds: Bounds,
        mesh: MeshHandle,
        material: MaterialHandle,
    ) {
        if let Some(g) = self
            .groups
            .iter_mut()
            .find(|g| g.detail.primitive == primitive)
        {
            g.base = Some((mesh, material));
            g.members.push(Member {
                object,
                center: bounds.center().into(),
                diameter: (Vec3::from(bounds.max()) - Vec3::from(bounds.min())).length(),
                near: false,
            });
        }
    }
    pub(super) fn base_reservations(
        &mut self,
        images: &[ImageDesc],
        samplers: &[SamplerDescriptor],
    ) {
        self.images = images.to_vec();
        self.prepared_images.extend_from_slice(images);
        self.samplers = samplers.to_vec();
        for group in &mut self.groups {
            let cell = |p: &Member| (p.center / 8.).floor().as_ivec3().to_array();
            group.members.sort_unstable_by_key(cell);
            let mut first = 0;
            for members in group.members.chunk_by(|a, b| cell(a) == cell(b)) {
                let mut lo = Vec3::splat(f32::INFINITY);
                let mut hi = Vec3::splat(f32::NEG_INFINITY);
                for m in members {
                    let radius = Vec3::splat(m.diameter * 0.5);
                    lo = lo.min(m.center - radius);
                    hi = hi.max(m.center + radius);
                }
                let end = first + members.len();
                group.sections.push(Section {
                    members: first..end,
                    center: (lo + hi) * 0.5,
                    radius: (hi - lo).length() * 0.5,
                });
                first = end;
            }
        }
    }
    pub(super) fn initial_classes(&self) -> (&[BufferDesc], &[ImageDesc]) {
        (&self.prepared_buffers, &self.prepared_images)
    }
    pub(super) fn preload_live(&self, assets: &mut SceneAssets<'_>) {
        for group in &self.groups {
            if let Some((mesh, material)) = group.base {
                assets.preload_pair(mesh, material);
            }
            if let Some(r) = &group.live {
                assets.preload_pair(r.pair.0, r.pair.1);
            }
        }
    }
    fn select(
        &mut self,
        scene: &mut DisplayScene,
        view: &PerspectiveView,
        height: u32,
        full: bool,
    ) {
        let eye = Vec3::from(view.position);
        let forward = Vec3::from(view.forward).normalize_or_zero();
        let focal = 0.5 * height as f32 / (0.5 * view.vertical_fov_degrees.to_radians()).tan();
        let now = Instant::now();
        for g in &mut self.groups {
            g.weight = 0.;
            g.wanted = false;
            let Some(base) = g.base else { continue };
            for section in &g.sections {
                let ahead = full || (section.center - eye).dot(forward) >= -section.radius;
                for m in &mut g.members[section.members.clone()] {
                    let offset = m.center - eye;
                    let pixels = focal * m.diameter / offset.length().max(0.02);
                    m.near =
                        full || pixels >= g.detail.min_pixels * if m.near { 0.95 } else { 1.05 };
                    let pair = if m.near {
                        g.live.as_ref().map_or(base, |r| r.pair)
                    } else {
                        base
                    };
                    scene.objects[m.object].mesh = pair.0;
                    scene.objects[m.object].material = pair.1;
                    if ahead && (full || offset.dot(forward) > -m.diameter) {
                        g.weight = g.weight.max(pixels / g.detail.min_pixels);
                    }
                }
            }
            if g.live.is_some() {
                g.weight *= 1.1;
            }
        }
        let groups = &self.groups;
        self.order.sort_unstable_by(|&a, &b| {
            groups[b]
                .weight
                .total_cmp(&groups[a].weight)
                .then(a.cmp(&b))
        });
        let mut room = self.bytes;
        let mut slots = self.assets;
        let loading = self.change.as_ref().map(|change| match change {
            Change::Read(i, _) | Change::Upload(i, _) | Change::Register(i, _, _) => *i,
        });
        for texture in &mut self.textures {
            texture.selected = false;
        }
        // Admit visible and approaching detail first. Retain cached groups in
        // spare capacity so a turn away and back does not force another upload.
        for retain_cached in [false, true] {
            for &index in &self.order {
                let g = &mut self.groups[index];
                if slots == 0 {
                    break;
                }
                let eligible = if retain_cached {
                    g.live.is_some() || loading == Some(index)
                } else {
                    full || g.weight >= 0.8
                };
                if g.wanted || !eligible || g.base.is_none() || g.retry.is_some_and(|t| now < t) {
                    continue;
                }
                let cost = g
                    .textures
                    .iter()
                    .flatten()
                    .filter(|&&t| !self.textures[t].selected)
                    .try_fold(g.geometry_cost, |n, &t| {
                        n.checked_add(self.textures[t].cost)
                    });
                if let Some(cost) = cost.filter(|&cost| cost <= room) {
                    g.wanted = true;
                    slots -= 1;
                    room -= cost;
                    for &t in g.textures.iter().flatten() {
                        self.textures[t].selected = true;
                    }
                }
            }
        }
    }
    pub(super) fn tick(
        &mut self,
        scene: &mut DisplayScene,
        renderer: &mut impl AssetPreparation,
        view: &PerspectiveView,
        height: u32,
        full: bool,
    ) -> PipelineResult<()> {
        self.select(scene, view, height, full);
        if let Some(change) = self.change.take() {
            self.change = self.advance(change, scene, renderer)?;
            return Ok(());
        }
        if let Some(g) = self
            .groups
            .iter_mut()
            .find(|g| g.live.is_some() && !g.wanted)
        {
            let r = g.live.as_ref().unwrap();
            let pair = r.pair;
            let mut empty = AssetCandidate::new(&scene.meshes, &scene.materials, &[])?;
            renderer.commit_candidate(
                &mut empty,
                &mut scene.meshes,
                &mut scene.materials,
                RetireAssets {
                    pairs: &[pair],
                    meshes: if g.owns_mesh {
                        std::slice::from_ref(&pair.0)
                    } else {
                        &[]
                    },
                    materials: &[pair.1],
                },
            )?;
            let base = g.base.unwrap();
            for m in &g.members {
                scene.objects[m.object].mesh = base.0;
                scene.objects[m.object].material = base.1;
            }
            g.live = None;
            #[cfg(feature = "metrics")]
            {
                self.metrics.evictions += 1;
            }
            renderer.collect_assets(&mut scene.meshes, &mut scene.materials)?;
            return Ok(());
        }
        if let Some(&i) = self
            .order
            .iter()
            .find(|&&i| self.groups[i].wanted && self.groups[i].live.is_none())
        {
            let g = &self.groups[i];
            let (_, _, refs) = scene
                .paints
                .iter()
                .find(|e| e.0 == g.base.unwrap().1)
                .unwrap();
            let base = *refs;
            let store = scene.store.clone();
            let detail = g.detail.clone();
            let owns_mesh = g.owns_mesh;
            let limit = self.transfer;
            let shared = g
                .textures
                .map(|t| t.and_then(|index| self.textures[index].owner.upgrade()));
            self.change = Some(Change::Read(
                i,
                std::thread::Builder::new().spawn(move || {
                    decode(&store, &detail, base, limit, owns_mesh, shared)
                        .map_err(|e| e.to_string())
                })?,
            ));
        }
        Ok(())
    }
    fn advance(
        &mut self,
        change: Change,
        scene: &mut DisplayScene,
        renderer: &mut impl AssetPreparation,
    ) -> PipelineResult<Option<Change>> {
        match change {
            Change::Read(i, job) => {
                if !job.is_finished() {
                    return Ok(Some(Change::Read(i, job)));
                }
                match job.join().map_err(|_| "scene detail reader panicked")? {
                    Ok(data) if self.groups[i].wanted => {
                        Ok(Some(Change::Upload(i, Box::new(data))))
                    }
                    Ok(_) => {
                        #[cfg(feature = "metrics")]
                        {
                            self.metrics.cancellations += 1;
                        }
                        Ok(None)
                    }
                    Err(e) => {
                        #[cfg(feature = "metrics")]
                        {
                            self.metrics.read_errors += 1;
                        }
                        eprintln!(
                            "Scene detail {} unavailable: {e}",
                            self.groups[i].detail.primitive
                        );
                        self.groups[i].retry = Some(Instant::now() + Duration::from_secs(3));
                        Ok(None)
                    }
                }
            }
            Change::Upload(i, mut d) => {
                if let Some(id) = d.pending {
                    if !renderer.poll_texture_upload(id)? {
                        return Ok(Some(Change::Upload(i, d)));
                    }
                    d.pending = None;
                }
                if !self.groups[i].wanted {
                    #[cfg(feature = "metrics")]
                    {
                        self.metrics.cancellations += 1;
                    }
                    return Ok(None);
                }
                if !d.reserved {
                    if renderer.collect_assets(&mut scene.meshes, &mut scene.materials)? != 0 {
                        return Ok(Some(Change::Upload(i, d)));
                    }
                    let mut buffers = d.buffers.clone();
                    let mut images = self.images.clone();
                    for r in self.groups.iter().filter_map(|g| g.live.as_ref()) {
                        buffers.extend_from_slice(&r.buffers);

                        buffers.push(BufferDesc {
                            size: sashiko_renderer::passes::pbr::MATERIAL_BYTES as u64,
                        });
                    }
                    buffers.push(BufferDesc {
                        size: sashiko_renderer::passes::pbr::MATERIAL_BYTES as u64,
                    });
                    images.extend(
                        self.textures
                            .iter()
                            .filter_map(|t| t.owner.upgrade().map(|img| img.snapshot().desc())),
                    );
                    for slot in MaterialSlot::ALL {
                        if let Some(img) = &d.maps[slot.index()] {
                            images.push(img.texture_desc(slot)?);
                        }
                    }
                    if !covers(&self.prepared_buffers, &buffers)
                        || !covers(&self.prepared_images, &images)
                    {
                        match renderer.replace_asset_reservations(
                            "scene texture",
                            &buffers,
                            &images,
                            &self.samplers,
                        ) {
                            Ok(Poll::Ready(())) => {}
                            Ok(Poll::Pending) => return Ok(Some(Change::Upload(i, d))),
                            Err(e) if retryable(&e) => return Ok(Some(Change::Upload(i, d))),
                            Err(e) => return Err(e.into()),
                        }
                        self.prepared_buffers = buffers;
                        self.prepared_images = images;
                    }
                    d.reserved = true;
                }
                for slot in MaterialSlot::ALL {
                    let k = slot.index();
                    if let Some(img) = &d.maps[k]
                        && d.uploaded[k].is_none()
                    {
                        let desc = img.texture_desc(slot)?;
                        renderer.check_texture_support(desc, true)?;
                        let (frame, owner) = match renderer.upload_texture_mips(
                            "scene texture",
                            desc,
                            0..desc.mip_levels,
                            &img.pixels,
                        ) {
                            Ok(v) => v,
                            Err(e) if retryable(&e) => return Ok(Some(Change::Upload(i, d))),
                            Err(e) => return Err(e.into()),
                        };
                        d.uploaded[k] = Some(Arc::new(owner));
                        d.pending = Some(frame);
                        #[cfg(feature = "metrics")]
                        {
                            self.metrics.submitted_texture_bytes += img.pixels.len() as u64;
                        }
                        return Ok(Some(Change::Upload(i, d)));
                    }
                }
                let base = self.groups[i].base.unwrap();
                let mut params = scene
                    .paints
                    .iter()
                    .find(|paint| paint.0 == base.1)
                    .unwrap()
                    .1
                    .clone();
                let texture = |k: usize, existing: &Option<_>| {
                    self.groups[i].detail.images[k]?;
                    d.uploaded[k]
                        .as_ref()
                        .map(|r| r.snapshot().clone().into())
                        .or_else(|| existing.clone())
                };
                params.textures = MaterialTextures {
                    base_color: texture(0, &params.textures.base_color),
                    normal: texture(1, &params.textures.normal),
                    orm: texture(2, &params.textures.orm),
                    emissive: texture(3, &params.textures.emissive),
                };
                let mesh = match d.mesh.take() {
                    Some(data) => scene.meshes.create(data)?,
                    None => base.0,
                };
                let mat = match scene.materials.create_pbr(params) {
                    Ok(mat) => mat,
                    Err(e) => {
                        if self.groups[i].owns_mesh {
                            scene.meshes.remove(mesh)?;
                        }
                        return Err(e.into());
                    }
                };
                let obj = RenderObject::new(mesh, mat);
                let candidate = match AssetCandidate::new(&scene.meshes, &scene.materials, &[obj]) {
                    Ok(candidate) => candidate,
                    Err(e) => {
                        scene.materials.remove(mat)?;
                        if self.groups[i].owns_mesh {
                            scene.meshes.remove(mesh)?;
                        }
                        return Err(e);
                    }
                };
                for (index, owner) in self.groups[i].textures.iter().zip(&d.uploaded) {
                    if let (Some(index), Some(owner)) = (index, owner) {
                        self.textures[*index].owner = Arc::downgrade(owner);
                    }
                }
                Ok(Some(Change::Register(
                    i,
                    Resident {
                        pair: (mesh, mat),
                        _images: d.uploaded,
                        buffers: d.buffers,
                    },
                    Box::new(candidate),
                )))
            }
            Change::Register(i, r, mut c) => {
                let pair = r.pair;
                if !self.groups[i].wanted {
                    #[cfg(feature = "metrics")]
                    {
                        self.metrics.cancellations += 1;
                    }
                    renderer.cancel_candidate(
                        &mut c,
                        &mut scene.meshes,
                        &mut scene.materials,
                        RetireAssets {
                            pairs: &[pair],
                            meshes: if self.groups[i].owns_mesh {
                                std::slice::from_ref(&pair.0)
                            } else {
                                &[]
                            },
                            materials: &[pair.1],
                        },
                    )?;
                    renderer.collect_assets(&mut scene.meshes, &mut scene.materials)?;
                    return Ok(None);
                }
                let result = renderer.poll_candidate(
                    &mut c,
                    &mut scene.meshes,
                    &mut scene.materials,
                    UploadBudget {
                        bytes: self.transfer,
                        submissions: 1,
                    },
                );
                match result {
                    Ok(Poll::Pending) => return Ok(Some(Change::Register(i, r, c))),
                    Err(e) if retryable(&e) => return Ok(Some(Change::Register(i, r, c))),
                    Err(e) => return Err(e.into()),
                    Ok(Poll::Ready(())) => {}
                }
                renderer.commit_candidate(
                    &mut c,
                    &mut scene.meshes,
                    &mut scene.materials,
                    RetireAssets::default(),
                )?;
                let group = &mut self.groups[i];
                for member in group.members.iter().filter(|m| m.near) {
                    scene.objects[member.object].mesh = pair.0;
                    scene.objects[member.object].material = pair.1;
                }
                group.live = Some(r);
                #[cfg(feature = "metrics")]
                {
                    self.metrics.promotions += 1;
                }
                Ok(None)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_reservation_counts_overlapping_native_objects() {
        assert!(covers(&[3, 7, 3], &[3, 3, 7]));
        assert!(!covers(&[3, 7, 3], &[7, 7]));
        let mut plan = crate::scene::StreamReservation {
            buffers: vec![64],
            images: vec![crate::scene::StreamImage {
                width: 8,
                height: 2,
                levels: 4,
                srgb: true,
            }],
        };
        assert!(plan.fits(156, 1));
        assert!(!plan.fits(155, 1));
        plan.images[0].width = u32::MAX;
        plan.images[0].height = u32::MAX;
        assert!(!plan.fits(u64::MAX, 1));
    }

    #[test]
    fn transfer_budget_selects_complete_rectangular_mip_suffix() {
        let image = ImageBlob {
            width: 8,
            height: 2,
            mip_levels: 4,
            srgb: true,
            pixels: (0..92).collect(),
        };
        let tail = fitting_mips(&image, 28).unwrap();
        assert_eq!((tail.width, tail.height, tail.mip_levels), (4, 1, 3));
        assert_eq!(tail.pixels, image.pixels[64..]);
        assert!(tail.srgb);
        assert_eq!(fitting_mips(&image, 92).unwrap(), image);
        assert!(fitting_mips(&image, 3).is_none());
    }
}

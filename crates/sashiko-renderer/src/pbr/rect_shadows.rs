//! Five 90-degree projections cover a rectangle's emitting hemisphere.
use super::history::{MapFrame, MapHistory, MapUpdates, ShadowKey};
use super::{Bounds, Frustum, LocalLight, RectLight, ShadowHistoryError};
use sashiko_renderer_core::graph::{Graph, GraphError, ImageDesc, ImageHandle, RasterRect};
use sashiko_renderer_core::runtime::{FrameSubmission, Runtime};

pub(crate) const FACE_SIZE: u32 = 512;
pub(crate) const FACES: usize = 5;

pub(crate) fn atlas_desc(lights: usize) -> Result<ImageDesc, GraphError> {
    let faces = u32::try_from(lights)
        .ok()
        .and_then(|n| n.checked_mul(FACES as u32))
        .filter(|&n| n > 0)
        .ok_or_else(|| GraphError::invalid_input("invalid rectangle shadow capacity"))?;
    let cols = (faces - 1).isqrt() + 1;
    Ok(ImageDesc::depth32(
        cols * FACE_SIZE,
        faces.div_ceil(cols) * FACE_SIZE,
    ))
}

pub(crate) fn tile(desc: ImageDesc, index: usize) -> RasterRect {
    let cols = desc.width / FACE_SIZE;
    RasterRect::new(
        index as u32 % cols * FACE_SIZE,
        index as u32 / cols * FACE_SIZE,
        FACE_SIZE,
        FACE_SIZE,
    )
}

pub(crate) fn affects(light: RectLight, bounds: Bounds) -> bool {
    let center = light.center();
    (0..3).all(|i| {
        center[i] + light.bound_radius >= bounds.min()[i]
            && center[i] - light.bound_radius <= bounds.max()[i]
    })
}

pub(crate) type Matrix = [[f32; 4]; 4];

pub(crate) fn matrix(light: RectLight, face: usize) -> Matrix {
    fn unit(a: [f32; 3]) -> [f32; 3] {
        let len = a[0].hypot(a[1]).hypot(a[2]);
        a.map(|x| x / len)
    }
    let neg = |a: [f32; 3]| a.map(|x| -x);
    let [u, v] = light.half_edges().map(unit);
    let n = light.normal();
    let (x, y, z) = match face {
        0 => (u, v, n),
        1 => (neg(n), v, u),
        2 => (n, v, neg(u)),
        3 => (u, neg(n), v),
        4 => (u, n, neg(v)),
        _ => unreachable!(),
    };
    let far = light.bound_radius;
    let near = (far * 0.0001).min(0.01);
    let scale = far / (far - near);
    let c = light.center();
    let offset = |a: [f32; 3]| -(a[0] * c[0] + a[1] * c[1] + a[2] * c[2]);
    let w = offset(z);
    [
        [x[0], y[0], z[0] * scale, z[0]],
        [x[1], y[1], z[1] * scale, z[1]],
        [x[2], y[2], z[2] * scale, z[2]],
        [offset(x), offset(y), (w - near) * scale, w],
    ]
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Projection {
    center: [u32; 3],
    edges: [[u32; 3]; 2],
    range: u32,
}

impl From<RectLight> for Projection {
    fn from(light: RectLight) -> Self {
        let bits = |v: f32| if v == 0. { 0 } else { v.to_bits() };
        Self {
            center: light.center().map(bits),
            edges: light.half_edges().map(|edge| edge.map(bits)),
            range: bits(light.range()),
        }
    }
}

/// One projection of a rectangle. A missing bound must conservatively be treated as overlapping.
#[derive(Clone, Copy)]
pub struct RectShadowFace {
    light: RectLight,
    matrix: Matrix,
}
impl RectShadowFace {
    /// Whether a caster's world bounds may affect this projection.
    pub fn overlaps(self, bounds: Bounds) -> bool {
        affects(self.light, bounds) && Frustum::new(self.matrix, bounds).overlaps(bounds)
    }
}

#[derive(Clone, Copy, Default)]
struct Slot {
    projection: Option<Projection>,
    revisions: [u64; FACES],
}

#[derive(Clone, Copy)]
struct Assignment {
    slot: usize,
    dirty: u8,
    projection: Projection,
}

/// Per-view atlas and its last submitted light placement and caster revisions.
pub struct RectShadowHistory {
    maps: MapHistory<1>,
    accepted: Vec<Slot>,
    pending: Vec<Slot>,
    lights: Vec<Assignment>,
    epoch: u64,
    available: std::collections::HashMap<Projection, usize>,
    next: Vec<Option<usize>>,
}

#[derive(Clone, Copy)]
pub(crate) struct Targets<'a> {
    pub image: ImageHandle,
    lights: &'a [Assignment],
    input: &'a [LocalLight],
    pub initialize: bool,
    faces: usize,
}
impl Targets<'_> {
    pub fn reused(self) -> bool {
        self.faces == 0
    }
    pub fn slot(self, light: usize) -> usize {
        self.lights[light].slot
    }
    pub fn dirty(self, light: usize, face: usize) -> bool {
        self.lights[light].dirty & (1 << face) != 0
    }
    pub fn check(self, lights: &[LocalLight]) -> Result<(), GraphError> {
        if std::ptr::eq(self.input, lights) {
            return Ok(());
        }
        if self.lights.iter().map(|a| a.projection).ne(lights
            .iter()
            .filter_map(LocalLight::shadow_emitter)
            .map(Projection::from))
        {
            return Err(GraphError::invalid_input(
                "rectangle history and frame lights differ",
            ));
        }
        Ok(())
    }
}

/// Graph-local selection. Attach it to the draw and export it before submission.
pub struct RectShadowFrame<'a> {
    map: MapFrame<1>,
    pub(crate) targets: Targets<'a>,
}
impl RectShadowFrame<'_> {
    pub fn faces_to_render(&self) -> usize {
        self.targets.faces
    }
    pub fn export(self, graph: &mut Graph) -> Result<RectShadowUpdates, GraphError> {
        Ok(RectShadowUpdates {
            map: self.map.export(graph)?,
            faces: self.targets.faces,
        })
    }
}
/// Exported tile changes to publish after a successful submission.
#[must_use = "accept the updates after submission"]
pub struct RectShadowUpdates {
    map: MapUpdates<1>,
    faces: usize,
}
impl RectShadowUpdates {
    pub fn rendered_faces(&self) -> usize {
        self.faces
    }
}

impl RectShadowHistory {
    pub(crate) fn require_idle(&self) -> Result<(), GraphError> {
        self.maps.require_idle()
    }
    /// Reserve CPU slots for this many simultaneously shadowed rectangles.
    pub fn new(capacity: usize) -> Self {
        Self {
            maps: MapHistory::default(),
            accepted: vec![Slot::default(); capacity],
            pending: vec![Slot::default(); capacity],
            lights: Vec::with_capacity(capacity),
            epoch: 0,
            available: std::collections::HashMap::with_capacity(capacity),
            next: vec![None; capacity],
        }
    }
    /// Return a caster content revision for each face. The previous revision is available
    /// when the projection still matches. Light order and radiance do not affect reuse.
    /// Returns None if there are no shadowed rectangles, preserving the last submitted atlas.
    pub fn begin<'a>(
        &'a mut self,
        graph: &mut Graph,
        lights: &'a [LocalLight],
        mut revision: impl FnMut(RectShadowFace, Option<u64>) -> u64,
    ) -> Result<Option<RectShadowFrame<'a>>, GraphError> {
        self.maps.require_idle()?;
        let emitters = || lights.iter().filter_map(LocalLight::shadow_emitter);
        let count = emitters().count();
        if count == 0 {
            return Ok(None);
        }
        if count > self.accepted.len() {
            return Err(GraphError::capacity_error(
                super::SceneLimits::SHADOWED_RECT_LIGHTS,
                self.accepted.len(),
                Some(count),
            ));
        }
        self.lights.clear();
        self.pending.fill(Slot::default());
        // Claim all surviving projections before recycling removed slots, including duplicates.
        self.available.clear();
        for (i, slot) in self.accepted.iter().enumerate().rev() {
            if let Some(key) = slot.projection {
                self.next[i] = self.available.insert(key, i);
            }
        }
        for light in emitters() {
            let projection = Projection::from(light);
            let slot = self.available.remove(&projection);
            if let Some(i) = slot {
                if let Some(next) = self.next[i] {
                    self.available.insert(projection, next);
                }
                self.pending[i].projection = Some(projection);
            }
            self.lights.push(Assignment {
                slot: slot.unwrap_or(usize::MAX),
                dirty: 0,
                projection,
            });
        }
        let mut faces = 0;
        let mut vacant = 0;
        for (assignment, light) in self.lights.iter_mut().zip(emitters()) {
            let matched = assignment.slot != usize::MAX;
            if !matched {
                while self.pending[vacant].projection.is_some() {
                    vacant += 1;
                }
                assignment.slot = vacant;
                self.pending[vacant].projection = Some(light.into());
            }
            for face in 0..FACES {
                let old = matched.then(|| self.accepted[assignment.slot].revisions[face]);
                let new = revision(
                    RectShadowFace {
                        light,
                        matrix: matrix(light, face),
                    },
                    old,
                );
                self.pending[assignment.slot].revisions[face] = new;
                if old != Some(new) {
                    assignment.dirty |= 1 << face;
                    faces += 1;
                }
            }
        }
        let initialize = self.maps.is_empty();
        let epoch = if faces > 0 {
            self.epoch.checked_add(1).expect("shadow epoch exhausted")
        } else {
            self.epoch
        };
        let map = self.maps.begin_maps(
            graph,
            atlas_desc(self.accepted.len())?,
            ["rectangle shadows"],
            [ShadowKey::new([[0.; 4]; 4], epoch)],
        )?;
        Ok(Some(RectShadowFrame {
            targets: Targets {
                image: map.target(0),
                lights: &self.lights,
                input: lights,
                initialize,
                faces,
            },
            map,
        }))
    }
    pub(crate) fn check_updates(
        &self,
        graph_id: u64,
        updates: &RectShadowUpdates,
    ) -> Result<(), ShadowHistoryError> {
        self.maps.check_updates(graph_id, &updates.map)
    }
    pub fn accept(
        &mut self,
        receipt: &FrameSubmission,
        updates: &mut RectShadowUpdates,
    ) -> Result<(), ShadowHistoryError> {
        self.maps.accept(receipt, &mut updates.map)?;
        if updates.faces != 0 {
            self.epoch += 1;
        }
        std::mem::swap(&mut self.accepted, &mut self.pending);
        Ok(())
    }
    pub fn clear(&mut self, runtime: &mut Runtime) -> Result<(), ShadowHistoryError> {
        self.maps.clear(runtime)?;
        self.accepted.fill(Slot::default());
        Ok(())
    }
}

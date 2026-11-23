use crate::{BakeError, BakeScene, Triangle, Vec3, cross, dot, sub};

/// Preorder BVH. A missed box skips directly to its escape index, on CPU or GPU.
pub(crate) struct Node {
    pub lo: Vec3,
    pub hi: Vec3,
    pub start: u32,
    pub count: u32,
    pub escape: u32,
}
pub(crate) struct Bvh(pub Vec<Node>);
impl Bvh {
    pub fn new(tris: &mut [Triangle]) -> Result<Self, BakeError> {
        if tris.len() > u32::MAX as usize / 2 {
            return Err(BakeError("bake BVH exceeds 32-bit addressing"));
        }
        let mut tree = Self(Vec::new());
        if !tris.is_empty() {
            tree.build(tris, 0);
        }
        Ok(tree)
    }
    fn build(&mut self, tris: &mut [Triangle], start: u32) {
        let mut lo = [f32::INFINITY; 3];
        let mut hi = [f32::NEG_INFINITY; 3];
        for p in tris.iter().flat_map(|t| t.positions.iter()) {
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        let id = self.0.len();
        self.0.push(Node {
            lo,
            hi,
            start,
            count: tris.len() as u32,
            escape: 0,
        });
        if tris.len() > 8 {
            let extent = sub(hi, lo);
            let axis = (0..3)
                .max_by(|&a, &b| extent[a].total_cmp(&extent[b]))
                .unwrap();
            let mid = tris.len() / 2;
            let center = |t: &Triangle| t.positions.iter().map(|p| f64::from(p[axis])).sum::<f64>();
            tris.select_nth_unstable_by(mid, |a, b| center(a).total_cmp(&center(b)));
            let (left, right) = tris.split_at_mut(mid);
            self.0[id].count = 0;
            self.build(left, start);
            self.build(right, start + mid as u32);
        }
        self.0[id].escape = self.0.len() as u32;
    }
}
impl Node {
    fn intersects(&self, o: Vec3, d: Vec3, mut far: f32) -> bool {
        let mut near = 0f32;
        for i in 0..3 {
            if d[i] == 0. {
                if o[i] < self.lo[i] || o[i] > self.hi[i] {
                    return false;
                }
            } else {
                let a = (self.lo[i] - o[i]) / d[i];
                let b = (self.hi[i] - o[i]) / d[i];
                near = near.max(a.min(b));
                far = far.min(a.max(b));
                if near > far {
                    return false;
                }
            }
        }
        true
    }
}
pub(crate) struct Hit {
    pub distance: f32,
    pub backface: bool,
    pub normal: Vec3,
    pub diffuse: Vec3,
    pub emission: Vec3,
}
impl BakeScene {
    pub(crate) fn trace(&self, o: Vec3, d: Vec3, mut limit: f32) -> Option<Hit> {
        let mut hit = None;
        let mut index = 0;
        while let Some(node) = self.bvh.0.get(index) {
            if !node.intersects(o, d, limit) {
                index = node.escape as usize;
                continue;
            }
            index += 1;
            for t in &self.triangles[node.start as usize..(node.start + node.count) as usize] {
                let [a, b, c] = t.positions;
                let e = sub(b, a);
                let f = sub(c, a);
                let h = cross(d, f);
                let det = dot(e, h);
                if det == 0. {
                    continue;
                }
                let delta = sub(o, a);
                let u = dot(delta, h) / det;
                let q = cross(delta, e);
                let v = dot(d, q) / det;
                let dist = dot(f, q) / det;
                if !(u >= 0. && v >= 0. && u + v <= 1. && dist > 0. && dist < limit) {
                    continue;
                }
                let m = self.materials[t.material];
                let uv = std::array::from_fn(|i| {
                    t.uv[0][i] * (1. - u - v) + t.uv[1][i] * u + t.uv[2][i] * v
                });
                let sample =
                    |id: Option<usize>| id.map_or([1.; 4], |i| self.textures[i].sample(uv));
                let tex = sample(m.base_texture);
                if m.alpha_cutoff
                    .is_some_and(|cutoff| m.opacity * tex[3] < cutoff)
                {
                    continue;
                }
                let dielectric = 1. - m.metallic * sample(m.orm_texture)[2].min(1.);
                let emitted = sample(m.emission_texture);
                let n = t.normal();
                let scale = det.signum() / dot(n, n).sqrt();
                hit = Some(Hit {
                    distance: dist,
                    backface: det < 0.,
                    normal: n.map(|x| x * scale),
                    diffuse: std::array::from_fn(|i| m.diffuse[i] * tex[i].min(1.) * dielectric),
                    emission: std::array::from_fn(|i| m.emissive_nits[i] * emitted[i]),
                });
                limit = dist;
            }
        }
        hit
    }
}

use renderer::capacity::{GraphRequirements, Preparation};
use renderer::graph::{BufferDesc, BufferHandle, Graph, GraphCapacityKind, GraphError};

/// Maximum model transforms in the fixed table used by one PBR view.
pub const MAX_TRANSFORMS: usize = 1_024;
const TRANSFORM_BYTES: usize = 96;
pub(crate) const TRANSFORM_BINDING: u32 = 4;
const SMALL_TABLE: usize = 64;

#[derive(Clone, Copy)]
pub(crate) enum InstanceTier {
    Small,
    Full,
}

impl InstanceTier {
    pub(crate) fn for_limit(limit: usize) -> Result<Option<Self>, GraphError> {
        match limit {
            0 => Ok(None),
            1..=SMALL_TABLE => Ok(Some(Self::Small)),
            n if n <= MAX_TRANSFORMS => Ok(Some(Self::Full)),
            _ => Err(GraphError::invalid_input(
                "PBR transform limit exceeds 1,024",
            )),
        }
    }

    pub(crate) const fn bytes(self) -> usize {
        let records = match self {
            Self::Small => SMALL_TABLE,
            Self::Full => MAX_TRANSFORMS,
        };
        records * TRANSFORM_BYTES
    }
}

/// A model-to-world affine transform and its inverse-transpose normal matrix.
///
/// The three rows of the affine matrix store the linear coefficients in
/// columns 0 to 2 and the translation in column 3. The encoded record also
/// carries orientation parity for mirrored material and shadow draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform {
    model: [[f32; 4]; 3],
    normal: [[f32; 4]; 3],
}

impl Transform {
    pub const IDENTITY: Self = Self {
        model: IDENTITY_ROWS,
        normal: IDENTITY_NORMAL_ROWS,
    };

    /// Prepare a finite, invertible transform.
    /// Establish the normal transform when model data is created or updated.
    /// Reject determinants whose sign cannot be resolved in f64.
    pub fn new(rows: [[f32; 4]; 3]) -> Result<Self, GraphError> {
        if !rows.iter().flatten().all(|value| value.is_finite()) {
            return Err(GraphError::invalid_input(
                "PBR transform must have finite coefficients",
            ));
        }
        // Use wider intermediates so small or large finite f32 scales do not
        // overflow or underflow in the determinant calculation itself.
        let [a, b, c] = rows.map(|[x, y, z, _]| [x, y, z].map(f64::from));
        let cofactors = [cross(b, c), cross(c, a), cross(a, b)];
        let terms: [f64; 3] = std::array::from_fn(|axis| a[axis] * cofactors[0][axis]);
        let determinant = terms.iter().sum::<f64>();
        // The initial f32 products are exact in f64. Bound rounding in
        // the differences, final products and sums before trusting a sign.
        let error = terms.into_iter().map(f64::abs).sum::<f64>() * (8.0 * f64::EPSILON);
        if determinant.abs() <= error {
            return Err(GraphError::invalid_input(
                "PBR transform must have a reliably nonzero determinant",
            ));
        }
        let mut normal = cofactors.map(|row| {
            let [x, y, z] = row.map(|value| (value / determinant) as f32);
            [x, y, z, 0.0]
        });
        normal[0][3] = determinant.signum() as f32;
        if !normal.iter().flatten().all(|value| value.is_finite()) {
            return Err(GraphError::invalid_input(
                "PBR inverse-transpose coefficients must fit f32",
            ));
        }
        Ok(Self {
            model: rows,
            normal,
        })
    }

    pub fn columns(self) -> [[f32; 4]; 4] {
        std::array::from_fn(|col| {
            [
                self.model[0][col],
                self.model[1][col],
                self.model[2][col],
                if col == 3 { 1.0 } else { 0.0 },
            ]
        })
    }

    /// Encode the canonical 96-byte transform record.
    pub fn encode(self) -> [u8; TRANSFORM_BYTES] {
        let mut bytes = [0; TRANSFORM_BYTES];
        for (destination, word) in bytes
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(self.model.into_iter().chain(self.normal).flatten())
        {
            *destination = word.to_le_bytes();
        }
        bytes
    }
}

const IDENTITY_ROWS: [[f32; 4]; 3] = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
];

const IDENTITY_NORMAL_ROWS: [[f32; 4]; 3] =
    [[1.0, 0.0, 0.0, 1.0], IDENTITY_ROWS[1], IDENTITY_ROWS[2]];

fn cross([ax, ay, az]: [f64; 3], [bx, by, bz]: [f64; 3]) -> [f64; 3] {
    [ay * bz - az * by, az * bx - ax * bz, ax * by - ay * bx]
}

#[derive(Clone, Copy)]
pub(super) struct InstanceBuffer {
    pub(super) handle: BufferHandle,
    pub(super) size: u64,
}

pub(crate) struct PreparedInstances {
    bytes: Vec<u8>,
    previous_len: usize,
}

impl PreparedInstances {
    pub(crate) fn reserve(
        preparation: &mut Preparation,
        tier: InstanceTier,
    ) -> Result<Self, GraphError> {
        let byte_count = tier.bytes();
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(byte_count).map_err(|_| {
            GraphError::capacity_error(GraphCapacityKind::UploadBytes, byte_count, None)
        })?;
        for _ in 0..byte_count / TRANSFORM_BYTES {
            bytes.extend_from_slice(&Transform::IDENTITY.encode());
        }
        preparation.add_requirements(
            GraphRequirements {
                resources: 1,
                passes: 1,
                accesses: 1,
                upload_bytes: byte_count,
                name_bytes: "pbr transforms".len() + "pbr transforms upload".len(),
                ..Default::default()
            },
            renderer::capacity::WorkingSet {
                resources: &[renderer::capacity::ResourceClass::Buffer {
                    desc: BufferDesc {
                        size: byte_count as u64,
                    },
                    count: preparation.frames_in_flight().get(),
                }],
                ..Default::default()
            },
        )?;
        Ok(Self {
            bytes,
            previous_len: 0,
        })
    }

    /// Keep the complete shader-visible prefix initialized. Reset entries
    /// removed since the preceding frame to identity in the reusable scratch.
    pub(super) fn append(
        &mut self,
        graph: &mut Graph,
        transforms: &[Transform],
    ) -> Result<InstanceBuffer, GraphError> {
        if transforms.len() > self.bytes.len() / TRANSFORM_BYTES {
            return Err(GraphError::capacity_error(
                GraphCapacityKind::UploadBytes,
                self.bytes.len(),
                transforms.len().checked_mul(TRANSFORM_BYTES),
            ));
        }
        let rows = self.bytes.as_chunks_mut::<TRANSFORM_BYTES>().0;
        for (destination, transform) in rows.iter_mut().zip(transforms) {
            *destination = transform.encode();
        }
        if transforms.len() < self.previous_len {
            rows[transforms.len()..self.previous_len].fill(Transform::IDENTITY.encode());
        }
        self.previous_len = transforms.len();
        let size = self.bytes.len() as u64;
        let handle = graph.buffer("pbr transforms", BufferDesc { size })?;
        graph.upload_buffer("pbr transforms upload", handle, 0, &self.bytes)?;
        Ok(InstanceBuffer { handle, size })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;

    use renderer::graph::{GraphCapacity, GraphPool};

    use super::*;

    #[test]
    fn encode_uses_row_major_model_and_inverse_transpose() {
        let transform = Transform::new([
            [0.0, -3.0, 0.0, 4.0],
            [2.0, 0.0, 0.0, 5.0],
            [0.0, 0.0, 4.0, 6.0],
        ])
        .unwrap();
        let words: Vec<_> = transform
            .encode()
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .map(f32::from_le_bytes)
            .collect();
        assert_eq!(
            words,
            [
                0.0,
                -3.0,
                0.0,
                4.0,
                2.0,
                0.0,
                0.0,
                5.0,
                0.0,
                0.0,
                4.0,
                6.0,
                0.0,
                -1.0 / 3.0,
                0.0,
                1.0,
                0.5,
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                0.25,
                0.0,
            ]
        );
    }

    #[test]
    fn retains_mirror_parity_and_the_inverse_transpose() {
        let transform =
            Transform::new([[-2.0, 0.0, 0.0, 0.0], IDENTITY_ROWS[1], IDENTITY_ROWS[2]]).unwrap();
        let encoded = transform.encode();
        let words = encoded.as_chunks::<4>().0;
        assert_eq!(f32::from_le_bytes(words[12]), -0.5);
        assert_eq!(f32::from_le_bytes(words[15]), -1.0);
    }

    #[test]
    fn rejects_singular_nonfinite_and_unrepresentable_transforms() {
        for x in [0.0, f32::INFINITY, f32::NAN, f32::from_bits(1)] {
            let mut rows = IDENTITY_ROWS;
            rows[0][0] = x;
            assert!(Transform::new(rows).is_err(), "{x}");
        }
        // The third row is the sum of the first two. Rounded products
        // nevertheless give a small positive determinant without a bound.
        assert!(
            Transform::new([
                [-6_319_415.0, -4_662_252.0, 1_552_586.0, 0.0],
                [1_388_121.0, 3_836_060.0, 5_607_740.0, 0.0],
                [-4_931_294.0, -826_192.0, 7_160_326.0, 0.0],
            ])
            .is_err()
        );
    }

    #[test]
    fn uploads_full_prepared_prefix_and_clears_reused_padding() {
        let mut preparation = Preparation::new(NonZeroUsize::MIN);
        let mut instances =
            PreparedInstances::reserve(&mut preparation, InstanceTier::Small).unwrap();
        let cap = preparation
            .graph_capacity(GraphCapacity {
                upload_bytes: 0,
                ..GraphCapacity::default()
            })
            .unwrap();
        assert_eq!(cap.upload_bytes, InstanceTier::Small.bytes());
        let mut pool = GraphPool::new(cap, 1).unwrap();
        let shift =
            Transform::new([[1.0, 0.0, 0.0, 2.0], IDENTITY_ROWS[1], IDENTITY_ROWS[2]]).unwrap();
        for (transforms, expected) in [
            (&[Transform::IDENTITY, shift][..], shift),
            (&[Transform::IDENTITY][..], Transform::IDENTITY),
        ] {
            let mut graph = pool.begin().unwrap();
            let table = instances.append(&mut graph, transforms).unwrap();
            graph.export_buffer(table.handle).unwrap();
            assert!(graph.compile().is_ok());
            assert_eq!(
                instances.bytes.as_chunks::<TRANSFORM_BYTES>().0[1],
                expected.encode(),
            );
        }
    }
}

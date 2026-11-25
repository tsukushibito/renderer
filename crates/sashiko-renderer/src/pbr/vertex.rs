use sashiko_renderer_core::graph::GraphError;

use crate::passes::pbr::VERTEX_BYTES;

/// A finite vertex in the PBR shader's 48-byte position, normal, tangent and UV
/// layout. Supply world-space attributes for ordinary draws, or model-space
/// attributes for draws with a model transform.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PbrVertex {
    position: [f32; 3],
    normal: [f32; 3],
    tangent: [f32; 4],
    texcoord: [f32; 2],
}

impl PbrVertex {
    /// Validate attributes while preparing geometry. The tangent's fourth
    /// component is its handedness.
    pub fn new(
        position: [f32; 3],
        normal: [f32; 3],
        tangent: [f32; 4],
        texcoord: [f32; 2],
    ) -> Result<Self, GraphError> {
        let vertex = Self {
            position,
            normal,
            tangent,
            texcoord,
        };
        if vertex.attributes().all(f32::is_finite) {
            Ok(vertex)
        } else {
            Err(GraphError::invalid_input(
                "PBR vertex attributes must be finite",
            ))
        }
    }

    pub const fn position(self) -> [f32; 3] {
        self.position
    }

    pub(crate) fn skinned(
        self,
        joints: impl Iterator<Item = (super::Transform, f32)>,
    ) -> Result<Self, GraphError> {
        let mut position = [0.; 3];
        let mut normal = [0.; 3];
        let mut tangent = [0.; 3];
        let mut bitangent = [0.; 3];
        let cross = |a: [f32; 3], b: [f32; 3]| {
            [
                a[1] * b[2] - a[2] * b[1],
                a[2] * b[0] - a[0] * b[2],
                a[0] * b[1] - a[1] * b[0],
            ]
        };
        let [x, y, z, sign] = self.tangent;
        let rest_tangent = [x, y, z];
        let rest_bitangent = cross(self.normal, rest_tangent).map(|v| v * sign);
        let dot = |row: [f32; 4], v: [f32; 3]| {
            (0..3)
                .map(|i| f64::from(row[i]) * f64::from(v[i]))
                .sum::<f64>()
        };
        for (joint, weight) in joints {
            let w = f64::from(weight);
            for i in 0..3 {
                let row = joint.model[i];
                position[i] += w * (dot(row, self.position) + f64::from(row[3]));
                normal[i] += w * dot(joint.normal[i], self.normal);
                tangent[i] += w * dot(row, rest_tangent);
                bitangent[i] += w * dot(row, rest_bitangent);
            }
        }
        let n = normal.map(|x| x as f32);
        let [tx, ty, tz] = tangent.map(|x| x as f32);
        let handedness = cross(n, [tx, ty, tz])
            .into_iter()
            .zip(bitangent)
            .map(|(a, b)| f64::from(a) * b)
            .sum::<f64>();
        let sign = if handedness == 0. {
            sign
        } else {
            handedness.signum() as f32
        };
        Self::new(
            position.map(|x| x as f32),
            n,
            [tx, ty, tz, sign],
            self.texcoord,
        )
    }

    /// Encode one little-endian record for [`sashiko_renderer_core::geometry::GeometryData`].
    pub fn encode(self) -> [u8; VERTEX_BYTES] {
        let mut bytes = [0; VERTEX_BYTES];
        for (word, attribute) in bytes
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(self.attributes())
        {
            *word = attribute.to_le_bytes();
        }
        bytes
    }

    fn attributes(self) -> impl Iterator<Item = f32> {
        self.position
            .into_iter()
            .chain(self.normal)
            .chain(self.tangent)
            .chain(self.texcoord)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reflected_joint_preserves_the_tangent_basis_and_texture_coordinates() {
        let vertex =
            PbrVertex::new([2., 3., 4.], [1., 0., 0.], [0., 1., 0., 1.], [0.3, 0.7]).unwrap();
        let joint =
            crate::pbr::Transform::new([[-2., 0., 0., 1.], [0., 4., 0., 0.], [0., 0., 1., 0.]])
                .unwrap();
        let posed = vertex.skinned(std::iter::once((joint, 1.))).unwrap();
        assert_eq!(posed.position, [-3., 12., 4.]);
        assert_eq!(posed.normal, [-0.5, 0., 0.]);
        assert_eq!(posed.tangent, [0., 4., 0., -1.]);
        assert_eq!(posed.texcoord, vertex.texcoord);
    }

    #[test]
    fn encodes_the_vertex_abi_for_indexed_geometry() {
        let vertex = PbrVertex::new(
            [1.0, 2.0, 3.0],
            [4.0, 5.0, 6.0],
            [7.0, 8.0, 9.0, -1.0],
            [0.25, 0.5],
        )
        .unwrap();
        let bytes = vertex.encode();
        let [position, normal, tangent, texcoord] = [0, 12, 24, 40]
            .map(|offset| f32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap()));
        assert_eq!([position, normal, tangent, texcoord], [1.0, 4.0, 7.0, 0.25]);
        let mesh = sashiko_renderer_core::geometry::GeometryData::new(
            VERTEX_BYTES as u32,
            &bytes,
            &[0, 0, 0],
        )
        .unwrap();
        assert_eq!(mesh.vertex_count(), 1);
        assert!(PbrVertex::new([0.0; 3], [0.0; 3], [0.0; 4], [f32::NAN, 0.0]).is_err());
    }
}

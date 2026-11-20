use renderer::graph::GraphError;

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

    /// Encode one little-endian record for [`renderer::geometry::GeometryData`].
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
        let mesh =
            renderer::geometry::GeometryData::new(VERTEX_BYTES as u32, &bytes, &[0, 0, 0]).unwrap();
        assert_eq!(mesh.vertex_count(), 1);
        assert!(PbrVertex::new([0.0; 3], [0.0; 3], [0.0; 4], [f32::NAN, 0.0]).is_err());
    }
}

//! Static glTF 2 equipment importer. The selected scene defines model-local space.
use glam::Mat4;
use gltf::{
    Document,
    mesh::{Mode, Semantic},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    error::Error,
    fs,
    path::{Component, Path},
};

mod geometry;
mod image;
mod material;

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum ImageRole {
    Color,
    Normal,
    Orm,
}

impl ImageRole {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Color => "color",
            Self::Normal => "normal",
            Self::Orm => "orm",
        }
    }
}

struct Part {
    mesh: usize,
    primitive: usize,
    transform: Mat4,
    material: material::SourceMaterial,
}

pub(crate) struct Source {
    document: Document,
    buffers: Vec<Vec<u8>>,
    images: Vec<Option<Vec<u8>>>,
    parts: Vec<Part>,
}

impl Source {
    pub(crate) fn open(path: &Path, source_root: &Path) -> Result<Self> {
        let bytes = fs::read(path)?;
        let gltf = gltf::Gltf::from_slice(&bytes)?;
        let document = gltf.document;
        let asset = &document.as_json().asset;
        if asset.version != "2.0" || asset.min_version.as_deref().is_some_and(|v| v != "2.0") {
            return Err(format!(
                "unsupported glTF version {} (minimum {:?})",
                asset.version, asset.min_version
            )
            .into());
        }
        if let Some(extension) = document.extensions_required().next() {
            return Err(format!("unsupported required extension {extension}").into());
        }
        if document.animations().next().is_some() || document.skins().next().is_some() {
            return Err("equipment requires a static, unskinned scene".into());
        }
        let root = path
            .canonicalize()?
            .parent()
            .ok_or("missing source directory")?
            .to_owned();
        let mut buffers = Vec::new();
        for buffer in document.buffers() {
            let data = match buffer.source() {
                gltf::buffer::Source::Uri(uri) => read_uri(source_root, &root, uri)?,
                gltf::buffer::Source::Bin => gltf.blob.clone().ok_or("missing BIN chunk")?,
            };
            if data.len() < buffer.length() {
                return Err(
                    format!("buffer {} shorter than declared byteLength", buffer.index()).into(),
                );
            }
            buffers.push(data);
        }
        for view in document.views() {
            let end = view
                .offset()
                .checked_add(view.length())
                .ok_or("buffer view size overflow")?;
            if end > view.buffer().length() {
                return Err(format!("buffer view {} exceeds declared buffer", view.index()).into());
            }
        }

        let scene = document
            .default_scene()
            .or_else(|| document.scenes().next())
            .ok_or("missing scene")?;
        let mut nodes: Vec<_> = scene.nodes().map(|node| (node, Mat4::IDENTITY)).collect();
        nodes.reverse();
        let mut visited = HashSet::new();
        let mut parts = Vec::new();
        while let Some((node, parent)) = nodes.pop() {
            if !visited.insert(node.index()) {
                return Err(format!("repeated or cyclic node {}", node.index()).into());
            }
            let transform = parent * Mat4::from_cols_array_2d(&node.transform().matrix());
            if !transform.is_finite() || transform.row(3) != glam::Vec4::W {
                return Err(format!("node {} has invalid affine transform", node.index()).into());
            }
            if let Some(mesh) = node.mesh() {
                for primitive in mesh.primitives() {
                    if primitive.mode() != Mode::Triangles || primitive.morph_targets().len() != 0 {
                        return Err(format!(
                            "mesh {} primitive {} needs static triangles without morph targets",
                            mesh.index(),
                            primitive.index()
                        )
                        .into());
                    }
                    for (semantic, _) in primitive.attributes() {
                        if !matches!(
                            semantic,
                            Semantic::Positions
                                | Semantic::Normals
                                | Semantic::Tangents
                                | Semantic::TexCoords(0)
                        ) {
                            return Err(format!(
                                "mesh {} unsupported attribute {semantic:?}",
                                mesh.index()
                            )
                            .into());
                        }
                    }
                    let material = material::SourceMaterial::read(primitive.material())?;
                    parts.push(Part {
                        mesh: mesh.index(),
                        primitive: primitive.index(),
                        transform,
                        material,
                    });
                }
            }
            let children: Vec<_> = node.children().collect();
            nodes.extend(children.into_iter().rev().map(|child| (child, transform)));
        }
        if parts.is_empty() {
            return Err("scene contains no static mesh primitives".into());
        }
        let selected: HashSet<_> = parts
            .iter()
            .flat_map(|p| p.material.images.iter().flatten().map(|(i, _)| *i))
            .collect();
        let mut images = Vec::new();
        for image in document.images() {
            let data = if selected.contains(&image.index()) {
                Some(match image.source() {
                    gltf::image::Source::Uri { uri, .. } => read_uri(source_root, &root, uri)?,
                    gltf::image::Source::View { view, .. } => buffers[view.buffer().index()]
                        [view.offset()..view.offset() + view.length()]
                        .to_vec(),
                })
            } else {
                None
            };
            images.push(data);
        }
        Ok(Self {
            document,
            buffers,
            parts,
            images,
        })
    }

    pub(crate) fn len(&self) -> usize {
        self.parts.len()
    }

    fn primitive(&self, i: usize) -> gltf::Primitive<'_> {
        let part = &self.parts[i];
        self.document
            .meshes()
            .nth(part.mesh)
            .unwrap()
            .primitives()
            .nth(part.primitive)
            .unwrap()
    }

    pub(crate) fn mesh_inputs(&self, i: usize) -> Result<Vec<u8>> {
        let mut hash = Sha256::new();
        for v in self.parts[i].transform.to_cols_array() {
            hash.update(v.to_le_bytes());
        }
        geometry::hash_inputs(self.primitive(i), &self.buffers, &mut hash)?;
        Ok(hash.finalize().to_vec())
    }

    pub(crate) fn cook_mesh(&self, i: usize) -> Result<Vec<u8>> {
        geometry::cook(self.primitive(i), &self.buffers, self.parts[i].transform)
    }

    pub(crate) fn material_inputs(&self, i: usize) -> Result<Vec<u8>> {
        self.parts[i].material.inputs()
    }

    pub(crate) fn cook_material(&self, i: usize) -> Result<Vec<u8>> {
        self.parts[i].material.cook()
    }

    pub(crate) fn images(&self, i: usize) -> [Option<(usize, ImageRole)>; 4] {
        self.parts[i].material.images
    }

    pub(crate) fn image_inputs(&self, i: usize) -> Result<&[u8]> {
        self.images
            .get(i)
            .and_then(Option::as_deref)
            .ok_or_else(|| format!("missing image {i}").into())
    }

    pub(crate) fn cook_image(&self, i: usize, role: ImageRole) -> Result<Vec<u8>> {
        image::cook(self.image_inputs(i)?, role)
    }
}

fn read_uri(source_root: &Path, directory: &Path, uri: &str) -> Result<Vec<u8>> {
    // External relative files only. Resolve escaped UTF-8 before checking the path.
    let mut bytes = uri.bytes();
    let mut decoded = Vec::new();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let mut hex = || {
                bytes
                    .next()
                    .and_then(|b| char::from(b).to_digit(16))
                    .ok_or("invalid URI escape")
            };
            decoded.push((16 * hex()? + hex()?) as u8);
        } else {
            decoded.push(byte);
        }
    }
    let name = std::str::from_utf8(&decoded)?;
    if name.contains([':', '\\', '?', '#', '\0'])
        || name.is_empty()
        || Path::new(name).components().any(|c| {
            !matches!(
                c,
                Component::Normal(_) | Component::CurDir | Component::ParentDir
            )
        })
    {
        return Err(format!("unsupported asset URI {uri}").into());
    }
    let resolved = directory
        .join(name)
        .canonicalize()
        .map_err(|e| format!("reading {uri}: {e}"))?;
    if !resolved.starts_with(source_root) {
        return Err(format!("asset URI escapes source directory: {uri}").into());
    }
    Ok(fs::read(resolved)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use renderer_asset::{MaterialBlob, MeshBlob};

    #[test]
    fn comparison_equipment_preserves_mikk_uvs_and_mirrored_basis() -> Result<()> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../training-yard/assets/equipment/source/comparison.gltf");
        let mut source = Source::open(&path, &path.parent().unwrap().canonicalize()?)?;
        let mesh = MeshBlob::decode(&source.cook_mesh(0)?)?;
        // Indexed triangle corners from tools/bistro/cook.py cook_primitive with
        // the pinned MikkTSpace generator and an identity model transform. Compare
        // attributes in draw order so equivalent vertex table layouts are accepted.
        let expected = [
            [-0.35, 0., 0.9916721, -0.12878859, -0.25, 0.1],
            [0.35, 0., 0.99825555, 0.05904074, 0.75, 0.2],
            [-0.175, 0.75, 0.97282594, 0.23153758, -0.1, 0.95],
            [0.35, 0., 0.99825555, 0.05904074, 0.75, 0.2],
            [0.175, 0.75, 0.85285133, 0.5221538, 0.9, 0.8],
            [-0.175, 0.75, 0.97282594, 0.23153758, -0.1, 0.95],
        ];
        assert_eq!(mesh.indices.len(), expected.len());
        for (&index, [x, y, tx, ty, u, v]) in mesh.indices.iter().zip(expected) {
            let vertex = mesh.vertices[index as usize];
            assert_eq!(vertex[..6], [x, y, 0.5, 0., 0., -1.]);
            assert_eq!(vertex[9], 1., "tangent handedness");
            assert_eq!(vertex[10..], [u, v]);
            for (actual, reference) in vertex[6..9].iter().zip([tx, ty, 0.]) {
                assert!(
                    (actual - reference).abs() <= 2e-6,
                    "{actual} vs {reference}"
                );
            }
        }

        let material = MaterialBlob::decode(&source.cook_material(0)?)?;
        assert_eq!(source.images(0)[0], source.images(0)[3]);
        assert_ne!(material.samplers[0], material.samplers[3]);
        let initial_key = source.mesh_inputs(0)?;
        source.parts[0].transform = Mat4::from_scale(glam::vec3(-2., 3., 1.5));
        assert_ne!(source.mesh_inputs(0)?, initial_key);
        let mirrored = MeshBlob::decode(&source.cook_mesh(0)?)?;
        for (original, reflected) in mesh
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .zip(mirrored.indices.as_chunks::<3>().0)
        {
            for (&a, b) in original
                .iter()
                .zip([reflected[0], reflected[2], reflected[1]])
            {
                let a = mesh.vertices[a as usize];
                let b = mirrored.vertices[b as usize];
                assert_eq!(b[..3], [-2. * a[0], 3. * a[1], 1.5 * a[2]]);
                assert_eq!(b[9], -a[9]);
                assert_eq!(b[10..], a[10..]);
            }
        }
        Ok(())
    }
}

//! Reverse bindings for edits to existing PNG files in a clean catalog.
use crate::{
    Error,
    cache::Cache,
    gltf::{self, ExternalUse, ImageRole},
    inputs::Snapshot,
};
use renderer_asset::{AssetId, Catalog};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

#[derive(Debug, Default, Serialize, Deserialize)]
pub(super) struct Manifest {
    pub models: BTreeMap<PathBuf, BTreeSet<PathBuf>>,
    // A path opened as a buffer needs the ordinary model cook, even when
    // another model uses the same path as a PNG.
    images: BTreeMap<PathBuf, Usage>,
}

#[derive(Debug, Serialize, Deserialize)]
enum Usage {
    Buffer,
    Images(BTreeMap<ImageRole, Vec<Binding>>),
}

#[derive(Debug, Serialize, Deserialize)]
struct Binding {
    asset: AssetId,
    primitive: usize,
    slot: usize,
}

impl Manifest {
    pub(super) fn record(&mut self, path: &Path, asset: AssetId, usage: ExternalUse) {
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
        {
            return;
        }
        match usage {
            ExternalUse::Buffer => {
                self.images.insert(path.to_owned(), Usage::Buffer);
            }
            ExternalUse::Image {
                primitive,
                slot,
                role,
            } => {
                if let Usage::Images(roles) = self
                    .images
                    .entry(path.to_owned())
                    .or_insert_with(|| Usage::Images(BTreeMap::new()))
                {
                    roles.entry(role).or_default().push(Binding {
                        asset,
                        primitive,
                        slot,
                    });
                }
            }
        }
    }

    pub(super) fn patch_images(
        &self,
        changes: &BTreeSet<PathBuf>,
        snapshot: &Snapshot,
        catalog: &mut Catalog,
        cache: &mut Cache<'_>,
    ) -> Result<bool, Error> {
        for path in changes {
            match self.images.get(path) {
                None => continue, // Unreferenced file in the source tree.
                Some(Usage::Buffer) => return Ok(false),
                Some(Usage::Images(roles)) => {
                    if roles.values().any(Vec::is_empty) {
                        return Ok(false);
                    }
                    for binding in roles.values().flatten() {
                        if catalog
                            .assets
                            .get(&binding.asset)
                            .and_then(|asset| asset.primitives.get(binding.primitive))
                            .and_then(|part| part.images.get(binding.slot))
                            .copied()
                            .flatten()
                            .is_none()
                        {
                            return Ok(false);
                        }
                    }
                }
            }
        }
        for path in changes {
            let Some(Usage::Images(roles)) = self.images.get(path) else {
                continue;
            };
            let bytes = snapshot.edited_image(path)?;
            for (&role, bindings) in roles {
                let id = bindings[0].asset;
                let reference = cache
                    .convert(id, "image", role.name(), &bytes, || {
                        gltf::cook_image_bytes(&bytes, role)
                    })
                    .map_err(|e| format!("asset {id} ({}): {e}", path.display()))?;
                for binding in bindings {
                    // All destinations were checked above before applying any edits.
                    let asset = catalog.assets.get_mut(&binding.asset).unwrap();
                    asset.primitives[binding.primitive].images[binding.slot] = Some(reference);
                }
            }
        }
        Ok(true)
    }
}

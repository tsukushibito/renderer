use crate::{Error, PROFILE, store};
use renderer_asset::{AssetId, BlobRef, Digest};
use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Serialize)]
pub(super) struct Event {
    asset_id: AssetId,
    stage: &'static str,
    action: &'static str,
    reason: &'static str,
    input_sha256: Digest,
    recipe_sha256: Digest,
    bytes: u64,
    hash: Digest,
}

#[derive(Serialize)]
pub(super) struct Report {
    pub generation: Digest,
    pub converter_sha256: Digest,
    pub events: Vec<Event>,
    pub published_bytes: u64,
    pub generated_bytes: u64,
}

pub(super) struct Cache<'a> {
    converter: Digest,
    directory: &'a Path,
    output: &'a Path,
    recipes: BTreeMap<Digest, BlobRef>,
    published_bytes: u64,
    events: Vec<Event>,
}

impl<'a> Cache<'a> {
    pub fn new(directory: &'a Path, output: &'a Path) -> Result<Self, Error> {
        // The executable includes the selected Rust and native converter code and build flags.
        let mut digest = Sha256::new();
        let mut executable = std::fs::File::open(std::env::current_exe()?)?;
        std::io::copy(&mut executable, &mut digest)?;
        Ok(Self {
            converter: format!("{:x}", digest.finalize()).parse()?,
            directory,
            output,
            recipes: BTreeMap::new(),
            published_bytes: 0,
            events: Vec::new(),
        })
    }

    pub fn convert(
        &mut self,
        asset: AssetId,
        stage: &'static str,
        variant: &str,
        inputs: &[u8],
        convert: impl FnOnce() -> Result<Vec<u8>, Error>,
    ) -> Result<BlobRef, Error> {
        // The recipe digest locates a candidate. Its output digest separately verifies bytes.
        let input_sha256 = Digest::of(inputs);
        let key = Digest::of(&serde_json::to_vec(&(
            self.converter,
            PROFILE,
            stage,
            variant,
            input_sha256,
        ))?);
        let (object, action, reason) = if let Some(existing) = self.recipes.get(&key) {
            (*existing, "reused", "shared recipe")
        } else {
            let recipe_path = self.directory.join("recipes").join(key.to_string());
            let record = store::read_if_present(&recipe_path)?;
            let mut bytes = None;
            if let Some(reference) = record
                .as_deref()
                .and_then(|v| serde_json::from_slice::<BlobRef>(v).ok())
            {
                let path = self
                    .directory
                    .join("objects")
                    .join(reference.hash.to_string());
                if let Some(cached) = store::read_if_present(&path)?
                    && BlobRef::of(&cached) == reference
                {
                    bytes = Some((reference, cached));
                }
            }
            let (reference, data, action, reason) = if let Some((reference, data)) = bytes {
                (reference, data, "reused", "verified cache hit")
            } else {
                let data = convert()?;
                let reference = BlobRef::of(&data);
                store::publish(
                    &self
                        .directory
                        .join("objects")
                        .join(reference.hash.to_string()),
                    &data,
                )?;
                store::publish(&recipe_path, &serde_json::to_vec(&reference)?)?;
                let reason = if record.is_some() {
                    "invalid cache entry"
                } else {
                    "new recipe inputs or converter"
                };
                (reference, data, "built", reason)
            };
            let path = self.output.join("objects").join(reference.hash.to_string());
            if store::read_if_present(&path)?.is_none_or(|old| BlobRef::of(&old) != reference) {
                store::publish(&path, &data)?;
                self.published_bytes += reference.size;
            }
            self.recipes.insert(key, reference);
            (reference, action, reason)
        };
        self.events.push(Event {
            asset_id: asset,
            stage,
            action,
            reason,
            input_sha256,
            recipe_sha256: key,
            bytes: object.size,
            hash: object.hash,
        });
        Ok(object)
    }

    pub fn report(self, generation: Digest) -> Report {
        let generated_bytes = self
            .events
            .iter()
            .filter(|e| e.action == "built")
            .map(|e| e.bytes)
            .sum();
        Report {
            generation,
            converter_sha256: self.converter,
            published_bytes: self.published_bytes,
            generated_bytes,
            events: self.events,
        }
    }
}

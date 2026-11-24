//! Load-time skin preparation and deterministic playback of sampled model-space poses.
use renderer_asset::AssetId;
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    num::NonZeroU32,
    path::{Component, Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    version: u32,
    models: BTreeMap<String, ModelEntry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelEntry {
    id: AssetId,
    skin: PathBuf,
    color: PathBuf,
    extent: [u32; 2],
    clips: BTreeMap<String, Clip>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct Clip {
    first: usize,
    frames: usize,
    hz: NonZeroU32,
    looping: bool,
}

pub struct CharacterCatalog {
    directory: PathBuf,
    models: BTreeMap<String, ModelEntry>,
}

impl CharacterCatalog {
    pub fn open(path: &Path) -> PipelineResult<Self> {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let file: CatalogFile = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if file.version != 1 {
            return Err(format!(
                "{}: unsupported character catalog version {}",
                path.display(),
                file.version
            )
            .into());
        }
        let mut ids = HashSet::new();
        for (alias, model) in &file.models {
            if alias.is_empty() || !ids.insert(model.id) {
                return Err(format!(
                    "{}: invalid model name or duplicate ID for {alias}",
                    path.display()
                )
                .into());
            }
        }
        Ok(Self {
            directory: path.parent().ok_or("missing catalog directory")?.into(),
            models: file.models,
        })
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.models.keys().map(String::as_str)
    }
    pub fn id(&self, name: &str) -> Option<AssetId> {
        self.models.get(name).map(|entry| entry.id)
    }

    pub fn load(&self, id: AssetId) -> PipelineResult<AnimatedModel> {
        let entry = self
            .models
            .values()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("unknown character asset {id}"))?;
        let read = |relative: &Path| -> PipelineResult<Vec<u8>> {
            if relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
            {
                return Err(format!("invalid asset path {}", relative.display()).into());
            }
            let path = self.directory.join(relative);
            let resolved = path
                .canonicalize()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if !resolved.starts_with(&self.directory) {
                return Err(format!("asset outside catalog directory: {}", path.display()).into());
            }
            fs::read(&resolved).map_err(|e| format!("{}: {e}", resolved.display()).into())
        };
        let model = SkinnedModel::decode(&read(&entry.skin)?)
            .map_err(|e| format!("{}: {e}", entry.skin.display()))?;
        if entry.clips.is_empty() {
            return Err(format!("no animation clips for {id}").into());
        }
        for (name, clip) in &entry.clips {
            if name.is_empty()
                || clip.frames == 0
                || clip
                    .first
                    .checked_add(clip.frames)
                    .is_none_or(|end| end > model.frames)
            {
                return Err(format!("invalid frame range in {id}/{name}").into());
            }
        }
        let pixels = read(&entry.color)?;
        let [w, h] = entry.extent;
        if w == 0
            || h == 0
            || u64::from(w)
                .checked_mul(u64::from(h))
                .and_then(|n| n.checked_mul(4))
                != Some(pixels.len() as u64)
        {
            return Err(format!(
                "{}: RGBA extent and byte count disagree",
                entry.color.display()
            )
            .into());
        }
        Ok(AnimatedModel {
            model,
            color: pixels,
            extent: entry.extent,
            clips: entry.clips.clone(),
        })
    }
}

pub struct AnimatedModel {
    pub model: SkinnedModel,
    color: Vec<u8>,
    extent: [u32; 2],
    clips: BTreeMap<String, Clip>,
}

impl AnimatedModel {
    /// Top-row-first sRGB RGBA8 pixels.
    pub fn color(&self) -> (&[u8], [u32; 2]) {
        (&self.color, self.extent)
    }
    pub fn clip_names(&self) -> impl Iterator<Item = &str> {
        self.clips.keys().map(String::as_str)
    }
    pub fn clip(&self, name: &str) -> Option<AnimationClip<'_>> {
        let clip = *self.clips.get(name)?;
        let start = clip.first * self.model.joints;
        let length = clip.frames * self.model.joints;
        Some(AnimationClip {
            clip,
            joints: self.model.joints,
            poses: &self.model.poses[start..start + length],
        })
    }
}

#[derive(Clone, Copy)]
pub struct AnimationClip<'a> {
    clip: Clip,
    joints: usize,
    poses: &'a [Transform],
}
impl<'a> AnimationClip<'a> {
    pub fn frames(&self) -> usize {
        self.clip.frames
    }
    /// Sample a tick offset relative to playback start; resetting the offset
    /// restarts playback. Integral arithmetic preserves replay timing.
    pub fn sample(&self, elapsed_ticks: u64, tick_rate: NonZeroU32) -> &'a [Transform] {
        let sample = u128::from(elapsed_ticks) * u128::from(self.clip.hz.get())
            / u128::from(tick_rate.get());
        let count = self.clip.frames as u128;
        let index = if self.clip.looping {
            sample % count
        } else {
            sample.min(count - 1)
        };
        let first = index as usize * self.joints;
        &self.poses[first..first + self.joints]
    }
}

use renderer::{
    mesh::{JointWeights, MeshData},
    pbr::{PbrVertex, Transform},
    pipeline::PipelineResult,
};

pub struct SkinnedModel {
    vertices: Vec<PbrVertex>,
    weights: Vec<JointWeights>,
    indices: Vec<u32>,
    joints: usize,
    frames: usize,
    poses: Vec<Transform>,
}

fn record_bytes<'a>(input: &mut &'a [u8], count: usize, stride: usize) -> PipelineResult<&'a [u8]> {
    let size = count.checked_mul(stride).ok_or("character size overflow")?;
    let (records, next) = input.split_at_checked(size).ok_or("short character data")?;
    *input = next;
    Ok(records)
}

fn floats<const N: usize>(record: &[u8], offset: usize) -> [f32; N] {
    std::array::from_fn(|i| {
        let start = offset + i * 4;
        f32::from_le_bytes(record[start..start + 4].try_into().unwrap())
    })
}

impl SkinnedModel {
    /// Decode the cooked mesh and joint matrix stream at asset preparation time.
    pub fn decode(input: &[u8]) -> PipelineResult<Self> {
        let mut data = input.strip_prefix(b"SURSKIN1").ok_or("character schema")?;
        let header = record_bytes(&mut data, 4, 4)?;
        let [points, elements, joints, frames] = std::array::from_fn(|i| {
            u32::from_le_bytes(header[i * 4..i * 4 + 4].try_into().unwrap()) as usize
        });
        if joints == 0 || frames == 0 {
            return Err("empty walk clip".into());
        }
        let vertex_bytes = record_bytes(&mut data, points, 72)?;
        let index_bytes = record_bytes(&mut data, elements, 4)?;
        let all_joints = frames.checked_mul(joints).ok_or("walk clip overflow")?;
        let matrices = record_bytes(&mut data, all_joints, 48)?;
        if !data.is_empty() {
            return Err("trailing character data".into());
        }
        let mut vertices = Vec::with_capacity(points);
        let mut weights = Vec::with_capacity(points);
        for v in vertex_bytes.as_chunks::<72>().0 {
            vertices.push(PbrVertex::new(
                floats(v, 0),
                floats(v, 12),
                floats(v, 24),
                floats(v, 40),
            )?);
            weights.push(JointWeights {
                joints: std::array::from_fn(|i| u16::from_le_bytes([v[48 + 2 * i], v[49 + 2 * i]])),
                weights: floats(v, 56),
            });
        }
        for influence in &weights {
            if influence.joints.iter().any(|&j| usize::from(j) >= joints)
                || influence.weights.iter().any(|&w| !w.is_finite() || w < 0.)
                || influence.weights.iter().all(|&w| w == 0.)
            {
                return Err("invalid cooked joint indices or weights".into());
            }
        }
        let indices: Vec<u32> = index_bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|x| u32::from_le_bytes(*x))
            .collect();
        if indices.is_empty()
            || !indices.len().is_multiple_of(3)
            || indices.iter().any(|&index| index as usize >= points)
        {
            return Err("invalid cooked triangle indices".into());
        }
        let mut poses = Vec::with_capacity(all_joints);
        for matrix in matrices.as_chunks::<48>().0 {
            poses.push(Transform::new(std::array::from_fn(|i| {
                floats(matrix, i * 16)
            }))?);
        }
        Ok(Self {
            vertices,
            indices,
            weights,
            joints,
            frames,
            poses,
        })
    }

    pub fn mesh_data(&self) -> PipelineResult<MeshData> {
        Ok(MeshData::skinned_pbr(
            &self.vertices,
            &self.indices,
            self.joints,
            &self.weights,
        )?)
    }

    pub fn indices(&self) -> &[u32] {
        &self.indices
    }
    pub fn vertices(&self) -> impl Iterator<Item = (&PbrVertex, &JointWeights)> {
        self.vertices.iter().zip(&self.weights)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clip_clock_resamples_and_holds_or_loops_without_overflow() {
        let hz = NonZeroU32::new(60).unwrap();
        let translated =
            Transform::new([[1., 0., 0., 1.], [0., 1., 0., 0.], [0., 0., 1., 0.]]).unwrap();
        let poses = [Transform::IDENTITY, translated];
        let mut clip = AnimationClip {
            clip: Clip {
                first: 0,
                frames: 2,
                hz: NonZeroU32::new(30).unwrap(),
                looping: false,
            },
            joints: 1,
            poses: &poses,
        };
        assert_eq!(clip.sample(1, hz), &poses[..1]);
        assert_eq!(clip.sample(2, hz), &poses[1..]);
        assert_eq!(clip.sample(u64::MAX, hz), &poses[1..]);
        clip.clip.looping = true;
        assert_eq!(clip.sample(4, hz), &poses[..1]);
    }
}

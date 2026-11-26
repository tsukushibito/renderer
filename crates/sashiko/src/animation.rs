//! Load-time skin preparation and deterministic playback of sampled model-space poses.
pub use sashiko_animation_character as character;
use sashiko_asset::{AssetId, Digest};
use serde::{Deserialize, Serialize};

type CatalogResult<T> = Result<T, Box<dyn std::error::Error>>;

#[cfg(feature = "rendering")]
mod rig;
#[cfg(feature = "rendering")]
use rig::Rig;
#[cfg(feature = "rendering")]
pub use rig::{PaletteBatch, PaletteRequest, PaletteWorkspace, PhaseAnimation, PreparedAnimation};
pub use sashiko_animation::{
    Advance, AnimationError, Clip as PoseClip, ClipTimeline, JointMask, LocalBuffer, LocalPose,
    Marker, PhaseMap, Playback, PoseSample, PoseWorkspace, Skeleton,
};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CatalogFile {
    version: u32,
    models: BTreeMap<String, ModelEntry>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ModelEntry {
    id: AssetId,
    skin: PathBuf,
    color: PathBuf,
    extent: [u32; 2],
    clips: BTreeMap<String, Clip>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    tracks: BTreeMap<String, Tracks>,
}

#[derive(Deserialize, Serialize, Clone, Copy)]
#[serde(deny_unknown_fields)]
struct Clip {
    first: usize,
    frames: usize,
    hz: NonZeroU32,
    looping: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Tracks {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    events: Vec<ClipEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sync_markers: Vec<ClipEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    root_translation: Vec<[f64; 3]>,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClipEvent {
    frame: usize,
    id: u32,
}

/// Serializable clip timing and gameplay tracks for simulation and replay.
#[derive(Clone, Serialize, Deserialize)]
pub struct TimelineAsset {
    frames: usize,
    hz: NonZeroU32,
    looping: bool,
    tracks: Tracks,
}
impl TimelineAsset {
    pub fn prepare(&self) -> CatalogResult<ClipTimeline> {
        Ok(ClipTimeline::new(
            self.frames,
            self.hz,
            self.looping,
            self.tracks
                .events
                .iter()
                .map(|e| sashiko_animation::Marker {
                    frame: e.frame,
                    id: e.id,
                })
                .collect(),
            self.tracks.root_translation.clone(),
        )?
        .with_sync_markers(
            self.tracks
                .sync_markers
                .iter()
                .map(|e| sashiko_animation::Marker {
                    frame: e.frame,
                    id: e.id,
                })
                .collect(),
        )?)
    }
}
impl ModelEntry {
    fn timeline(&self, name: &str) -> CatalogResult<TimelineAsset> {
        let clip = self
            .clips
            .get(name)
            .ok_or_else(|| format!("unknown animation clip {name}"))?;
        Ok(TimelineAsset {
            frames: clip.frames,
            hz: clip.hz,
            looping: clip.looping,
            tracks: self.tracks.get(name).cloned().unwrap_or_default(),
        })
    }
}

pub struct CharacterCatalog {
    definition: Digest,
    #[cfg(feature = "rendering")]
    directory: PathBuf,
    models: BTreeMap<String, ModelEntry>,
}

impl CharacterCatalog {
    pub fn open(path: &Path) -> CatalogResult<Self> {
        let path = path
            .canonicalize()
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Self::parse(&path, &fs::read(&path)?)
    }

    /// Parse an on-disk or embedded catalog. Relative asset paths resolve beside this file.
    pub fn parse(path: &Path, bytes: &[u8]) -> CatalogResult<Self> {
        let file: CatalogFile =
            serde_json::from_slice(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
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
            if model
                .tracks
                .keys()
                .any(|name| !model.clips.contains_key(name))
            {
                return Err(format!("unknown clip in tracks for {alias}").into());
            }
            if alias.is_empty() || !ids.insert(model.id) {
                return Err(format!(
                    "{}: invalid model name or duplicate ID for {alias}",
                    path.display()
                )
                .into());
            }
        }
        Ok(Self {
            definition: Digest::of(&serde_json::to_vec(&file)?),
            #[cfg(feature = "rendering")]
            directory: path.parent().ok_or("missing catalog directory")?.into(),
            models: file.models,
        })
    }
    /// Digest of the typed catalog, independent of formatting or its installation path.
    pub fn definition_digest(&self) -> Digest {
        self.definition
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.models.keys().map(String::as_str)
    }
    pub fn id(&self, name: &str) -> Option<AssetId> {
        self.models.get(name).map(|entry| entry.id)
    }

    pub fn has_clip(&self, model: &str, clip: &str) -> bool {
        self.models
            .get(model)
            .is_some_and(|m| m.clips.contains_key(clip))
    }

    /// Read simulation timing and markers without loading mesh or texture bytes.
    pub fn timeline(&self, model: &str, clip: &str) -> CatalogResult<TimelineAsset> {
        self.models
            .get(model)
            .ok_or_else(|| format!("unknown character {model}"))?
            .timeline(clip)
    }

    #[cfg(feature = "rendering")]
    pub fn load(&self, id: AssetId) -> PipelineResult<AnimatedModel> {
        let entry = self
            .models
            .values()
            .find(|entry| entry.id == id)
            .ok_or_else(|| format!("unknown character asset {id}"))?;
        let read = |relative: &Path| -> PipelineResult<Vec<u8>> {
            if relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
            {
                return Err(format!("invalid asset path {}", relative.display()).into());
            }
            let path = self.directory.join(relative);
            let resolved = path
                .canonicalize()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            if !resolved.starts_with(self.directory.canonicalize()?) {
                return Err(format!("asset outside catalog directory: {}", path.display()).into());
            }
            fs::read(&resolved).map_err(|e| format!("{}: {e}", resolved.display()).into())
        };
        let skin_bytes = read(&entry.skin)?;
        let model = SkinnedModel::decode(&skin_bytes)
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
            content: [Digest::of(&skin_bytes), Digest::of(&pixels)],
            model,
            color: pixels,
            extent: entry.extent,
            clips: entry.clips.clone(),
            timelines: entry
                .clips
                .keys()
                .map(|name| Ok((name.clone(), entry.timeline(name)?.prepare()?)))
                .collect::<PipelineResult<_>>()?,
        })
    }
}

#[cfg(feature = "rendering")]
pub struct AnimatedModel {
    content: [Digest; 2],
    model: SkinnedModel,
    color: Vec<u8>,
    extent: [u32; 2],
    clips: BTreeMap<String, Clip>,
    timelines: BTreeMap<String, ClipTimeline>,
}

#[cfg(feature = "rendering")]
impl AnimatedModel {
    pub fn has_skeleton(&self) -> bool {
        self.model.skeleton.is_some()
    }

    /// Validated mesh and skin paired with this catalog entry.
    pub fn model(&self) -> &SkinnedModel {
        &self.model
    }

    /// Digests of the exact skin and color bytes consumed during preparation.
    pub fn content_digests(&self) -> [Digest; 2] {
        self.content
    }

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

#[cfg(feature = "rendering")]
#[derive(Clone, Copy)]
pub struct AnimationClip<'a> {
    clip: Clip,
    joints: usize,
    poses: &'a [Transform],
}
#[cfg(feature = "rendering")]
impl<'a> AnimationClip<'a> {
    pub fn frames(&self) -> usize {
        self.clip.frames
    }
    /// Sample a tick offset relative to playback start; resetting the offset
    /// restarts playback. Integral arithmetic preserves replay timing.
    pub fn sample(&self, elapsed_ticks: u64, tick_rate: NonZeroU32) -> &'a [Transform] {
        let first = self.sample_index(u128::from(elapsed_ticks), tick_rate) * self.joints;
        &self.poses[first..first + self.joints]
    }

    fn sample_index(&self, elapsed_ticks: u128, tick_rate: NonZeroU32) -> usize {
        let sample = elapsed_ticks * u128::from(self.clip.hz.get()) / u128::from(tick_rate.get());
        let count = self.clip.frames as u128;
        let index = if self.clip.looping {
            sample % count
        } else {
            sample.min(count - 1)
        };
        index as usize
    }
}

#[cfg(feature = "rendering")]
use sashiko_renderer::{
    mesh::{JointWeights, MeshData},
    pbr::{PbrVertex, Transform},
    pipeline::PipelineResult,
};

#[cfg(feature = "rendering")]
pub struct SkinnedModel {
    vertices: Vec<PbrVertex>,
    weights: Vec<JointWeights>,
    indices: Vec<u32>,
    joints: usize,
    frames: usize,
    poses: std::sync::Arc<[Transform]>,
    skeleton: Option<Rig>,
}

#[cfg(feature = "rendering")]
impl SkinnedModel {
    /// Prepare renderer vertices, bind transforms and sampled palettes from a cooked skin.
    pub fn decode(input: &[u8]) -> PipelineResult<Self> {
        let skin = sashiko_asset::skin::Skin::decode(input)?;
        let poses = skin
            .poses()
            .map(Transform::new)
            .collect::<Result<Vec<_>, _>>()?;
        let skeleton = skin.bind_joints.map(Rig::from_bind_joints).transpose()?;
        let mut vertices = Vec::with_capacity(skin.vertices.len());
        let mut weights = Vec::with_capacity(skin.vertices.len());
        for vertex in skin.vertices {
            let surface = vertex.surface;
            vertices.push(PbrVertex::new(
                surface.position,
                surface.normal,
                surface.tangent,
                surface.uv,
            )?);
            weights.push(JointWeights {
                joints: vertex.joints,
                weights: vertex.weights,
            });
        }
        Ok(Self {
            skeleton,
            vertices,
            indices: skin.indices,
            weights,
            joints: skin.joint_count,
            frames: skin.frame_count,
            poses: poses.into(),
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

#[cfg(all(test, feature = "rendering"))]
mod tests {
    use super::*;
    #[test]
    fn catalog_tracks_reach_headless_and_skeletal_playback() -> PipelineResult<()> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/survivor/characters/catalog.json");
        let mut data: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)?;
        let model = &mut data["models"]["primary"];
        model["clips"]["walk"]["frames"] = 2.into();
        model["tracks"]["walk"] = serde_json::json!({"events":[{"frame":1,"id":7}],"root_translation":[[0,0,0],[0.5,0,0],[1,0,0]]});
        let catalog = CharacterCatalog::parse(&path, &serde_json::to_vec(&data)?)?;
        let rate = 60.try_into()?;
        let timeline = catalog.timeline("primary", "walk")?.prepare()?;
        let model = catalog.load(catalog.id("primary").unwrap())?;
        let mut animation = model.prepare_animation("walk")?;
        let (clip, _) = animation.rigged().unwrap();
        let mut playback = Playback::new(clip, rate);
        let mut events = [0; 2];
        let advanced = playback.advance_to(3, &mut events)?;
        assert_eq!(advanced.translation, [1.5, 0., 0.]);
        assert_eq!(&events[..advanced.event_count], &[7, 7]);
        assert_eq!(
            (1..=3)
                .flat_map(|t| timeline.events_at(t, rate))
                .collect::<Vec<_>>(),
            events
        );
        assert_eq!(playback.advance_to(3, &mut events)?.event_count, 0);
        Ok(())
    }
    #[test]
    fn legacy_skin_prepares_direct_palette_playback() -> PipelineResult<()> {
        let legacy = include_bytes!("../../../assets/verification/animation/v1.skin");
        let mesh = SkinnedModel::decode(legacy)?;
        let rate = NonZeroU32::new(60).unwrap();
        let clips = BTreeMap::from([(
            "legacy".into(),
            Clip {
                first: 1,
                frames: 2,
                hz: rate,
                looping: true,
            },
        )]);
        let model = AnimatedModel {
            model: mesh,
            timelines: BTreeMap::new(),
            clips,
            content: [Digest::of(legacy); 2],
            color: vec![],
            extent: [0, 0],
        };
        let mut prepared = model.prepare_animation("legacy")?;
        assert!(prepared.rigged().is_none());
        for tick in [0, 1, 2, u64::MAX] {
            assert_eq!(
                prepared.sample(tick, rate)?,
                model.clip("legacy").unwrap().sample(tick, rate)
            );
        }
        let mut phases = prepared.into_four_phases();
        let ticks = [0, 13, 26, u64::MAX];
        let reference = model.clip("legacy").unwrap();
        assert_eq!(
            phases.sample(ticks, rate)?,
            ticks.map(|t| reference.sample(t, rate))
        );
        Ok(())
    }
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

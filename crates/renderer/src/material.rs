//! Persistent surface materials shared by mesh draws. Create and edit them
//! outside graph construction. Cloning a snapshot retains that exact revision.
mod definition;
mod residency;
use crate::pbr::{self, DefaultViews, MaterialUniform};
pub use crate::pbr::{MaterialFactors, MaterialSamplers, MaterialTextures};
pub use definition::{CustomMaterial, MaterialDefinition, Parameter, Value};
use renderer_core::graph::{Graph, GraphError, ImageView, ImportImage};
use renderer_core::program::SamplerDescriptor;
pub(crate) use residency::MaterialCheckpoint;
use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

/// Alpha interpretation used for scene classification and shadow rendering.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AlphaMode {
    #[default]
    Opaque,
    Mask,
    Blend,
}

/// Registered image and the mip levels that this material can sample.
/// The loader keeps the registration owner until the last using graph is submitted.
#[derive(Clone, Debug)]
pub struct MaterialTexture {
    image: ImportImage,
    mips: std::ops::Range<u32>,
}

impl MaterialTexture {
    pub fn new(image: ImportImage, mips: std::ops::Range<u32>) -> Result<Self, GraphError> {
        let map = Self { image, mips };
        map.validate()?;
        Ok(map)
    }

    fn validate(&self) -> Result<(), GraphError> {
        definition::check_texture(&self.image)?;
        if self.mips.is_empty()
            || self.mips.end > self.image.desc().mip_levels
            || !self
                .mips
                .clone()
                .all(|level| self.image.is_mip_initialized(level))
        {
            return Err(GraphError::invalid_input(
                "material requires an initialized mip range",
            ));
        }
        Ok(())
    }

    fn view(&self, graph: &mut Graph) -> Result<ImageView, GraphError> {
        let image = graph.import_image("material texture", &self.image)?;
        graph.sampled_image_view(image, self.mips.start, self.mips.end - self.mips.start)
    }
}

impl From<ImportImage> for MaterialTexture {
    fn from(image: ImportImage) -> Self {
        let mips = 0..image.desc().mip_levels;
        Self { image, mips }
    }
}

#[derive(Clone, Debug, Default)]
pub struct PbrParams {
    pub factors: MaterialFactors,
    pub textures: MaterialTextures<Option<MaterialTexture>>,
    pub samplers: MaterialSamplers,
}

impl PbrParams {
    /// Start with the standard dielectric and override its linear base color.
    pub fn color(base_color: [f32; 4]) -> Self {
        Self {
            factors: MaterialFactors {
                base_color,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct UnlitParams {
    pub color: [f32; 4],
}
impl Default for UnlitParams {
    fn default() -> Self {
        Self { color: [1.; 4] }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct MaterialHandle(u64);

#[derive(Clone, Debug)]
pub struct PbrMaterial {
    source: PbrSource,
    pub(crate) uploaded_by: Option<renderer_core::runtime::FrameId>,
    textures: MaterialTextures<Option<MaterialTexture>>,
    samplers: MaterialSamplers,
    alpha: AlphaMode,
}
#[derive(Clone, Debug)]
enum PbrSource {
    Inline([u8; crate::passes::pbr::MATERIAL_BYTES]),
    Imported(Arc<renderer_core::graph::ImportBuffer>),
}
impl PbrMaterial {
    pub(crate) fn is_resident(&self) -> bool {
        matches!(self.source, PbrSource::Imported(_))
    }
    pub(crate) fn coverage_image(&self) -> Option<&ImportImage> {
        self.textures
            .base_color
            .as_ref()
            .filter(|_| self.alpha == AlphaMode::Mask)
            .map(|map| &map.image)
    }

    fn new(params: PbrParams) -> Result<Self, GraphError> {
        let encoded = params.factors.encode()?;
        Self::from_source(
            PbrSource::Inline(encoded),
            params.factors.alpha_mode,
            params.textures,
            params.samplers,
        )
    }
    fn from_source(
        source: PbrSource,
        alpha: AlphaMode,
        textures: MaterialTextures<Option<MaterialTexture>>,
        samplers: MaterialSamplers,
    ) -> Result<Self, GraphError> {
        for texture in [
            &textures.base_color,
            &textures.normal,
            &textures.orm,
            &textures.emissive,
        ]
        .into_iter()
        .flatten()
        {
            texture.validate()?
        }
        Ok(Self {
            source,
            uploaded_by: None,
            alpha,
            textures,
            samplers,
        })
    }
    /// Keep a snapshot of an accepted resident uniform and its four texture maps.
    /// The asset owner retains their runtime registrations while building future frames.
    pub fn resident(
        uniform: &pbr::ResidentMaterialUniform,
        textures: MaterialTextures<impl Into<MaterialTexture>>,
        samplers: MaterialSamplers,
    ) -> Result<Self, GraphError> {
        Self::from_source(
            PbrSource::Imported(Arc::new(uniform.snapshot()?.clone())),
            uniform.alpha_mode(),
            MaterialTextures {
                base_color: Some(textures.base_color.into()),
                normal: Some(textures.normal.into()),
                orm: Some(textures.orm.into()),
                emissive: Some(textures.emissive.into()),
            },
            samplers,
        )
    }
    /// Import this revision in a graph. Defaults are needed only for omitted maps.
    pub fn resolve(
        &self,
        graph: &mut Graph,
        name: &str,
        defaults: Option<DefaultViews>,
    ) -> Result<pbr::GraphMaterial, GraphError> {
        let uniform = match &self.source {
            PbrSource::Inline(bytes) => MaterialUniform::append(graph, name, bytes, self.alpha)?,
            PbrSource::Imported(snapshot) => {
                MaterialUniform::import_snapshot(graph, name, snapshot, self.alpha)?
            }
        };
        let mut import =
            |image: &Option<MaterialTexture>| image.as_ref().map(|map| map.view(graph)).transpose();
        let textures = MaterialTextures {
            base_color: import(&self.textures.base_color)?,
            normal: import(&self.textures.normal)?,
            orm: import(&self.textures.orm)?,
            emissive: import(&self.textures.emissive)?,
        };
        let textures = match textures {
            MaterialTextures {
                base_color: Some(base_color),
                normal: Some(normal),
                orm: Some(orm),
                emissive: Some(emissive),
            } => MaterialTextures {
                base_color,
                normal,
                orm,
                emissive,
            },
            other => defaults
                .ok_or_else(|| GraphError::invalid_input("missing material texture defaults"))?
                .material_textures(other)?,
        };
        Ok(pbr::GraphMaterial::new(uniform, textures).with_samplers(self.samplers))
    }
}
pub(crate) fn texture_view(
    graph: &mut Graph,
    image: &ImportImage,
) -> Result<ImageView, GraphError> {
    let handle = graph.import_image("material texture", image)?;
    graph.sampled_image_view(handle, 0, image.desc().mip_levels)
}

#[derive(Clone, Debug)]
pub enum MaterialInstance {
    Pbr(PbrMaterial),
    Unlit(UnlitParams),
    Custom(CustomMaterial),
}
impl MaterialInstance {
    pub(crate) fn visit_samplers(&self, mut visit: impl FnMut(SamplerDescriptor)) {
        match self {
            Self::Pbr(material) => {
                let maps = material.samplers;
                for sampler in [maps.base_color, maps.normal, maps.orm, maps.emissive] {
                    visit(sampler);
                }
            }
            Self::Custom(material) => {
                for parameter in material.definition().parameters() {
                    if let Some(Value::Sampler(sampler)) = material.value(parameter.name()) {
                        visit(*sampler);
                    }
                }
            }
            Self::Unlit(_) => {}
        }
    }

    pub fn alpha_mode(&self) -> AlphaMode {
        match self {
            Self::Pbr(p) => p.alpha,
            Self::Unlit(_) => AlphaMode::Opaque,
            Self::Custom(p) => p.definition().alpha_mode(),
        }
    }
}

/// Handles are unique across registries, including after removal.
#[derive(Default)]
pub struct Materials {
    revisions: HashMap<MaterialHandle, u64>,
    entries: HashMap<MaterialHandle, Arc<MaterialInstance>>,
    dirty: std::collections::BTreeSet<MaterialHandle>,
    owned_uniforms: Vec<(
        std::sync::Weak<renderer_core::graph::ImportBuffer>,
        pbr::ResidentMaterialUniform,
    )>,
}
impl Materials {
    fn insert(&mut self, instance: Arc<MaterialInstance>) -> MaterialHandle {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let handle = MaterialHandle(
            NEXT.try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
                .expect("material handle space exhausted"),
        );
        if matches!(instance.as_ref(), MaterialInstance::Pbr(p) if !p.is_resident()) {
            self.dirty.insert(handle);
        }
        self.entries.insert(handle, instance);
        self.revisions.insert(handle, 1);
        handle
    }
    pub(crate) fn revision(&self, handle: MaterialHandle) -> Result<u64, GraphError> {
        self.revisions
            .get(&handle)
            .copied()
            .ok_or_else(invalid_handle)
    }
    fn bump_revision(&mut self, handle: MaterialHandle) {
        let rev = self
            .revisions
            .get_mut(&handle)
            .expect("registered material");
        *rev = rev.checked_add(1).expect("material revisions exhausted");
    }
    pub fn create_pbr(&mut self, params: PbrParams) -> Result<MaterialHandle, GraphError> {
        Ok(self.insert(Arc::new(MaterialInstance::Pbr(PbrMaterial::new(params)?))))
    }
    pub fn create_resident_pbr(&mut self, pbr: PbrMaterial) -> MaterialHandle {
        self.insert(Arc::new(MaterialInstance::Pbr(pbr)))
    }
    pub fn update_resident_pbr(
        &mut self,
        handle: MaterialHandle,
        pbr: PbrMaterial,
    ) -> Result<(), GraphError> {
        let slot = self.entries.get_mut(&handle).ok_or_else(invalid_handle)?;
        if !matches!(slot.as_ref(), MaterialInstance::Pbr(_)) {
            return Err(GraphError::invalid_input("expected PBR material"));
        }
        if pbr.is_resident() {
            self.dirty.remove(&handle);
        } else {
            self.dirty.insert(handle);
        }
        *slot = Arc::new(MaterialInstance::Pbr(pbr));
        self.bump_revision(handle);
        Ok(())
    }
    pub fn create_unlit(&mut self, params: UnlitParams) -> Result<MaterialHandle, GraphError> {
        check_color(params.color)?;
        Ok(self.insert(Arc::new(MaterialInstance::Unlit(params))))
    }
    pub fn create_custom(
        &mut self,
        definition: &MaterialDefinition,
        values: &[(&str, Value)],
    ) -> Result<MaterialHandle, GraphError> {
        Ok(
            self.insert(Arc::new(MaterialInstance::Custom(CustomMaterial::new(
                definition, values,
            )?))),
        )
    }
    pub fn get(&self, handle: MaterialHandle) -> Result<&MaterialInstance, GraphError> {
        Ok(self.entry(handle)?.as_ref())
    }
    fn entry(&self, handle: MaterialHandle) -> Result<&Arc<MaterialInstance>, GraphError> {
        self.entries.get(&handle).ok_or_else(invalid_handle)
    }
    pub fn snapshot(&self, handle: MaterialHandle) -> Result<Arc<MaterialInstance>, GraphError> {
        self.entry(handle).cloned()
    }
    pub fn duplicate(&mut self, handle: MaterialHandle) -> Result<MaterialHandle, GraphError> {
        Ok(self.insert(self.snapshot(handle)?))
    }
    pub fn remove(&mut self, handle: MaterialHandle) -> Result<(), GraphError> {
        self.dirty.remove(&handle);
        self.revisions.remove(&handle);
        self.entries
            .remove(&handle)
            .map(|_| ())
            .ok_or_else(invalid_handle)
    }
    pub fn update_pbr(
        &mut self,
        handle: MaterialHandle,
        params: PbrParams,
    ) -> Result<(), GraphError> {
        self.update_resident_pbr(handle, PbrMaterial::new(params)?)
    }
    pub fn update_unlit(
        &mut self,
        handle: MaterialHandle,
        params: UnlitParams,
    ) -> Result<(), GraphError> {
        let slot = self.entries.get_mut(&handle).ok_or_else(invalid_handle)?;
        if !matches!(slot.as_ref(), MaterialInstance::Unlit(_)) {
            return Err(GraphError::invalid_input("expected Unlit material"));
        }
        check_color(params.color)?;
        *slot = Arc::new(MaterialInstance::Unlit(params));
        self.bump_revision(handle);
        Ok(())
    }
    pub fn update_custom(
        &mut self,
        handle: MaterialHandle,
        values: &[(&str, Value)],
    ) -> Result<(), GraphError> {
        let slot = self.entries.get_mut(&handle).ok_or_else(invalid_handle)?;
        let MaterialInstance::Custom(old) = slot.as_ref() else {
            return Err(GraphError::invalid_input("expected custom material"));
        };
        let mut next = old.clone();
        next.apply(values)?;
        *slot = Arc::new(MaterialInstance::Custom(next));
        self.bump_revision(handle);
        Ok(())
    }
}
fn check_color(color: [f32; 4]) -> Result<(), GraphError> {
    if color.iter().all(|x| x.is_finite()) {
        Ok(())
    } else {
        Err(GraphError::invalid_input("non-finite material color"))
    }
}
fn invalid_handle() -> GraphError {
    GraphError::invalid_input("material handle was removed or belongs to another registry")
}

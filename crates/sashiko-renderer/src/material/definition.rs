use super::{AlphaMode, SamplerDescriptor, texture_view};
use sashiko_renderer_core::{
    ImageFormat,
    graph::{
        Binding, BindingResource, Graph, GraphError, ImportBuffer, ImportImage, RasterBindings,
        RasterConstants, SampleCount,
    },
    program::{BindingType, BlendState, MAX_RASTER_BINDINGS, ManagedRasterProgram},
};
use std::{
    hash::{Hash, Hasher},
    sync::Arc,
};

/// Defaults also specify the type accepted by named overrides.
#[derive(Clone, Debug)]
pub enum Value {
    Float(f32),
    Vec2([f32; 2]),
    Vec3([f32; 3]),
    Uint(u32),
    Color([f32; 4]),
    Uniform(ImportBuffer),
    StorageRead(ImportBuffer),
    Texture(ImportImage),
    Sampler(SamplerDescriptor),
}

#[derive(Clone, Debug)]
pub enum Parameter {
    /// Word offset in the inline constants. The first 16 words hold clip-from-model.
    Constant {
        name: String,
        word: usize,
        default: Value,
    },
    Binding {
        name: String,
        group: u32,
        binding: u32,
        /// None requires a resource at instance creation.
        default: Option<Value>,
    },
}
impl Parameter {
    fn binding_slot(&self) -> Option<(u32, u32)> {
        match self {
            Self::Binding { group, binding, .. } => Some((*group, *binding)),
            Self::Constant { .. } => None,
        }
    }
    pub fn name(&self) -> &str {
        match self {
            Self::Constant { name, .. } | Self::Binding { name, .. } => name,
        }
    }
    pub fn default_value(&self) -> Option<&Value> {
        match self {
            Self::Constant { default, .. } => Some(default),
            Self::Binding { default, .. } => default.as_ref(),
        }
    }
}
#[derive(Clone, Debug)]
pub struct MaterialDefinition(Arc<Layout>);
impl PartialEq for MaterialDefinition {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for MaterialDefinition {}
impl Hash for MaterialDefinition {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state)
    }
}
#[derive(Debug)]
struct Layout {
    forward: ManagedRasterProgram,
    shadow: Option<ManagedRasterProgram>,
    alpha: AlphaMode,
    parameters: Vec<Parameter>,
}
impl MaterialDefinition {
    /// Register a compiled indexed Forward program. The optional shadow variant
    /// uses the same material layout and receives the cascade clip-from-model matrix.
    pub fn new(
        forward: ManagedRasterProgram,
        shadow: Option<ManagedRasterProgram>,
        alpha: AlphaMode,
        parameters: Vec<Parameter>,
    ) -> Result<Self, GraphError> {
        let desc = forward.descriptor();
        let blend = alpha == AlphaMode::Blend;
        if desc.color_outputs != [ImageFormat::Rgba16Float]
            || desc.vertex_stride == 0
            || desc.depth.compare().is_none()
            || desc.depth.write_enabled() == blend
            || (desc.blend == BlendState::Replace) == blend
        {
            return Err(GraphError::invalid_input(
                "material Forward program requires indexed HDR, depth and matching alpha state",
            ));
        }
        if !blend
            && matches!(
                desc.depth,
                sashiko_renderer_core::program::DepthState::Required {
                    require_load: true,
                    ..
                }
            )
        {
            return Err(GraphError::invalid_input(
                "opaque material must allow a cleared depth target",
            ));
        }
        if let Some(shadow) = &shadow {
            let s = shadow.descriptor();
            if !s.color_outputs.is_empty()
                || !s.depth.write_enabled()
                || matches!(
                    s.depth,
                    sashiko_renderer_core::program::DepthState::Required {
                        require_load: true,
                        ..
                    }
                )
                || s.vertex_attributes != desc.vertex_attributes
                || s.vertex_stride != desc.vertex_stride
                || s.parameters.size != desc.parameters.size
                || s.bindings.len() != desc.bindings.len()
                || !s
                    .bindings
                    .iter()
                    .zip(desc.bindings)
                    .all(|(a, b)| a.group == b.group && a.binding == b.binding && a.kind == b.kind)
            {
                return Err(GraphError::invalid_input(
                    "incompatible shadow material layout",
                ));
            }
        }
        let words = desc.parameters.size as usize / 4;
        if words < 16 {
            return Err(GraphError::invalid_input(
                "material program requires clip-from-model constants",
            ));
        }
        let mut used = [false; 32];
        used[..16].fill(true);
        let mut bindings = 0;
        for (index, param) in parameters.iter().enumerate() {
            if param.name().is_empty()
                || parameters[..index].iter().any(|p| p.name() == param.name())
            {
                return Err(GraphError::invalid_input(
                    "duplicate or empty material parameter name",
                ));
            }
            if let Some(value) = param.default_value() {
                validate(value)?;
                check_binding(param, value, &forward, shadow.as_ref())?;
            }
            match param {
                Parameter::Constant { word, default, .. } => {
                    let size = match default {
                        Value::Float(_) | Value::Uint(_) => 1,
                        Value::Vec2(_) => 2,
                        Value::Vec3(_) => 3,
                        Value::Color(_) => 4,
                        _ => {
                            return Err(GraphError::invalid_input("expected a numeric constant"));
                        }
                    };
                    let range = word
                        .checked_add(size)
                        .filter(|end| *end <= words)
                        .and_then(|end| used.get_mut(*word..end))
                        .ok_or_else(|| {
                            GraphError::invalid_input("material constant exceeds program layout")
                        })?;
                    if range.iter().any(|x| *x) {
                        return Err(GraphError::invalid_input("overlapping material constants"));
                    }
                    range.fill(true);
                }
                Parameter::Binding { group, binding, .. } => {
                    let matches_program = desc
                        .bindings
                        .iter()
                        .any(|slot| slot.group == *group && slot.binding == *binding);
                    let already_bound = parameters[..index]
                        .iter()
                        .any(|parameter| parameter.binding_slot() == Some((*group, *binding)));
                    if !matches_program || already_bound {
                        return Err(GraphError::invalid_input(
                            "material binding does not match program layout",
                        ));
                    }
                    bindings += 1;
                }
            }
        }
        if bindings != desc.bindings.len() {
            return Err(GraphError::invalid_input(
                "missing material program binding",
            ));
        }
        Ok(Self(Arc::new(Layout {
            forward,
            shadow,
            alpha,
            parameters,
        })))
    }
    pub fn parameters(&self) -> &[Parameter] {
        &self.0.parameters
    }
    pub fn alpha_mode(&self) -> AlphaMode {
        self.0.alpha
    }
    pub fn forward(&self) -> &ManagedRasterProgram {
        &self.0.forward
    }
    pub fn shadow(&self) -> Option<&ManagedRasterProgram> {
        self.0.shadow.as_ref()
    }
}
fn check_binding(
    param: &Parameter,
    value: &Value,
    forward: &ManagedRasterProgram,
    shadow: Option<&ManagedRasterProgram>,
) -> Result<(), GraphError> {
    if let Parameter::Binding { group, binding, .. } = param {
        let (kind, size) = match value {
            Value::Texture(_) => (BindingType::SampledImage, 0),
            Value::Sampler(_) => (BindingType::Sampler, 0),
            Value::Uniform(b) => (BindingType::Uniform, b.desc().size),
            Value::StorageRead(b) => (BindingType::StorageRead, b.desc().size),
            _ => return Err(GraphError::invalid_input("expected resource binding")),
        };
        for program in std::iter::once(forward).chain(shadow) {
            if !program.bindings().iter().any(|s| {
                s.group == *group && s.binding == *binding && s.kind == kind && size >= s.min_size
            }) {
                return Err(GraphError::custom(format!(
                    "incompatible material binding {}",
                    param.name()
                )));
            }
        }
    }
    Ok(())
}
pub(super) fn check_texture(texture: &ImportImage) -> Result<(), GraphError> {
    let d = texture.desc();
    if d.array_layers != 1 || d.sample_count != SampleCount::X1 {
        return Err(GraphError::invalid_input(
            "material texture must be a single-sample 2D image",
        ));
    }
    Ok(())
}
fn validate(v: &Value) -> Result<(), GraphError> {
    match v {
        Value::Float(f) if !f.is_finite() => {
            Err(GraphError::invalid_input("nonfinite material value"))
        }
        Value::Vec2(v) if !v.iter().all(|n| n.is_finite()) => {
            Err(GraphError::invalid_input("nonfinite material vector"))
        }
        Value::Vec3(v) if !v.iter().all(|n| n.is_finite()) => {
            Err(GraphError::invalid_input("nonfinite material vector"))
        }
        Value::Color(c) => super::check_color(*c),
        Value::Texture(t) => check_texture(t),
        _ => Ok(()),
    }
}
#[derive(Clone, Debug)]
pub struct CustomMaterial {
    definition: MaterialDefinition,
    values: Vec<Value>,
    words: [u32; 32],
}
impl CustomMaterial {
    pub(super) fn new(
        definition: &MaterialDefinition,
        overrides: &[(&str, Value)],
    ) -> Result<Self, GraphError> {
        let mut instance = Self {
            definition: definition.clone(),
            values: definition
                .0
                .parameters
                .iter()
                .map(|p| {
                    p.default_value()
                        .or_else(|| {
                            overrides
                                .iter()
                                .find(|(name, _)| *name == p.name())
                                .map(|(_, value)| value)
                        })
                        .cloned()
                        .ok_or_else(|| {
                            GraphError::custom(format!(
                                "missing required material parameter {}",
                                p.name()
                            ))
                        })
                })
                .collect::<Result<_, _>>()?,
            words: [0; 32],
        };
        instance.apply(overrides)?;
        Ok(instance)
    }
    pub fn definition(&self) -> &MaterialDefinition {
        &self.definition
    }
    pub fn value(&self, name: &str) -> Option<&Value> {
        self.definition
            .0
            .parameters
            .iter()
            .position(|p| p.name() == name)
            .map(|i| &self.values[i])
    }
    pub(super) fn apply(&mut self, overrides: &[(&str, Value)]) -> Result<(), GraphError> {
        for (name, v) in overrides {
            let i = self
                .definition
                .0
                .parameters
                .iter()
                .position(|p| p.name() == *name)
                .ok_or_else(|| GraphError::custom(format!("unknown material parameter {name}")))?;
            if std::mem::discriminant(&self.values[i]) != std::mem::discriminant(v) {
                return Err(GraphError::custom(format!(
                    "wrong type for material parameter {name}"
                )));
            }
            validate(v)?;
            check_binding(
                &self.definition.0.parameters[i],
                v,
                self.definition.forward(),
                self.definition.shadow(),
            )?;
            self.values[i] = v.clone();
        }
        for (param, value) in self.definition.0.parameters.iter().zip(&self.values) {
            if let Parameter::Constant { word, .. } = param {
                match value {
                    Value::Float(f) => self.words[*word] = f.to_bits(),
                    Value::Uint(u) => self.words[*word] = *u,
                    Value::Vec2(v) => {
                        self.words[*word..*word + 2].copy_from_slice(&v.map(f32::to_bits))
                    }
                    Value::Vec3(v) => {
                        self.words[*word..*word + 3].copy_from_slice(&v.map(f32::to_bits))
                    }
                    Value::Color(c) => {
                        self.words[*word..*word + 4].copy_from_slice(&c.map(f32::to_bits))
                    }
                    _ => unreachable!(),
                }
            }
        }
        Ok(())
    }
    pub(crate) fn constants(&self, matrix: [[f32; 4]; 4]) -> Result<RasterConstants, GraphError> {
        let mut words = self.words;
        for (out, x) in words.iter_mut().zip(matrix.into_iter().flatten()) {
            *out = x.to_bits();
        }
        RasterConstants::new(&words[..self.definition.forward().parameter_size() as usize / 4])
    }
    pub(crate) fn bindings(
        &self,
        graph: &mut Graph,
        program: &ManagedRasterProgram,
    ) -> Result<Option<RasterBindings>, GraphError> {
        if program.bindings().is_empty() {
            return Ok(None);
        }
        let mut slots = [Binding {
            group: 0,
            binding: 0,
            resource: BindingResource::Sampler(SamplerDescriptor::NearestClamp),
        }; MAX_RASTER_BINDINGS];
        let mut n = 0;
        for (param, value) in self.definition.0.parameters.iter().zip(&self.values) {
            if let Parameter::Binding { group, binding, .. } = param {
                slots[n] = Binding {
                    group: *group,
                    binding: *binding,
                    resource: match value {
                        Value::Uniform(src) | Value::StorageRead(src) => BindingResource::Buffer {
                            buffer: graph.import_buffer("material buffer", src)?,
                            offset: 0,
                            size: src.desc().size,
                        },
                        Value::Texture(image) => {
                            BindingResource::ImageView(texture_view(graph, image)?)
                        }
                        Value::Sampler(s) => BindingResource::Sampler(*s),
                        _ => unreachable!(),
                    },
                };
                n += 1;
            }
        }
        graph.raster_bindings(program, &slots[..n]).map(Some)
    }
}

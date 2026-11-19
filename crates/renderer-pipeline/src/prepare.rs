use std::collections::{HashMap, HashSet};
use std::error::Error;

use renderer::ImageFormat;
use renderer::capacity::ResourceClass;
use renderer::graph::{BufferDesc, ImageDesc, LoadOp, RasterConstants, SampleCount};
use renderer::program::{
    BindingLayout, BindingType, DispatchContract, DispatchWorkgroups, ManagedProgram,
    ManagedRasterProgram, RasterPipelineRequest,
};

use crate::schema::{
    self, AttachmentSpec, BindingSpec, Config, LoadAction, PassSpec, ResourceSource, ResourceSpec,
};
use crate::{
    BindingSource, FrameScratch, MaterialVariant, Pass, PassKind, PipelineError, PrepareOptions,
    PreparedAttachment, PreparedBinding, PreparedPipeline, Program, RasterDrawKind, RasterPrograms,
    Resource, ResourceDesc, ShaderRequest,
};

type ResolverError = Box<dyn Error + Send + Sync>;

impl PreparedPipeline {
    /// Parse and prepare a version 1 pipeline, resolving each Program name or
    /// template and material type pair once.
    ///
    /// Return the Program for the backend or backends selected by the application.
    /// Import shader assets through the renderer's trusted Program constructors before
    /// returning them from the resolver. Resource extents and capacity apply until
    /// the pipeline is prepared again.
    pub fn from_yaml(
        yaml: &str,
        options: PrepareOptions,
        resolve: impl FnMut(ShaderRequest<'_>) -> Result<Program, ResolverError>,
    ) -> Result<Self, PipelineError> {
        let Config {
            version,
            resources,
            draw_lists,
            passes,
            output,
        } = schema::parse(yaml)?;
        if version != 1 {
            return Err(invalid(format!("unsupported pipeline version {version}")));
        }
        if options.frames_in_flight == 0 {
            return Err(invalid("frames_in_flight must be nonzero"));
        }
        if passes.is_empty() {
            return Err(invalid("a pipeline requires at least one pass"));
        }

        let resources = resources
            .into_iter()
            .map(|spec| prepare_resource(spec, options.frame_extent))
            .collect::<Result<Vec<_>, _>>()?;
        let resource_ids = index_names(
            resources.iter().map(|resource| resource.id.as_str()),
            "resource",
        )?;
        let draw_list_ids =
            index_names(draw_lists.iter().map(|list| list.id.as_str()), "draw list")?;
        for list in &draw_lists {
            if list.view.trim().is_empty() || list.queue.trim().is_empty() {
                return Err(invalid(format!(
                    "draw list {:?} requires a view and queue",
                    list.id
                )));
            }
        }
        let output = *resource_ids
            .get(output.as_str())
            .ok_or_else(|| invalid(format!("unknown output resource {output:?}")))?;
        if !matches!(resources[output].desc, ResourceDesc::Image(_)) {
            return Err(invalid(format!(
                "output {:?} must be an image",
                resources[output].id
            )));
        }

        let mut pass_ids = HashSet::with_capacity(passes.len());
        let mut programs = Programs::default();
        let mut resolve = resolve;
        let mut max_bindings = 0;
        let mut max_colors = 0;
        let passes = passes
            .into_iter()
            .map(|pass| {
                let id = match &pass {
                    PassSpec::Raster { id, .. } | PassSpec::Compute { id, .. } => id,
                };
                if id.trim().is_empty() || !pass_ids.insert(id.clone()) {
                    return Err(invalid(format!(
                        "empty or duplicate pass identifier {id:?}"
                    )));
                }
                let pass = prepare_pass(
                    pass,
                    &resources,
                    &resource_ids,
                    &draw_list_ids,
                    &mut programs,
                    &mut resolve,
                )?;
                max_bindings = max_bindings.max(pass.bindings.len());
                if let PassKind::Raster { colors, .. } = &pass.kind {
                    max_colors = max_colors.max(colors.len());
                }
                Ok(pass)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let scratch = FrameScratch::new(
            resources.len(),
            draw_lists.len(),
            options.max_draws_per_list,
            max_bindings,
            max_colors,
            options.max_materials,
        )?;
        let classes = prepare_classes(&resources, options.frames_in_flight)?;

        Ok(Self {
            resources,
            passes,
            draw_lists,
            output,
            raster: programs.raster,
            compute: programs.compute,
            classes,
            scratch,
        })
    }
}

fn invalid(message: impl Into<String>) -> PipelineError {
    PipelineError::Invalid(message.into())
}

fn pass_error(id: &str, message: impl std::fmt::Display) -> PipelineError {
    invalid(format!("pass {id:?}: {message}"))
}

fn index_names<'a>(
    names: impl Iterator<Item = &'a str>,
    kind: &str,
) -> Result<HashMap<&'a str, usize>, PipelineError> {
    let mut indices = HashMap::new();
    for (index, name) in names.enumerate() {
        if name.trim().is_empty() || indices.insert(name, index).is_some() {
            return Err(invalid(format!(
                "empty or duplicate {kind} identifier {name:?}"
            )));
        }
    }
    Ok(indices)
}

fn prepare_resource(
    spec: ResourceSpec,
    frame_extent: Option<[u32; 2]>,
) -> Result<Resource, PipelineError> {
    let (id, source, desc) = match spec {
        ResourceSpec::Image {
            id,
            source,
            width,
            height,
            extent,
            format,
        } => {
            let [width, height] = match (width, height, extent) {
                (Some(width), Some(height), None) => [width, height],
                (None, None, Some(schema::ImageExtent::Frame)) => frame_extent
                    .ok_or_else(|| invalid(format!("image {id:?} requires a frame_extent")))?,
                _ => {
                    return Err(invalid(format!(
                        "image {id:?} requires either width and height or extent: frame"
                    )));
                }
            };
            let desc = ImageDesc {
                width,
                height,
                format: format.into(),
                mip_levels: 1,
                array_layers: 1,
                sample_count: SampleCount::X1,
            };
            if desc.byte_len().is_none() {
                return Err(invalid(format!("image {id:?} has invalid dimensions")));
            }
            (id, source, ResourceDesc::Image(desc))
        }
        ResourceSpec::Buffer { id, source, bytes } => {
            if bytes == 0 {
                return Err(invalid(format!("buffer {id:?} must have nonzero size")));
            }
            (id, source, ResourceDesc::Buffer(BufferDesc { size: bytes }))
        }
    };
    Ok(Resource {
        id,
        external: source == ResourceSource::External,
        desc,
    })
}

#[derive(Clone, Copy)]
enum ProgramRef {
    Raster(usize),
    Compute(usize),
}

#[derive(Eq, Hash, PartialEq)]
enum ProgramKey {
    Program(String),
    Template {
        template: String,
        material_type: String,
    },
}

#[derive(Default)]
struct Programs {
    by_key: HashMap<ProgramKey, ProgramRef>,
    raster: Vec<RasterPipelineRequest>,
    compute: Vec<ManagedProgram>,
}

impl Programs {
    fn resolve(
        &mut self,
        pass: &str,
        key: ProgramKey,
        resolve: &mut impl FnMut(ShaderRequest<'_>) -> Result<Program, ResolverError>,
    ) -> Result<ProgramRef, PipelineError> {
        if let Some(program) = self.by_key.get(&key) {
            return Ok(*program);
        }
        let request = match &key {
            ProgramKey::Program(name) if name.trim().is_empty() => {
                return Err(pass_error(pass, "shader name must be nonempty"));
            }
            ProgramKey::Program(name) => ShaderRequest::Program(name),
            ProgramKey::Template {
                template,
                material_type,
            } => ShaderRequest::Template {
                template,
                material_type,
            },
        };
        let program = match resolve(request).map_err(|source| PipelineError::Resolve {
            name: match request {
                ShaderRequest::Program(name) => name.to_owned(),
                ShaderRequest::Template {
                    template,
                    material_type,
                } => format!("template {template:?}, material type {material_type:?}"),
            },
            source,
        })? {
            Program::Raster(program) => {
                let index = self.raster.len();
                self.raster.push(program.into());
                ProgramRef::Raster(index)
            }
            Program::Compute(program) => {
                let index = self.compute.len();
                self.compute.push(program);
                ProgramRef::Compute(index)
            }
        };
        self.by_key.insert(key, program);
        Ok(program)
    }
}

fn prepare_pass(
    spec: PassSpec,
    resources: &[Resource],
    resource_ids: &HashMap<&str, usize>,
    draw_list_ids: &HashMap<&str, usize>,
    programs: &mut Programs,
    resolve: &mut impl FnMut(ShaderRequest<'_>) -> Result<Program, ResolverError>,
) -> Result<Pass, PipelineError> {
    match spec {
        PassSpec::Raster {
            id,
            shader,
            shader_template,
            material_types,
            colors,
            depth,
            draw_list,
            constants,
            bindings,
        } => {
            let draw_list = draw_list
                .map(|list| {
                    draw_list_ids
                        .get(list.as_str())
                        .copied()
                        .ok_or_else(|| pass_error(&id, format!("unknown draw list {list:?}")))
                })
                .transpose()?;
            let bindings = prepare_bindings(&id, bindings, resource_ids)?;
            let has_material_bindings = bindings
                .iter()
                .any(|binding| matches!(binding.source, BindingSource::Material(_)));
            if draw_list.is_none() && has_material_bindings {
                return Err(pass_error(
                    &id,
                    "material bindings require an indexed Raster pass",
                ));
            }
            let colors = colors
                .into_iter()
                .map(|attachment| prepare_attachment(&id, attachment, resource_ids))
                .collect::<Result<Vec<_>, _>>()?;
            let depth = depth
                .map(|attachment| prepare_attachment(&id, attachment, resource_ids))
                .transpose()?;
            let program = match (shader, shader_template) {
                (Some(shader), None) => {
                    if !material_types.is_empty() {
                        return Err(pass_error(&id, "material_types requires shader_template"));
                    }
                    let ProgramRef::Raster(index) =
                        programs.resolve(&id, ProgramKey::Program(shader), resolve)?
                    else {
                        return Err(pass_error(
                            &id,
                            "shader is a Compute Program, expected Raster",
                        ));
                    };
                    RasterPrograms::Fixed(index)
                }
                (None, Some(template)) => {
                    if draw_list.is_none() {
                        return Err(pass_error(
                            &id,
                            "shader_template requires an indexed draw list",
                        ));
                    }
                    if template.trim().is_empty() || material_types.is_empty() {
                        return Err(pass_error(
                            &id,
                            "shader_template and material_types must be nonempty",
                        ));
                    }
                    index_names(material_types.iter().map(String::as_str), "material type")
                        .map_err(|error| pass_error(&id, error))?;
                    let variants = material_types
                        .into_iter()
                        .map(|name| {
                            let key = ProgramKey::Template {
                                template: template.clone(),
                                material_type: name.clone(),
                            };
                            let ProgramRef::Raster(index) = programs.resolve(&id, key, resolve)? else {
                                return Err(pass_error(&id, format!("template {template:?} with material type {name:?} resolved to a Compute Program")));
                            };
                            Ok(MaterialVariant { name, program: index })
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    RasterPrograms::Material(variants)
                }
                _ => {
                    return Err(pass_error(
                        &id,
                        "specify exactly one of shader and shader_template",
                    ));
                }
            };
            let uses_materials =
                has_material_bindings || matches!(&program, RasterPrograms::Material(_));
            let validate_program = |label: &str, index: usize| {
                let program = &programs.raster[index].program;
                validate_bindings(
                    label,
                    resources,
                    &bindings,
                    program.bindings(),
                    program.binding_bytes(),
                    None,
                )?;
                validate_attachments(label, resources, program, &colors, depth.as_ref())?;
                if draw_list.is_some() {
                    if program.vertex_stride() == 0 || !constants.is_empty() {
                        return Err(pass_error(
                            label,
                            "indexed Raster requires a vertex stream and draw-list constants",
                        ));
                    }
                } else if program.vertex_stride() != 0
                    || size_of_val(constants.as_slice()) != program.parameter_size() as usize
                {
                    return Err(pass_error(
                        label,
                        "fullscreen Raster requires a generated-vertex program and matching inline constants",
                    ));
                }
                Ok(())
            };
            match &program {
                RasterPrograms::Fixed(index) => validate_program(&id, *index)?,
                RasterPrograms::Material(variants) => {
                    for variant in variants {
                        validate_program(
                            &format!("{id} (material type {:?})", variant.name),
                            variant.program,
                        )?;
                    }
                }
            }
            let draw = match draw_list {
                Some(index) => RasterDrawKind::Indexed(index),
                None => RasterDrawKind::Fullscreen(RasterConstants::new(&constants)?),
            };
            Ok(Pass {
                id,
                bindings,
                kind: PassKind::Raster {
                    program,
                    colors,
                    depth,
                    draw,
                    uses_materials,
                },
            })
        }
        PassSpec::Compute {
            id,
            shader,
            bindings,
            grid,
        } => {
            let ProgramRef::Compute(index) =
                programs.resolve(&id, ProgramKey::Program(shader), resolve)?
            else {
                return Err(pass_error(
                    &id,
                    "shader is a Raster Program, expected Compute",
                ));
            };
            let program = &programs.compute[index];
            let bindings = prepare_bindings(&id, bindings, resource_ids)?;
            if bindings
                .iter()
                .any(|binding| matches!(binding.source, BindingSource::Material(_)))
            {
                return Err(pass_error(
                    &id,
                    "material bindings require an indexed Raster pass",
                ));
            }
            let contract = program.dispatch_contract();
            validate_bindings(
                &id,
                resources,
                &bindings,
                program.bindings(),
                contract.bytes,
                Some((contract, grid)),
            )?;
            Ok(Pass {
                id,
                bindings,
                kind: PassKind::Compute {
                    program: index,
                    grid,
                },
            })
        }
    }
}

fn resource_index(
    pass: &str,
    name: &str,
    resource_ids: &HashMap<&str, usize>,
) -> Result<usize, PipelineError> {
    resource_ids
        .get(name)
        .copied()
        .ok_or_else(|| pass_error(pass, format!("unknown resource {name:?}")))
}

fn prepare_attachment<T>(
    pass: &str,
    spec: AttachmentSpec<T>,
    resource_ids: &HashMap<&str, usize>,
) -> Result<PreparedAttachment<T>, PipelineError> {
    let load = match (spec.load, spec.clear) {
        (LoadAction::Load, None) => LoadOp::Load,
        (LoadAction::Clear, Some(value)) => LoadOp::Clear(value),
        (LoadAction::Discard, None) => LoadOp::Discard,
        _ => {
            return Err(pass_error(
                pass,
                format!(
                    "attachment {:?} requires a clear value exactly when load is clear",
                    spec.image
                ),
            ));
        }
    };
    Ok(PreparedAttachment {
        resource: resource_index(pass, &spec.image, resource_ids)?,
        load,
        store: spec.store.into(),
    })
}

fn validate_attachments(
    pass: &str,
    resources: &[Resource],
    program: &ManagedRasterProgram,
    colors: &[PreparedAttachment<[f32; 4]>],
    depth: Option<&PreparedAttachment<f32>>,
) -> Result<(), PipelineError> {
    if colors.len() != program.color_outputs().len() || (colors.is_empty() && depth.is_none()) {
        return Err(pass_error(
            pass,
            "attachments must match the Program color output count and include at least one target",
        ));
    }
    if program.requires_depth() && depth.is_none() {
        return Err(pass_error(pass, "program requires a depth attachment"));
    }
    let mut extent = None;
    let mut seen = HashSet::new();
    let mut check_image = |index: usize, format| {
        let resource = &resources[index];
        let ResourceDesc::Image(desc) = resource.desc else {
            return Err(pass_error(
                pass,
                format!("attachment {:?} must be an image", resource.id),
            ));
        };
        let shape = [desc.width, desc.height];
        if desc.format != format || extent.is_some_and(|extent| extent != shape) {
            return Err(pass_error(
                pass,
                format!(
                    "attachment {:?} format or extent differs from the Raster targets",
                    resource.id
                ),
            ));
        }
        if !seen.insert(index) {
            return Err(pass_error(
                pass,
                format!("duplicate attachment {:?}", resource.id),
            ));
        }
        extent = Some(shape);
        Ok(())
    };
    for (attachment, &format) in colors.iter().zip(program.color_outputs()) {
        check_image(attachment.resource, format)?;
        if let LoadOp::Clear(value) = attachment.load
            && !value.iter().all(|value| value.is_finite())
        {
            return Err(pass_error(pass, "color clear values must be finite"));
        }
    }
    if let Some(attachment) = depth {
        check_image(attachment.resource, ImageFormat::Depth32Float)?;
        match attachment.load {
            LoadOp::Load => {}
            _ if program.requires_depth_load() => {
                return Err(pass_error(
                    pass,
                    "program requires loading the depth attachment",
                ));
            }
            LoadOp::Clear(value) if !(0.0..=1.0).contains(&value) || value.is_subnormal() => {
                return Err(pass_error(
                    pass,
                    "depth clear must be zero or a normal value within zero and one",
                ));
            }
            _ => {}
        }
    }
    Ok(())
}

fn prepare_bindings(
    pass: &str,
    bindings: Vec<BindingSpec>,
    resource_ids: &HashMap<&str, usize>,
) -> Result<Vec<PreparedBinding>, PipelineError> {
    bindings
        .into_iter()
        .map(|spec| {
            let (group, binding, source) = match spec {
                BindingSpec::Image {
                    group,
                    binding,
                    resource,
                } => (
                    group,
                    binding,
                    BindingSource::Image(resource_index(pass, &resource, resource_ids)?),
                ),
                BindingSpec::Buffer {
                    group,
                    binding,
                    resource,
                    offset,
                    size,
                } => (
                    group,
                    binding,
                    BindingSource::Buffer {
                        resource: resource_index(pass, &resource, resource_ids)?,
                        offset,
                        size,
                    },
                ),
                BindingSpec::Sampler {
                    group,
                    binding,
                    sampler,
                } => (group, binding, BindingSource::Sampler(sampler.into())),
                BindingSpec::Material {
                    group,
                    binding,
                    resource,
                } => {
                    if resource.trim().is_empty() {
                        return Err(pass_error(
                            pass,
                            "material binding resource must be nonempty",
                        ));
                    }
                    (group, binding, BindingSource::Material(resource))
                }
            };
            Ok(PreparedBinding {
                group,
                binding,
                source,
            })
        })
        .collect()
}

fn validate_bindings(
    pass: &str,
    resources: &[Resource],
    bindings: &[PreparedBinding],
    layout: &[BindingLayout],
    bytes: &[u64],
    dispatch: Option<(DispatchContract<'_>, Option<[u32; 2]>)>,
) -> Result<(), PipelineError> {
    if bindings.len() != layout.len() {
        return Err(pass_error(
            pass,
            "bindings must cover the complete Program layout",
        ));
    }
    let mut grid_slots = None;
    let mut matching_storage_extents = false;
    if let Some((contract, grid)) = dispatch {
        match (contract.workgroups, grid) {
            (
                DispatchWorkgroups::BufferGrid2d {
                    group,
                    binding,
                    read_bindings,
                    max_workgroups,
                },
                Some([x, y]),
            ) if x != 0 && y != 0 && x <= max_workgroups[0] && y <= max_workgroups[1] => {
                grid_slots = Some(((group, binding), read_bindings, u64::from(x) * u64::from(y)));
            }
            (DispatchWorkgroups::BufferGrid2d { .. }, _) => {
                return Err(pass_error(
                    pass,
                    "buffer-grid dispatch requires an explicit nonzero grid within the Program limits",
                ));
            }
            (_, Some(_)) => {
                return Err(pass_error(
                    pass,
                    "explicit grid requires a buffer-grid Compute Program",
                ));
            }
            (DispatchWorkgroups::Image2d { .. }, None) => matching_storage_extents = true,
            (DispatchWorkgroups::Fixed(_), None) => {}
        }
    }

    let mut storage_extent = None;
    for (slot, &footprint) in layout.iter().zip(bytes) {
        let key = (slot.group, slot.binding);
        let error = |reason| pass_error(pass, format!("binding ({}, {}): {reason}", key.0, key.1));
        let mut matching = bindings
            .iter()
            .filter(|input| (input.group, input.binding) == key);
        let binding = matching
            .next()
            .ok_or_else(|| error("missing Program slot"))?;
        if matching.next().is_some() {
            return Err(error("duplicate Program slot"));
        }
        match (&binding.source, slot.kind) {
            (
                BindingSource::Buffer {
                    resource,
                    offset,
                    size,
                },
                BindingType::Uniform | BindingType::StorageRead | BindingType::StorageWrite,
            ) => {
                let ResourceDesc::Buffer(desc) = resources[*resource].desc else {
                    return Err(error("resource must be a buffer"));
                };
                let active = grid_slots
                    .filter(|(write_slot, read_slots, _)| {
                        key == *write_slot || read_slots.contains(&key)
                    })
                    .map_or(Some(footprint), |(_, _, groups)| {
                        footprint.checked_mul(groups)
                    });
                if *size == 0
                    || *size < slot.min_size
                    || offset.checked_add(*size).is_none_or(|end| end > desc.size)
                    || active.is_none_or(|bytes| bytes > *size)
                {
                    return Err(error(
                        "buffer range does not cover the Program footprint or exceeds the buffer",
                    ));
                }
            }
            (
                BindingSource::Image(resource),
                BindingType::SampledImage
                | BindingType::StorageImageRead(_)
                | BindingType::StorageImageWrite(_),
            ) => {
                let ResourceDesc::Image(desc) = resources[*resource].desc else {
                    return Err(error("resource must be an image"));
                };
                if let BindingType::StorageImageRead(format)
                | BindingType::StorageImageWrite(format) = slot.kind
                {
                    if desc.format != format
                        || !matches!(format, ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float)
                    {
                        return Err(error(
                            "storage image format differs from the supported Program ABI",
                        ));
                    }
                    let dimensions = [desc.width, desc.height];
                    if matching_storage_extents
                        && storage_extent.is_some_and(|extent| extent != dimensions)
                    {
                        return Err(error(
                            "storage image extents differ within the dispatch domain",
                        ));
                    }
                    storage_extent = Some(dimensions);
                }
            }
            (BindingSource::Sampler(_), BindingType::Sampler) => {}
            // Graph validates the concrete material operand when the frame is built.
            (BindingSource::Material(_), _) => {}
            _ => return Err(error("resource kind differs from the Program slot")),
        }
    }
    Ok(())
}

fn prepare_classes(
    resources: &[Resource],
    frames: usize,
) -> Result<Vec<ResourceClass>, PipelineError> {
    let mut classes: Vec<ResourceClass> = Vec::new();
    for resource in resources.iter().filter(|resource| !resource.external) {
        let existing = classes
            .iter_mut()
            .find_map(|class| match (class, &resource.desc) {
                (ResourceClass::Buffer { desc, count }, ResourceDesc::Buffer(other))
                    if desc == other =>
                {
                    Some(count)
                }
                (ResourceClass::Image { desc, count }, ResourceDesc::Image(other))
                    if desc == other =>
                {
                    Some(count)
                }
                _ => None,
            });
        if let Some(count) = existing {
            *count = count
                .checked_add(frames)
                .ok_or_else(|| invalid("transient working-set count overflow"))?;
        } else {
            classes.push(match resource.desc {
                ResourceDesc::Buffer(desc) => ResourceClass::Buffer {
                    desc,
                    count: frames,
                },
                ResourceDesc::Image(desc) => ResourceClass::Image {
                    desc,
                    count: frames,
                },
            });
        }
    }
    Ok(classes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTIONS: PrepareOptions = PrepareOptions {
        frame_extent: None,
        frames_in_flight: 2,
        max_draws_per_list: 4,
        max_materials: 2,
    };

    const DRAW_PIPELINE: &str = r#"
version: 1
resources:
  - {id: final, kind: image, width: 2, height: 2, format: rgba8_unorm}
draw_lists:
  - {id: scene, view: main, queue: opaque}
passes:
  - id: draw
    kind: raster
    shader: mesh
    draw_list: scene
    colors:
      - {image: final, load: clear, clear: [0, 0, 0, 1]}
output: final
"#;

    fn prepare_draw(yaml: &str) -> Result<PreparedPipeline, PipelineError> {
        PreparedPipeline::from_yaml(yaml, OPTIONS, |_| {
            Ok(Program::Raster(ManagedRasterProgram::mesh()))
        })
    }

    #[test]
    fn image_extent_requires_exactly_one_valid_size_source() {
        for (declaration, frame_extent, reason) in [
            ("extent: frame", None, "requires a frame_extent"),
            ("extent: frame", Some([2, 0]), "invalid dimensions"),
            ("width: 2", None, "requires either"),
            (
                "width: 2, height: 2, extent: frame",
                Some([2, 2]),
                "requires either",
            ),
        ] {
            let yaml = DRAW_PIPELINE.replace("width: 2, height: 2", declaration);
            let result = PreparedPipeline::from_yaml(
                &yaml,
                PrepareOptions {
                    frame_extent,
                    ..OPTIONS
                },
                |_| panic!("invalid extent must be rejected before shader resolution"),
            );
            assert!(
                matches!(result, Err(PipelineError::Invalid(message)) if message.contains(reason))
            );
        }
    }

    const TEMPLATE_SELECTION: &str = "shader_template: mesh\n    material_types: [wall, leaf]";

    #[test]
    fn raster_requires_one_shader_selection_and_valid_template_material_types() {
        for (selection, reason) in [
            ("", "exactly one"),
            ("shader: mesh\n    shader_template: mesh", "exactly one"),
            (
                "shader: mesh\n    material_types: [wall]",
                "requires shader_template",
            ),
            ("shader_template: mesh", "must be nonempty"),
            (
                "shader_template: mesh\n    material_types: [wall, wall]",
                "duplicate material type",
            ),
        ] {
            let yaml = DRAW_PIPELINE.replace("shader: mesh", selection);
            let result = PreparedPipeline::from_yaml(&yaml, OPTIONS, |_| {
                panic!("malformed shader selection must be rejected before resolution");
            });
            assert!(
                matches!(result, Err(PipelineError::Invalid(message)) if message.contains(reason))
            );
        }
        let fullscreen = DRAW_PIPELINE
            .replace("shader: mesh", TEMPLATE_SELECTION)
            .replace("    draw_list: scene\n", "");
        assert!(
            matches!(prepare_draw(&fullscreen), Err(PipelineError::Invalid(message)) if message.contains("requires an indexed draw list"))
        );
    }

    #[test]
    fn template_materials_resolve_once_per_pair_and_validate_every_variant() {
        let yaml = DRAW_PIPELINE
            .replace("shader: mesh", TEMPLATE_SELECTION)
            .replace(
                "output: final",
                r#"  - id: draw_again
    kind: raster
    shader_template: mesh
    material_types: [leaf, wall]
    draw_list: scene
    colors: [{image: final, load: load}]
  - id: direct
    kind: raster
    shader: mesh
    draw_list: scene
    colors: [{image: final, load: load}]
output: final"#,
            );
        let mut requests = Vec::new();
        let pipeline = PreparedPipeline::from_yaml(&yaml, OPTIONS, |request| {
            requests.push(match request {
                ShaderRequest::Program(name) => (name.to_owned(), None),
                ShaderRequest::Template {
                    template,
                    material_type,
                } => (template.to_owned(), Some(material_type.to_owned())),
            });
            Ok(Program::Raster(ManagedRasterProgram::mesh()))
        })
        .unwrap();
        assert_eq!(
            requests
                .iter()
                .map(|(name, material)| (name.as_str(), material.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("mesh", Some("wall")),
                ("mesh", Some("leaf")),
                ("mesh", None)
            ]
        );
        assert_eq!(pipeline.raster.len(), 3);

        let result = PreparedPipeline::from_yaml(&yaml, OPTIONS, |request| {
            let program = match request {
                ShaderRequest::Template {
                    material_type: "leaf",
                    ..
                } => ManagedRasterProgram::mesh_hdr(),
                _ => ManagedRasterProgram::mesh(),
            };
            Ok(Program::Raster(program))
        });
        assert!(
            matches!(result, Err(PipelineError::Invalid(message)) if message.contains("material type") && message.contains("leaf") && message.contains("format or extent"))
        );
    }

    #[test]
    fn validates_declared_material_slots_for_direct_and_template_programs() {
        let yaml = DRAW_PIPELINE.replace(
            "    draw_list: scene\n",
            r#"    draw_list: scene
    bindings:
      - {kind: material, group: 0, binding: 0, resource: transform}
      - {kind: material, group: 1, binding: 0, resource: base_color}
      - {kind: sampler, group: 1, binding: 1, sampler: nearest_clamp}
"#,
        );
        let prepare = |yaml: &str| {
            PreparedPipeline::from_yaml(yaml, OPTIONS, |_| {
                Ok(Program::Raster(ManagedRasterProgram::textured_mesh()))
            })
        };
        assert!(prepare(&yaml).is_ok());
        assert!(prepare(&yaml.replace("shader: mesh", TEMPLATE_SELECTION)).is_ok());
        let duplicate = yaml.replace("group: 1, binding: 1", "group: 1, binding: 0");
        assert!(
            matches!(prepare(&duplicate), Err(PipelineError::Invalid(message)) if message.contains("duplicate Program slot"))
        );
        let fullscreen = yaml.replace("    draw_list: scene\n", "");
        assert!(
            matches!(prepare(&fullscreen), Err(PipelineError::Invalid(message)) if message.contains("material bindings require an indexed Raster pass"))
        );
    }

    #[test]
    fn version_one_requires_unambiguous_yaml() {
        assert!(prepare_draw(DRAW_PIPELINE).is_ok());
        for yaml in [
            DRAW_PIPELINE.replace("version: 1", "version: 1\nversion: 1"),
            DRAW_PIPELINE.replace("version: 1", "version: 1\nextra: true"),
            DRAW_PIPELINE.replace("shader: mesh", "shader: &asset mesh"),
        ] {
            assert!(matches!(prepare_draw(&yaml), Err(PipelineError::Yaml(_))));
        }
        let yaml = DRAW_PIPELINE.replace("version: 1", "version: 2");
        assert!(
            matches!(prepare_draw(&yaml), Err(PipelineError::Invalid(message)) if message.contains("version 2"))
        );
    }

    #[test]
    fn unresolved_connections_and_attachment_abi_are_configuration_errors() {
        for (from, to, expected) in [
            ("output: final", "output: missing", "unknown output"),
            (
                "draw_list: scene",
                "draw_list: missing",
                "unknown draw list",
            ),
            ("image: final", "image: missing", "unknown resource"),
            ("rgba8_unorm", "rgba16_float", "format or extent"),
        ] {
            let yaml = DRAW_PIPELINE.replace(from, to);
            assert!(
                matches!(prepare_draw(&yaml), Err(PipelineError::Invalid(message)) if message.contains(expected))
            );
        }
    }

    #[test]
    fn compute_footprint_is_checked_while_resolving_the_definition() {
        let yaml = r#"
version: 1
resources:
  - {id: final, kind: image, width: 2, height: 2, format: rgba8_unorm}
  - {id: input, kind: buffer, bytes: 16, source: external}
  - {id: destination, kind: buffer, bytes: 16}
passes:
  - id: compute
    kind: compute
    shader: copy_uniform
    bindings:
      - {kind: buffer, group: 0, binding: 0, resource: input, offset: 0, size: 16}
      - {kind: buffer, group: 1, binding: 0, resource: destination, offset: 0, size: 8}
output: final
"#;
        let result = PreparedPipeline::from_yaml(yaml, OPTIONS, |_| {
            Ok(Program::Compute(ManagedProgram::uniform_readback()))
        });
        assert!(
            matches!(result, Err(PipelineError::Invalid(message)) if message.contains("binding (1, 0)") && message.contains("Program footprint"))
        );
    }
}

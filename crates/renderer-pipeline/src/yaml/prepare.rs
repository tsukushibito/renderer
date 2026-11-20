use std::collections::{BTreeMap, HashMap, HashSet};
use std::error::Error;

use renderer::ImageFormat;
use renderer::capacity::ResourceClass;
use renderer::graph::{BufferDesc, ImageDesc, LoadOp, RasterConstants, SampleCount};
use renderer::program::{
    BindingLayout, BindingType, DispatchContract, DispatchWorkgroups, MAX_RASTER_COLOR_ATTACHMENTS,
    ManagedProgram, ManagedRasterProgram, RasterPipelineRequest,
};

use super::assembly::{ParameterType, Parameters, assemble};
use super::frame::RasterScratchLimits;
use super::schema::{
    self, AttachmentSpec, BindingSpec, Config, DrawListConditionSpec, LoadAction, PassSpec,
    RasterBatchSpec, RasterBatchesSpec, ResourceSource, ResourceSpec,
};
use super::{
    BindingSource, DrawListCondition, FrameScratch, MaterialVariant, Pass, PassKind, PipelineError,
    PipelineRequirements, PrepareInputs, PrepareOptions, PreparedAttachment, PreparedAttachments,
    PreparedBinding, PreparedPipeline, PreparedRasterBatch, Program, RasterDrawKind,
    RasterPrograms, Resource, ResourceDesc, ShaderRequest,
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
        Self::from_yaml_fragments(&[yaml], options, PrepareInputs::default(), resolve)
    }

    /// Prepare an ordered set of version 1 YAML fragments as one pipeline.
    ///
    /// Resources, draw lists and passes share one namespace per kind across
    /// the fragments. Declare the output once. All parameters are resolved
    /// during preparation, before the pipeline can build a frame.
    pub fn from_yaml_fragments(
        fragments: &[&str],
        options: PrepareOptions,
        inputs: PrepareInputs<'_>,
        resolve: impl FnMut(ShaderRequest<'_>) -> Result<Program, ResolverError>,
    ) -> Result<Self, PipelineError> {
        Self::prepare(assemble(fragments)?, options, inputs, resolve)
    }

    fn prepare(
        config: Config,
        options: PrepareOptions,
        inputs: PrepareInputs<'_>,
        mut resolve: impl FnMut(ShaderRequest<'_>) -> Result<Program, ResolverError>,
    ) -> Result<Self, PipelineError> {
        let Config {
            resources,
            draw_lists,
            raster_batches,
            passes,
            output,
            ..
        } = config;
        if options.frames_in_flight == 0 {
            return Err(invalid("frames_in_flight must be nonzero"));
        }
        if passes.is_empty() {
            return Err(invalid("a pipeline requires at least one pass"));
        }

        let params = Parameters::new(inputs.values)?;
        let resources = resources
            .into_iter()
            .map(|spec| prepare_resource(spec, options.frame_extent, &params))
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
        let output = output.ok_or_else(|| invalid("pipeline must declare an output"))?;
        let output = *resource_ids
            .get(output.as_str())
            .ok_or_else(|| invalid(format!("unknown output resource {output:?}")))?;
        if !matches!(resources[output].desc, ResourceDesc::Image(_)) {
            return Err(invalid(format!(
                "output {:?} must be an image",
                resources[output].id
            )));
        }

        if raster_batches
            .iter()
            .any(|(name, batches)| name.trim().is_empty() || batches.is_empty())
        {
            return Err(invalid(
                "raster_batches requires nonempty names and batch lists",
            ));
        }
        let declarations = PassDeclarations {
            resources: &resources,
            resource_ids: &resource_ids,
            draw_list_ids: &draw_list_ids,
            raster_batches: &raster_batches,
            params: &params,
        };
        let mut pass_ids = HashSet::with_capacity(passes.len());
        let mut sampled_views = Vec::new();
        let mut programs = Programs::default();
        let mut scratch_limits = RasterScratchLimits::default();
        let passes = passes
            .into_iter()
            .map(|pass| {
                let id = match &pass {
                    PassSpec::Raster { id, .. }
                    | PassSpec::Attachment { id, .. }
                    | PassSpec::Compute { id, .. } => id,
                };
                if id.trim().is_empty() || !pass_ids.insert(id.clone()) {
                    return Err(invalid(format!(
                        "empty or duplicate pass identifier {id:?}"
                    )));
                }
                let pass = prepare_pass(
                    pass,
                    &declarations,
                    &mut sampled_views,
                    &mut programs,
                    &mut resolve,
                )?;
                scratch_limits.include(&pass);
                Ok(pass)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let requirements =
            measure_requirements(&resources, &passes, draw_lists.len(), &programs, options)?;
        let scratch = FrameScratch::new(
            resources.len(),
            sampled_views.len(),
            draw_lists.len(),
            options.max_draws_per_list,
            passes.len(),
            scratch_limits,
            options.max_materials,
        )?;
        let classes = prepare_classes(&resources, options.frames_in_flight)?;

        Ok(Self {
            resources,
            sampled_views,
            passes,
            draw_lists,
            output,
            raster: programs.raster_requests,
            compute: programs.compute,
            classes,
            requirements,
            scratch,
        })
    }
}

fn measure_requirements(
    resources: &[Resource],
    passes: &[Pass],
    draw_lists: usize,
    programs: &Programs,
    options: PrepareOptions,
) -> Result<PipelineRequirements, PipelineError> {
    let mut requirements = PipelineRequirements {
        passes: passes.len(),
        draw_lists,
        ..PipelineRequirements::default()
    };
    for resource in resources.iter().filter(|resource| !resource.external) {
        requirements.resources += 1;
        requirements.name_bytes += resource.id.len();
    }

    let overflow = || invalid("pipeline Graph declaration limits overflow");
    let mut indexed_draws = 0usize;
    for pass in passes {
        requirements.name_bytes += pass.id.len();
        if let Some(attachments) = pass.kind.attachments() {
            requirements.attachments +=
                attachments.colors.len() + usize::from(attachments.depth.is_some());
            let targets = attachments
                .colors
                .iter()
                .flat_map(|color| [Some(color.resource), color.resolve].into_iter().flatten());
            requirements.writes_external_resources |= targets
                .chain(attachments.depth.iter().map(|depth| depth.resource))
                .any(|resource| resources[resource].external);
        }
        match &pass.kind {
            PassKind::Compute { program, .. } => {
                requirements.compute_dispatches += 1;
                requirements.writes_external_resources |=
                    compute_writes_external(&programs.compute[*program], &pass.bindings, resources);
                requirements.bindings = requirements
                    .bindings
                    .checked_add(pass.bindings.len())
                    .ok_or_else(overflow)?;
            }
            PassKind::Raster { batches, .. } => {
                for batch in batches {
                    let (draws, binding_sets, words) = match &batch.draw {
                        RasterDrawKind::Fullscreen(constants) => (1, 1, constants.words().len()),
                        RasterDrawKind::Indexed(_) => {
                            let draw_limit = options.max_draws_per_list;
                            indexed_draws =
                                indexed_draws.checked_add(draw_limit).ok_or_else(overflow)?;
                            let sets = if batch.uses_materials {
                                options.max_materials.min(draw_limit)
                            } else {
                                usize::from(draw_limit > 0)
                            };
                            let words_per_draw = |index: usize| {
                                programs.raster_requests[index].program.parameter_size() as usize
                                    / size_of::<u32>()
                            };
                            let max_words = match &batch.program {
                                RasterPrograms::Fixed(index) => words_per_draw(*index),
                                RasterPrograms::Material(variants) => variants
                                    .iter()
                                    .map(|variant| words_per_draw(variant.program))
                                    .max()
                                    .unwrap_or(0),
                            };
                            let words = draw_limit.checked_mul(max_words).ok_or_else(overflow)?;
                            (draw_limit, sets, words)
                        }
                    };
                    let bindings = binding_sets
                        .checked_mul(batch.bindings.len())
                        .ok_or_else(overflow)?;
                    requirements.bindings = requirements
                        .bindings
                        .checked_add(bindings)
                        .ok_or_else(overflow)?;
                    requirements.draws =
                        requirements.draws.checked_add(draws).ok_or_else(overflow)?;
                    requirements.raster_constant_words = requirements
                        .raster_constant_words
                        .checked_add(words)
                        .ok_or_else(overflow)?;
                }
            }
            PassKind::Attachment(_) => {}
        }
    }
    // A resolve can add a write. An attachment with a resolve uses at most
    // two graph accesses: its attachment access can also represent a load.
    // A binding can read or write one contiguous range. Each indexed draw
    // can add vertex and index reads; attachments can add reads and writes.
    requirements.accesses = indexed_draws
        .checked_add(requirements.attachments)
        .and_then(|count| count.checked_mul(2))
        .and_then(|count| count.checked_add(requirements.bindings))
        .ok_or_else(overflow)?;
    Ok(requirements)
}

fn compute_writes_external(
    program: &ManagedProgram,
    bindings: &[PreparedBinding],
    resources: &[Resource],
) -> bool {
    bindings.iter().any(|binding| {
        let resource = match binding.source {
            BindingSource::Image(resource) | BindingSource::Buffer { resource, .. } => resource,
            _ => return false,
        };
        resources[resource].external
            && program.bindings().iter().any(|slot| {
                (slot.group, slot.binding) == (binding.group, binding.binding)
                    && matches!(
                        slot.kind,
                        BindingType::StorageWrite | BindingType::StorageImageWrite(_)
                    )
            })
    })
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
    params: &Parameters<'_>,
) -> Result<Resource, PipelineError> {
    let (id, source, desc) = match spec {
        ResourceSpec::Image {
            id,
            source,
            width,
            height,
            extent,
            divisor,
            format,
            samples,
        } => {
            if divisor.is_some() && extent.is_none() {
                return Err(invalid(format!(
                    "image {id:?} divisor requires extent: frame"
                )));
            }
            let [width, height] = match (width, height, extent) {
                (Some(width), Some(height), None) => [width, height],
                (None, None, Some(schema::ImageExtent::Frame)) => {
                    let divisor = divisor.unwrap_or(1);
                    if divisor == 0 {
                        return Err(invalid(format!("image {id:?} divisor must be nonzero")));
                    }
                    frame_extent
                        .ok_or_else(|| invalid(format!("image {id:?} requires a frame_extent")))?
                        .map(|dimension| dimension.div_ceil(divisor))
                }
                _ => {
                    return Err(invalid(format!(
                        "image {id:?} requires either width and height or extent: frame"
                    )));
                }
            };
            let sample_count = match samples.unwrap_or(1) {
                1 => SampleCount::X1,
                4 => SampleCount::X4,
                _ => return Err(invalid(format!("image {id:?} samples must be 1 or 4"))),
            };
            let desc = ImageDesc {
                width,
                height,
                format: format.into(),
                mip_levels: 1,
                array_layers: 1,
                sample_count,
            };
            if desc.byte_len().is_none() {
                return Err(invalid(format!("image {id:?} has invalid dimensions")));
            }
            (id, source, ResourceDesc::Image(desc))
        }
        ResourceSpec::Buffer { id, source, bytes } => {
            let bytes = params
                .resolve(bytes)
                .map_err(|error| invalid(format!("buffer {id:?}: {error}")))?;
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
    raster: Vec<ManagedRasterProgram>,
    raster_requests: Vec<RasterPipelineRequest>,
    raster_variants: HashMap<(usize, SampleCount), usize>,
    compute: Vec<ManagedProgram>,
}

impl Programs {
    fn raster_variant(&mut self, index: usize, sample_count: SampleCount) -> usize {
        *self
            .raster_variants
            .entry((index, sample_count))
            .or_insert_with(|| {
                let variant = self.raster_requests.len();
                self.raster_requests.push(RasterPipelineRequest {
                    program: self.raster[index].clone(),
                    sample_count,
                });
                variant
            })
    }

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
                self.raster.push(program);
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

struct PassDeclarations<'a> {
    resources: &'a [Resource],
    resource_ids: &'a HashMap<&'a str, usize>,
    draw_list_ids: &'a HashMap<&'a str, usize>,
    raster_batches: &'a BTreeMap<String, Vec<RasterBatchSpec>>,
    params: &'a Parameters<'a>,
}

fn prepare_pass(
    spec: PassSpec,
    declarations: &PassDeclarations<'_>,
    sampled_views: &mut Vec<String>,
    programs: &mut Programs,
    resolve: &mut impl FnMut(ShaderRequest<'_>) -> Result<Program, ResolverError>,
) -> Result<Pass, PipelineError> {
    let PassDeclarations {
        resources,
        resource_ids,
        draw_list_ids,
        raster_batches,
        params,
    } = declarations;
    match spec {
        PassSpec::Raster {
            id,
            shader,
            shader_template,
            material_types,
            colors,
            depth,
            when,
            batches,
            draw_list,
            constants,
            bindings,
        } => {
            let when = prepare_condition(&id, when, draw_list_ids)?;
            let attachments = prepare_attachments(&id, colors, depth, declarations)?;
            let batches = if let Some(batches) = batches {
                if shader.is_some()
                    || shader_template.is_some()
                    || draw_list.is_some()
                    || !material_types.is_empty()
                    || !constants.is_empty()
                    || !bindings.is_empty()
                {
                    return Err(pass_error(
                        &id,
                        "batches cannot be combined with flat Raster draw fields",
                    ));
                }
                match batches {
                    RasterBatchesSpec::Inline(batches) => batches,
                    RasterBatchesSpec::Named(name) => {
                        raster_batches.get(&name).cloned().ok_or_else(|| {
                            pass_error(&id, format!("unknown raster batch list {name:?}"))
                        })?
                    }
                }
            } else {
                vec![RasterBatchSpec {
                    shader,
                    shader_template,
                    material_types,
                    draw_list,
                    constants,
                    bindings,
                }]
            };
            if batches.is_empty() {
                return Err(pass_error(&id, "Raster pass requires at least one batch"));
            }
            let batches = batches
                .into_iter()
                .map(|batch| {
                    prepare_raster_batch(
                        &id,
                        batch,
                        &attachments,
                        declarations,
                        sampled_views,
                        programs,
                        resolve,
                    )
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Pass {
                id,
                bindings: Vec::new(),
                kind: PassKind::Raster {
                    attachments,
                    batches,
                },
                when,
            })
        }
        PassSpec::Attachment {
            id,
            colors,
            depth,
            when,
        } => {
            let when = prepare_condition(&id, when, draw_list_ids)?;
            let attachments = prepare_attachments(&id, colors, depth, declarations)?;
            Ok(Pass {
                id,
                bindings: Vec::new(),
                kind: PassKind::Attachment(attachments),
                when,
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
            let bindings = prepare_bindings(&id, bindings, resource_ids, sampled_views, params)?;
            let grid = grid
                .map(|grid| params.resolve(grid))
                .transpose()
                .map_err(|error| pass_error(&id, error))?;
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
                when: None,
            })
        }
    }
}

fn prepare_raster_batch(
    id: &str,
    spec: RasterBatchSpec,
    attachments: &PreparedAttachments,
    declarations: &PassDeclarations<'_>,
    sampled_views: &mut Vec<String>,
    programs: &mut Programs,
    resolve: &mut impl FnMut(ShaderRequest<'_>) -> Result<Program, ResolverError>,
) -> Result<PreparedRasterBatch, PipelineError> {
    let RasterBatchSpec {
        shader,
        shader_template,
        material_types,
        draw_list,
        constants,
        bindings,
    } = spec;
    let PassDeclarations {
        resources,
        resource_ids,
        draw_list_ids,
        params,
        ..
    } = declarations;
    let draw_list = draw_list
        .map(|list| {
            draw_list_ids
                .get(list.as_str())
                .copied()
                .ok_or_else(|| pass_error(id, format!("unknown draw list {list:?}")))
        })
        .transpose()?;
    let bindings = prepare_bindings(id, bindings, resource_ids, sampled_views, params)?;
    let has_material_bindings = bindings
        .iter()
        .any(|binding| matches!(binding.source, BindingSource::Material(_)));
    if draw_list.is_none() && has_material_bindings {
        return Err(pass_error(
            id,
            "material bindings require an indexed Raster pass",
        ));
    }
    let program = match (shader, shader_template) {
        (Some(shader), None) => {
            if !material_types.is_empty() {
                return Err(pass_error(id, "material_types requires shader_template"));
            }
            let ProgramRef::Raster(index) =
                programs.resolve(id, ProgramKey::Program(shader), resolve)?
            else {
                return Err(pass_error(
                    id,
                    "shader is a Compute Program, expected Raster",
                ));
            };
            RasterPrograms::Fixed(programs.raster_variant(index, attachments.samples))
        }
        (None, Some(template)) => {
            if draw_list.is_none() {
                return Err(pass_error(
                    id,
                    "shader_template requires an indexed draw list",
                ));
            }
            if template.trim().is_empty() || material_types.is_empty() {
                return Err(pass_error(
                    id,
                    "shader_template and material_types must be nonempty",
                ));
            }
            index_names(material_types.iter().map(String::as_str), "material type")
                .map_err(|error| pass_error(id, error))?;
            let variants = material_types
                .into_iter()
                .map(|name| {
                    let key = ProgramKey::Template {
                        template: template.clone(),
                        material_type: name.clone(),
                    };
                    let ProgramRef::Raster(index) = programs.resolve(id, key, resolve)? else {
                        return Err(pass_error(id, format!("template {template:?} with material type {name:?} resolved to a Compute Program")));
                    };
                    let program = programs.raster_variant(index, attachments.samples);
                    Ok(MaterialVariant { name, program })
                })
                .collect::<Result<Vec<_>, _>>()?;
            RasterPrograms::Material(variants)
        }
        _ => {
            return Err(pass_error(
                id,
                "specify exactly one of shader and shader_template",
            ));
        }
    };
    let uses_materials = has_material_bindings || matches!(&program, RasterPrograms::Material(_));
    let validate_program = |label: &str, index: usize| {
        let program = &programs.raster_requests[index].program;
        validate_bindings(
            label,
            resources,
            &bindings,
            program.bindings(),
            program.binding_bytes(),
            None,
        )?;
        validate_raster_program(label, resources, program, attachments)?;
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
        RasterPrograms::Fixed(index) => validate_program(id, *index)?,
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
    Ok(PreparedRasterBatch {
        program,
        bindings,
        draw,
        uses_materials,
    })
}

fn prepare_condition(
    pass: &str,
    spec: Option<DrawListConditionSpec>,
    draw_list_ids: &HashMap<&str, usize>,
) -> Result<Option<DrawListCondition>, PipelineError> {
    spec.map(|spec| {
        let draw_list = draw_list_ids
            .get(spec.draw_list.as_str())
            .copied()
            .ok_or_else(|| pass_error(pass, format!("unknown draw list {:?}", spec.draw_list)))?;
        Ok(DrawListCondition {
            draw_list,
            empty: spec.empty,
        })
    })
    .transpose()
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

fn prepare_attachment<T: ParameterType>(
    pass: &str,
    spec: AttachmentSpec<T>,
    resource_ids: &HashMap<&str, usize>,
    params: &Parameters<'_>,
) -> Result<PreparedAttachment<T>, PipelineError> {
    let load = match (spec.load, spec.clear) {
        (LoadAction::Load, None) => LoadOp::Load,
        (LoadAction::Clear, Some(value)) => LoadOp::Clear(
            params
                .resolve(value)
                .map_err(|error| pass_error(pass, error))?,
        ),
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
        resolve: spec
            .resolve
            .as_deref()
            .map(|name| resource_index(pass, name, resource_ids))
            .transpose()?,
        load,
        store: spec.store.into(),
    })
}

fn prepare_attachments(
    pass: &str,
    colors: Vec<AttachmentSpec<[f32; 4]>>,
    depth: Option<AttachmentSpec<f32>>,
    declarations: &PassDeclarations<'_>,
) -> Result<PreparedAttachments, PipelineError> {
    if colors.len() > MAX_RASTER_COLOR_ATTACHMENTS || (colors.is_empty() && depth.is_none()) {
        return Err(pass_error(
            pass,
            "attachment count must be nonzero and within the common Raster limit",
        ));
    }
    let colors = colors
        .into_iter()
        .map(|attachment| {
            prepare_attachment(
                pass,
                attachment,
                declarations.resource_ids,
                declarations.params,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let depth = depth
        .map(|attachment| {
            prepare_attachment(
                pass,
                attachment,
                declarations.resource_ids,
                declarations.params,
            )
        })
        .transpose()?;
    let mut shape = None;
    let mut seen = HashSet::new();
    let mut check_image = |index: usize| {
        let resource = &declarations.resources[index];
        let ResourceDesc::Image(desc) = resource.desc else {
            return Err(pass_error(
                pass,
                format!("attachment {:?} must be an image", resource.id),
            ));
        };
        if !seen.insert(index) {
            return Err(pass_error(
                pass,
                format!("duplicate attachment {:?}", resource.id),
            ));
        }
        Ok(desc)
    };
    let mut check_shape = |desc: ImageDesc| {
        let actual = (desc.width, desc.height, desc.sample_count);
        if shape.is_some_and(|shape| shape != actual) {
            return Err(pass_error(
                pass,
                "attachment extents and sample counts must match",
            ));
        }
        shape = Some(actual);
        Ok(())
    };
    for attachment in &colors {
        let desc = check_image(attachment.resource)?;
        if !matches!(
            desc.format,
            ImageFormat::Rgba8Unorm | ImageFormat::Rgba16Float
        ) {
            return Err(pass_error(
                pass,
                "color attachments require rgba8_unorm or rgba16_float",
            ));
        }
        check_shape(desc)?;
        if let LoadOp::Clear(value) = attachment.load
            && !value.iter().all(|value| value.is_finite())
        {
            return Err(pass_error(pass, "color clear values must be finite"));
        }
        if let Some(resolve) = attachment.resolve {
            let destination = check_image(resolve)?;
            if desc.sample_count != SampleCount::X4
                || matches!(attachment.load, LoadOp::Discard)
                || destination != desc.with_sample_count(SampleCount::X1)
            {
                return Err(pass_error(
                    pass,
                    "color resolve requires initialized 4x color and a matching 1x destination",
                ));
            }
        }
    }
    if let Some(depth) = &depth {
        let desc = check_image(depth.resource)?;
        if desc.format != ImageFormat::Depth32Float || depth.resolve.is_some() {
            return Err(pass_error(
                pass,
                "depth attachments require depth32_float and cannot resolve",
            ));
        }
        check_shape(desc)?;
        if let LoadOp::Clear(value) = depth.load
            && (!(0.0..=1.0).contains(&value) || value.is_subnormal())
        {
            return Err(pass_error(
                pass,
                "depth clear must be zero or a normal value within zero and one",
            ));
        }
    }
    let (_, _, samples) = shape.expect("at least one prepared attachment");
    Ok(PreparedAttachments {
        colors,
        depth,
        samples,
    })
}

fn validate_raster_program(
    pass: &str,
    resources: &[Resource],
    program: &ManagedRasterProgram,
    attachments: &PreparedAttachments,
) -> Result<(), PipelineError> {
    if attachments.colors.len() != program.color_outputs().len() {
        return Err(pass_error(
            pass,
            "attachments must match the Program color output count",
        ));
    }
    for (attachment, &format) in attachments.colors.iter().zip(program.color_outputs()) {
        let ResourceDesc::Image(desc) = resources[attachment.resource].desc else {
            unreachable!("checked attachment image")
        };
        if desc.format != format {
            return Err(pass_error(
                pass,
                format!(
                    "attachment {:?} format differs from the Raster targets",
                    resources[attachment.resource].id
                ),
            ));
        }
    }
    if program.requires_depth() && attachments.depth.is_none() {
        return Err(pass_error(pass, "program requires a depth attachment"));
    }
    if program.requires_depth_load()
        && !attachments
            .depth
            .as_ref()
            .is_some_and(|depth| matches!(depth.load, LoadOp::Load))
    {
        return Err(pass_error(
            pass,
            "program requires loading the depth attachment",
        ));
    }
    Ok(())
}

fn prepare_bindings(
    pass: &str,
    bindings: Vec<BindingSpec>,
    resource_ids: &HashMap<&str, usize>,
    sampled_views: &mut Vec<String>,
    params: &Parameters<'_>,
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
                BindingSpec::SampledView {
                    group,
                    binding,
                    resource,
                } => {
                    if resource.trim().is_empty() {
                        return Err(pass_error(pass, "sampled view name must be nonempty"));
                    }
                    let index = sampled_views.iter().position(|view| *view == resource);
                    let index = index.unwrap_or_else(|| {
                        sampled_views.push(resource);
                        sampled_views.len() - 1
                    });
                    (group, binding, BindingSource::SampledView(index))
                }
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
                        size: params
                            .resolve(size)
                            .map_err(|error| pass_error(pass, error))?,
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
                if desc.sample_count != SampleCount::X1 {
                    return Err(error("shader image bindings require a single-sample image"));
                }
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
            (BindingSource::SampledView(_), BindingType::SampledImage)
            | (BindingSource::Sampler(_), BindingType::Sampler) => {}
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
            (
                "extent: frame, divisor: 0",
                Some([2, 2]),
                "divisor must be nonzero",
            ),
            (
                "width: 2, height: 2, divisor: 2",
                None,
                "divisor requires extent: frame",
            ),
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
            matches!(result, Err(PipelineError::Invalid(message)) if message.contains("material type") && message.contains("leaf") && message.contains("format differs"))
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
    fn fragments_resolve_typed_values_and_share_connections() {
        let (declarations, passes) = DRAW_PIPELINE.split_once("passes:").unwrap();
        let declarations = declarations.replace(
            "draw_lists:",
            "  - {id: scratch, kind: buffer, bytes: {param: scratch_size}}\ndraw_lists:",
        );
        let passes = format!("version: 1\npasses:{passes}")
            .replace("clear: [0, 0, 0, 1]", "clear: {param: background}");
        let values = [
            super::super::PrepareParameter {
                name: "scratch_size",
                value: super::super::PrepareValue::Size(16),
            },
            super::super::PrepareParameter {
                name: "background",
                value: super::super::PrepareValue::Color([0.3, 0.6, 0.9, 1.0]),
            },
        ];
        let prepare = |fragments: &[&str], values| {
            PreparedPipeline::from_yaml_fragments(
                fragments,
                OPTIONS,
                PrepareInputs { values },
                |_| Ok(Program::Raster(ManagedRasterProgram::mesh())),
            )
        };
        let fragments = [declarations.as_str(), passes.as_str()];
        prepare(&fragments, &values).unwrap();
        for (fragments, values, expected) in [
            (
                fragments.as_slice(),
                &values[..1],
                "missing preparation parameter \"background\"",
            ),
            (
                &[
                    declarations.as_str(),
                    passes.as_str(),
                    "version: 1\noutput: final",
                ],
                values.as_slice(),
                "exactly one output",
            ),
        ] {
            assert!(
                matches!(prepare(fragments, values), Err(PipelineError::Invalid(message)) if message.contains(expected))
            );
        }
        let mut wrong = values;
        wrong[1].value = super::super::PrepareValue::Grid([1, 1]);
        assert!(
            matches!(prepare(&fragments, &wrong), Err(PipelineError::Invalid(message)) if message.contains("wrong type"))
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
            ("rgba8_unorm", "rgba16_float", "format differs"),
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

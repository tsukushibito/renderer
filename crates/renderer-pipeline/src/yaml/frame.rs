use std::ops::Range;

use renderer::graph::{
    Attachment, Binding, BindingResource, Graph, GraphError, ImageHandle, ImageView,
    RasterAttachments, RasterBindings, RasterDraw, ResourceHandle,
};
use renderer::program::{ManagedProgram, RasterPipelineRequest};

use super::{
    BindingSource, Pass, PassKind, PipelineError, PreparedAttachment, PreparedPipeline,
    RasterDrawKind, RasterPrograms, Resource, ResourceDesc,
};

/// A graph-local image or buffer for one `source: external` declaration.
#[derive(Clone, Copy)]
pub struct FrameResource<'a> {
    pub name: &'a str,
    pub resource: ResourceHandle,
}

/// A graph-local View for a shared `kind: sampled_view` binding.
/// The View retains its supplied mip range.
#[derive(Clone, Copy)]
pub struct FrameSampledView<'a> {
    pub name: &'a str,
    pub view: ImageView,
}

/// Indexed geometry and per-draw constants for a view. Leave `draw.bindings`
/// empty. Pass-shared and material bindings are selected by the YAML.
#[derive(Clone, Copy)]
pub struct DrawCandidate<'a> {
    pub queue: &'a str,
    pub draw: RasterDraw,
    /// Index into [`FrameInputs::materials`] for a pass that uses materials.
    pub material: Option<usize>,
}

/// The application's candidates for one named view, in draw order.
#[derive(Clone, Copy)]
pub struct FrameView<'a> {
    pub name: &'a str,
    pub candidates: &'a [DrawCandidate<'a>],
}

/// A graph-local resource for a named material slot declared in the YAML.
/// An image binding may use a sampled View covering multiple mip levels.
#[derive(Clone, Copy)]
pub struct MaterialBinding<'a> {
    pub name: &'a str,
    pub resource: BindingResource,
}

/// Resources of a single material instance for this frame.
#[derive(Clone)]
pub struct FrameMaterial<'a> {
    /// The name declared in the template pass's `material_types`.
    pub material_type: &'a str,
    /// A range in [`FrameInputs::material_bindings`].
    pub bindings: Range<usize>,
}

/// Borrowed inputs for one Graph checkout. Supply each required name once.
#[derive(Clone, Copy, Default)]
pub struct FrameInputs<'a> {
    pub resources: &'a [FrameResource<'a>],
    pub sampled_views: &'a [FrameSampledView<'a>],
    pub views: &'a [FrameView<'a>],
    pub materials: &'a [FrameMaterial<'a>],
    pub material_bindings: &'a [MaterialBinding<'a>],
}

/// Image handles declared in one successfully built Graph. The handles belong
/// to that Graph; consumers add their own uses before compiling it.
pub struct FrameOutput<'a> {
    primary: ImageHandle,
    resources: &'a [Resource],
    handles: &'a [ResourceHandle],
}

impl FrameOutput<'_> {
    /// The image selected by the YAML `output` field.
    pub fn image(&self) -> ImageHandle {
        self.primary
    }

    /// Look up a graph-local image by its YAML resource name.
    pub fn named_image(&self, name: &str) -> Option<ImageHandle> {
        let index = self
            .resources
            .iter()
            .position(|resource| resource.id == name)?;
        match self.handles[index] {
            ResourceHandle::Image(image) => Some(image),
            ResourceHandle::Buffer(_) => None,
        }
    }
}

#[derive(Clone, Copy)]
struct SelectedDraw {
    draw: RasterDraw,
    material: Option<usize>,
}

#[derive(Clone, Copy)]
struct MaterialSet {
    program: usize,
    bindings: Option<RasterBindings>,
}

pub(super) struct FrameScratch {
    handles: Vec<ResourceHandle>,
    sampled_views: Vec<ImageView>,
    draws: Vec<Vec<SelectedDraw>>,
    max_draws_per_list: usize,
    bindings: Vec<Binding>,
    colors: Vec<Attachment<[f32; 4]>>,
    max_materials: usize,
    material_sets: Vec<Option<MaterialSet>>,
    material_draws: Vec<(usize, RasterDraw)>,
}

impl FrameScratch {
    pub(super) fn new(
        resources: usize,
        sampled_views: usize,
        draw_lists: usize,
        max_draws_per_list: usize,
        max_bindings: usize,
        max_colors: usize,
        max_materials: usize,
    ) -> Result<Self, PipelineError> {
        let mut draws = reserve(draw_lists)?;
        for _ in 0..draw_lists {
            draws.push(reserve(max_draws_per_list)?);
        }
        Ok(Self {
            handles: reserve(resources)?,
            sampled_views: reserve(sampled_views)?,
            draws,
            max_draws_per_list,
            bindings: reserve(max_bindings)?,
            colors: reserve(max_colors)?,
            max_materials,
            material_sets: reserve(max_materials)?,
            material_draws: reserve(if max_materials == 0 {
                0
            } else {
                max_draws_per_list
            })?,
        })
    }

    fn append_pass(
        &mut self,
        graph: &mut Graph,
        pass: &Pass,
        raster: &[RasterPipelineRequest],
        compute: &[ManagedProgram],
        inputs: FrameInputs<'_>,
    ) -> Result<(), PipelineError> {
        self.bindings.clear();
        for binding in &pass.bindings {
            let resource = match binding.source {
                BindingSource::Image(resource) => {
                    BindingResource::ImageView(graph.image_view(self.image(resource), 0, 0)?)
                }
                BindingSource::SampledView(index) => {
                    BindingResource::ImageView(self.sampled_views[index])
                }
                BindingSource::Buffer {
                    resource,
                    offset,
                    size,
                } => {
                    let ResourceHandle::Buffer(buffer) = self.handles[resource] else {
                        unreachable!("prepared buffer declaration")
                    };
                    BindingResource::Buffer {
                        buffer,
                        offset,
                        size,
                    }
                }
                BindingSource::Sampler(sampler) => BindingResource::Sampler(sampler),
                BindingSource::Material(_) => continue,
            };
            self.bindings.push(Binding {
                group: binding.group,
                binding: binding.binding,
                resource,
            });
        }

        match &pass.kind {
            PassKind::Compute { program, grid } => {
                let program = &compute[*program];
                if let Some(grid) = grid {
                    graph.dispatch_buffer_grid(&pass.id, program, &self.bindings, *grid)?;
                } else {
                    graph.dispatch(&pass.id, program, &self.bindings)?;
                }
            }
            PassKind::Raster {
                program,
                colors,
                depth,
                draw,
                uses_materials,
            } => {
                if *uses_materials {
                    let RasterDrawKind::Indexed(list) = draw else {
                        unreachable!("prepared material draws are indexed")
                    };
                    self.select_material_draws(graph, pass, *list, program, raster, inputs)?;
                }
                self.colors.clear();
                for color in colors {
                    let attachment = self.attachment(graph, color)?;
                    self.colors.push(attachment);
                }
                let depth = depth
                    .as_ref()
                    .map(|attachment| self.attachment(graph, attachment))
                    .transpose()?;
                let attachments = RasterAttachments {
                    colors: &self.colors,
                    color_resolves: &[],
                    depth,
                };
                if *uses_materials {
                    graph.raster(&pass.id, attachments, |encoder| {
                        for batch in self.material_draws.chunk_by(|a, b| a.0 == b.0) {
                            let program = &raster[batch[0].0].program;
                            encoder.draw(program, batch.iter().map(|(_, draw)| draw))?;
                        }
                        Ok(())
                    })?;
                    return Ok(());
                }
                let RasterPrograms::Fixed(program) = program else {
                    unreachable!("prepared pass with no material inputs has a fixed Program")
                };
                let program = &raster[*program].program;
                match draw {
                    RasterDrawKind::Fullscreen(constants) => {
                        graph.raster(&pass.id, attachments, |raster| {
                            raster.draw_fullscreen(program, &self.bindings, *constants)
                        })?;
                    }
                    RasterDrawKind::Indexed(list) => {
                        let draws = &self.draws[*list];
                        let bindings = if self.bindings.is_empty() || draws.is_empty() {
                            None
                        } else {
                            Some(graph.raster_bindings(program, &self.bindings)?)
                        };
                        graph.raster(&pass.id, attachments, |raster| {
                            raster.draw(
                                program,
                                draws.iter().map(|selected| RasterDraw {
                                    bindings,
                                    ..selected.draw
                                }),
                            )
                        })?;
                    }
                }
            }
        }
        Ok(())
    }

    fn select_material_draws(
        &mut self,
        graph: &mut Graph,
        pass: &Pass,
        list: usize,
        programs: &RasterPrograms,
        raster: &[RasterPipelineRequest],
        inputs: FrameInputs<'_>,
    ) -> Result<(), PipelineError> {
        self.material_draws.clear();
        self.material_sets.clear();
        self.material_sets.resize(inputs.materials.len(), None);
        let shared_bindings = self.bindings.len();
        for selected in &self.draws[list] {
            let index = selected
                .material
                .filter(|&index| index < inputs.materials.len())
                .ok_or_else(|| {
                    PipelineError::Invalid(format!(
                        "pass '{}': draw has no valid material index",
                        pass.id
                    ))
                })?;
            let set = match self.material_sets[index] {
                Some(set) => set,
                None => {
                    let material = &inputs.materials[index];
                    let program = programs.select(material.material_type).ok_or_else(|| {
                        PipelineError::Invalid(format!(
                            "pass '{}': material type '{}' has no prepared Program",
                            pass.id, material.material_type
                        ))
                    })?;
                    let material_bindings = inputs
                        .material_bindings
                        .get(material.bindings.clone())
                        .ok_or_else(|| {
                            PipelineError::Invalid(format!(
                                "pass '{}', material {index} ('{}'): binding range is outside the frame material bindings",
                                pass.id, material.material_type,
                            ))
                        })?;
                    for binding in &pass.bindings {
                        if let BindingSource::Material(ref name) = binding.source {
                            let resource =
                                named(material_bindings, name, "material binding", |b| b.name)
                                    .map_err(|error| {
                                        PipelineError::Invalid(format!(
                                            "pass '{}', material {index} ('{}'): {error}",
                                            pass.id, material.material_type
                                        ))
                                    })?
                                    .resource;
                            self.bindings.push(Binding {
                                group: binding.group,
                                binding: binding.binding,
                                resource,
                            });
                        }
                    }
                    let bindings = if self.bindings.is_empty() {
                        None
                    } else {
                        Some(graph.raster_bindings(&raster[program].program, &self.bindings)?)
                    };
                    self.bindings.truncate(shared_bindings);
                    let set = MaterialSet { program, bindings };
                    self.material_sets[index] = Some(set);
                    set
                }
            };
            self.material_draws.push((
                set.program,
                RasterDraw {
                    bindings: set.bindings,
                    ..selected.draw
                },
            ));
        }
        Ok(())
    }

    fn attachment<T: Copy>(
        &self,
        graph: &Graph,
        attachment: &PreparedAttachment<T>,
    ) -> Result<Attachment<T>, GraphError> {
        Ok(Attachment {
            view: graph.image_view(self.image(attachment.resource), 0, 0)?,
            load: attachment.load,
            store: attachment.store,
        })
    }

    fn image(&self, resource: usize) -> ImageHandle {
        let ResourceHandle::Image(image) = self.handles[resource] else {
            unreachable!("prepared image declaration")
        };
        image
    }
}

fn reserve<T>(count: usize) -> Result<Vec<T>, PipelineError> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|error| {
        PipelineError::Invalid(format!("cannot reserve pipeline frame storage: {error}"))
    })?;
    Ok(values)
}

impl PreparedPipeline {
    /// Add this pipeline's passes to an existing Graph and return the frame's
    /// declared images. On error the Graph is consumed and its storage can be reused by
    /// the pool. The same prepared pipeline can build a new frame after failure.
    /// The caller chooses how to consume the output before compiling the Graph.
    pub fn build(
        &mut self,
        mut graph: Graph,
        inputs: FrameInputs<'_>,
    ) -> Result<(Graph, FrameOutput<'_>), PipelineError> {
        let scratch = &mut self.scratch;
        if inputs.materials.len() > scratch.max_materials {
            return Err(PipelineError::Invalid(format!(
                "frame has {} materials, exceeding prepared capacity {}",
                inputs.materials.len(),
                scratch.max_materials
            )));
        }
        scratch.handles.clear();
        for resource in &self.resources {
            scratch
                .handles
                .push(bind_resource(&mut graph, resource, inputs.resources)?);
        }
        scratch.sampled_views.clear();
        for name in &self.sampled_views {
            let view = named(inputs.sampled_views, name, "sampled view", |input| {
                input.name
            })?
            .view;
            // A Graph owns the image identity behind each View. Range and
            // sample compatibility at a binding stay with the Graph.
            graph.image_desc(view.image())?;
            scratch.sampled_views.push(view);
        }
        for (spec, draws) in self.draw_lists.iter().zip(&mut scratch.draws) {
            let view = named(inputs.views, &spec.view, "view", |view| view.name)?;
            draws.clear();
            for candidate in view
                .candidates
                .iter()
                .filter(|candidate| candidate.queue == spec.queue)
            {
                if draws.len() == scratch.max_draws_per_list {
                    return Err(PipelineError::Invalid(format!(
                        "draw list '{}' exceeds prepared capacity {}",
                        spec.id, scratch.max_draws_per_list
                    )));
                }
                if candidate.draw.bindings.is_some() {
                    return Err(PipelineError::Invalid(format!(
                        "draw list '{}' received a candidate with pass bindings",
                        spec.id
                    )));
                }
                draws.push(SelectedDraw {
                    draw: candidate.draw,
                    material: candidate.material,
                });
            }
        }
        for pass in &self.passes {
            scratch
                .append_pass(&mut graph, pass, &self.raster, &self.compute, inputs)
                .map_err(|error| match error {
                    PipelineError::Graph(source) => PipelineError::PassGraph {
                        id: pass.id.clone(),
                        source,
                    },
                    other => other,
                })?;
        }
        Ok((
            graph,
            FrameOutput {
                primary: scratch.image(self.output),
                resources: &self.resources,
                handles: &scratch.handles,
            },
        ))
    }
}

fn bind_resource(
    graph: &mut Graph,
    resource: &Resource,
    inputs: &[FrameResource<'_>],
) -> Result<ResourceHandle, PipelineError> {
    if !resource.external {
        return Ok(match resource.desc {
            ResourceDesc::Image(desc) => graph.image(&resource.id, desc)?.into(),
            ResourceDesc::Buffer(desc) => graph.buffer(&resource.id, desc)?.into(),
        });
    }

    let handle = named(inputs, &resource.id, "external resource", |input| {
        input.name
    })?
    .resource;
    let actual = match handle {
        ResourceHandle::Image(image) => ResourceDesc::Image(graph.image_desc(image)?),
        ResourceHandle::Buffer(buffer) => ResourceDesc::Buffer(graph.buffer_desc(buffer)?),
    };
    if actual != resource.desc {
        return Err(PipelineError::Invalid(format!(
            "external resource '{}' does not match its prepared description",
            resource.id
        )));
    }
    Ok(handle)
}

fn named<'a, T>(
    inputs: &'a [T],
    name: &str,
    kind: &str,
    name_of: impl Fn(&T) -> &str,
) -> Result<&'a T, PipelineError> {
    let mut matching = inputs.iter().filter(|input| name_of(input) == name);
    let entry = matching
        .next()
        .ok_or_else(|| PipelineError::Invalid(format!("missing {kind} '{name}'")))?;
    if matching.next().is_some() {
        return Err(PipelineError::Invalid(format!("duplicate {kind} '{name}'")));
    }
    Ok(entry)
}

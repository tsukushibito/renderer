use renderer::graph::{
    Attachment, Binding, BindingResource, Graph, GraphError, ImageHandle, RasterAttachments,
    RasterDraw, ResourceHandle,
};
use renderer::program::{ManagedProgram, RasterPipelineRequest};

use crate::{
    BindingSource, Pass, PassKind, PipelineError, PreparedAttachment, PreparedPipeline,
    RasterDrawKind, Resource, ResourceDesc,
};

/// A graph-local image or buffer for one `source: external` declaration.
#[derive(Clone, Copy)]
pub struct FrameResource<'a> {
    pub name: &'a str,
    pub resource: ResourceHandle,
}

/// Indexed geometry and per-draw constants for a view. Leave `draw.bindings`
/// empty. Each Raster pass supplies its own bindings from the YAML.
#[derive(Clone, Copy)]
pub struct DrawCandidate<'a> {
    pub queue: &'a str,
    pub draw: RasterDraw,
}

/// The application's candidates for one named view, in draw order.
#[derive(Clone, Copy)]
pub struct FrameView<'a> {
    pub name: &'a str,
    pub candidates: &'a [DrawCandidate<'a>],
}

/// Borrowed inputs for one Graph checkout. Supply each required name once.
#[derive(Clone, Copy, Default)]
pub struct FrameInputs<'a> {
    pub resources: &'a [FrameResource<'a>],
    pub views: &'a [FrameView<'a>],
}

pub(super) struct FrameScratch {
    handles: Vec<ResourceHandle>,
    draws: Vec<Vec<RasterDraw>>,
    max_draws_per_list: usize,
    bindings: Vec<Binding>,
    colors: Vec<Attachment<[f32; 4]>>,
}

impl FrameScratch {
    pub(super) fn new(
        resources: usize,
        draw_lists: usize,
        max_draws_per_list: usize,
        max_bindings: usize,
        max_colors: usize,
    ) -> Result<Self, PipelineError> {
        let mut draws = reserve(draw_lists)?;
        for _ in 0..draw_lists {
            draws.push(reserve(max_draws_per_list)?);
        }
        Ok(Self {
            handles: reserve(resources)?,
            draws,
            max_draws_per_list,
            bindings: reserve(max_bindings)?,
            colors: reserve(max_colors)?,
        })
    }

    fn append_pass(
        &mut self,
        graph: &mut Graph,
        pass: &Pass,
        raster: &[RasterPipelineRequest],
        compute: &[ManagedProgram],
    ) -> Result<(), GraphError> {
        self.bindings.clear();
        for binding in &pass.bindings {
            let resource = match binding.source {
                BindingSource::Image(resource) => {
                    BindingResource::ImageView(graph.image_view(self.image(resource), 0, 0)?)
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
            } => {
                let program = &raster[*program].program;
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
                                draws.iter().map(|draw| RasterDraw { bindings, ..*draw }),
                            )
                        })?;
                    }
                }
            }
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
    /// Add this pipeline's passes to an existing Graph and return its output
    /// image. On error the Graph is consumed and its storage can be reused by
    /// the pool. The same prepared pipeline can build a new frame after failure.
    /// The caller chooses how to consume the output before compiling the Graph.
    pub fn build(
        &mut self,
        mut graph: Graph,
        inputs: FrameInputs<'_>,
    ) -> Result<(Graph, ImageHandle), PipelineError> {
        let scratch = &mut self.scratch;
        scratch.handles.clear();
        for resource in &self.resources {
            scratch
                .handles
                .push(bind_resource(&mut graph, resource, inputs.resources)?);
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
                draws.push(candidate.draw);
            }
        }
        for pass in &self.passes {
            scratch
                .append_pass(&mut graph, pass, &self.raster, &self.compute)
                .map_err(|source| PipelineError::PassGraph {
                    id: pass.id.clone(),
                    source,
                })?;
        }
        let output = scratch.image(self.output);
        Ok((graph, output))
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

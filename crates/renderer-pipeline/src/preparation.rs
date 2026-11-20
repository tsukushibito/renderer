//! Aggregate recipe reservations at a configuration change boundary.

use std::num::NonZeroUsize;

use renderer::capacity::{ResourceClass, WorkingSet};
use renderer::graph::{BufferDesc, GraphCapacity, GraphError, ImageDesc};
use renderer::program::{ManagedProgram, RasterPipelineRequest};

/// Upper bounds for Graph declarations in one frame, including declarations
/// later culled by compilation. Sum requirements for parts in the same Graph.
/// Take the maximum for mutually exclusive configurations of a single slot.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct GraphRequirements {
    pub resources: usize,
    pub passes: usize,
    /// Used by callers to budget native Compute descriptor uses.
    pub compute_dispatches: usize,
    pub accesses: usize,
    pub draws: usize,
    pub raster_constant_words: usize,
    pub bindings: usize,
    pub attachments: usize,
    pub name_bytes: usize,
}

impl GraphRequirements {
    pub fn checked_add(self, rhs: Self) -> Result<Self, GraphError> {
        Ok(Self {
            resources: checked_add(self.resources, rhs.resources)?,
            passes: checked_add(self.passes, rhs.passes)?,
            compute_dispatches: checked_add(self.compute_dispatches, rhs.compute_dispatches)?,
            accesses: checked_add(self.accesses, rhs.accesses)?,
            draws: checked_add(self.draws, rhs.draws)?,
            raster_constant_words: checked_add(
                self.raster_constant_words,
                rhs.raster_constant_words,
            )?,
            bindings: checked_add(self.bindings, rhs.bindings)?,
            attachments: checked_add(self.attachments, rhs.attachments)?,
            name_bytes: checked_add(self.name_bytes, rhs.name_bytes)?,
        })
    }
}

/// Collects physical resource and Program reservations and per-slot declaration
/// limits before frame processing. Pass the resulting [`WorkingSet`] to the
/// Renderer and use [`Self::graph_capacity`] to configure the Graph pool.
///
/// Runtime registration of persistent resources, analysis bounds for the
/// composed Graph, and the actual number of in-flight Graphs belong to the
/// caller. Frame building uses the prepared Graph and runtime directly.
pub struct Preparation {
    frames_in_flight: NonZeroUsize,
    resources: Vec<ResourceClass>,
    raster: Vec<RasterPipelineRequest>,
    compute: Vec<ManagedProgram>,
    requirements: GraphRequirements,
}

impl Preparation {
    pub fn new(frames_in_flight: NonZeroUsize) -> Self {
        Self {
            frames_in_flight,
            resources: Vec::new(),
            raster: Vec::new(),
            compute: Vec::new(),
            requirements: GraphRequirements::default(),
        }
    }

    /// Reserve this many buffers per in-flight frame.
    pub fn reserve_buffer(
        &mut self,
        desc: BufferDesc,
        count_per_frame: usize,
    ) -> Result<(), GraphError> {
        let count = self.concurrent_count(count_per_frame)?;
        add_class(&mut self.resources, ResourceClass::Buffer { desc, count })
    }

    /// Reserve this many images per in-flight frame.
    pub fn reserve_image(
        &mut self,
        desc: ImageDesc,
        count_per_frame: usize,
    ) -> Result<(), GraphError> {
        let count = self.concurrent_count(count_per_frame)?;
        add_class(&mut self.resources, ResourceClass::Image { desc, count })
    }

    /// Sum resource reservations that already include concurrency. This also
    /// accepts Program-only sets and reservations for diagnostic work in flight
    /// with normal frames. Raster variants with the same asset and sample count
    /// share a request. The runtime handles Compute Program identities.
    pub fn add_working_set(&mut self, working_set: WorkingSet<'_>) -> Result<(), GraphError> {
        if !working_set.resources.is_empty() {
            let mut resources = self.resources.clone();
            for &class in working_set.resources {
                add_class(&mut resources, class)?;
            }
            self.resources = resources;
        }
        for requested in working_set.raster {
            if !self.raster.iter().any(|current| {
                current.sample_count == requested.sample_count
                    && current.program.same(&requested.program)
            }) {
                self.raster.push(requested.clone());
            }
        }
        self.compute.extend_from_slice(working_set.compute);
        Ok(())
    }

    pub fn add_graph_requirements(
        &mut self,
        requirements: GraphRequirements,
    ) -> Result<(), GraphError> {
        self.requirements = self.requirements.checked_add(requirements)?;
        Ok(())
    }

    pub fn graph_requirements(&self) -> GraphRequirements {
        self.requirements
    }

    /// Raise the caller's minimum declaration limits to cover all registered
    /// parts. Set import, dependency, read/write analysis and upload limits in
    /// `base` for the assembled Graph.
    pub fn graph_capacity(&self, mut base: GraphCapacity) -> Result<GraphCapacity, GraphError> {
        let requirements = self.requirements;
        base.resources = base.resources.max(requirements.resources);
        base.passes = base.passes.max(requirements.passes);
        base.accesses = base.accesses.max(requirements.accesses);
        base.draws = base.draws.max(requirements.draws);
        if let Some(words) = &mut base.raster_constant_words {
            *words = (*words).max(requirements.raster_constant_words);
        } else {
            // GraphCapacity's default is 32 words per reserved draw.
            let default_words = checked_mul(base.draws, 32)?;
            if default_words < requirements.raster_constant_words {
                base.raster_constant_words = Some(requirements.raster_constant_words);
            }
        }
        base.bindings = base.bindings.max(requirements.bindings);
        base.attachments = base.attachments.max(requirements.attachments);
        base.name_bytes = base.name_bytes.max(requirements.name_bytes);
        Ok(base)
    }

    pub fn working_set(&self) -> WorkingSet<'_> {
        WorkingSet {
            resources: &self.resources,
            raster: &self.raster,
            compute: &self.compute,
        }
    }

    /// Add a standard recipe atomically. Each image in the recipe has one
    /// instance per in-flight frame.
    pub(crate) fn add_recipe(
        &mut self,
        requirements: GraphRequirements,
        images: &[ImageDesc],
        raster: &[RasterPipelineRequest],
    ) -> Result<(), GraphError> {
        let total = self.requirements.checked_add(requirements)?;
        let count = self.frames_in_flight.get();
        let resources: Vec<_> = images
            .iter()
            .map(|&desc| ResourceClass::Image { desc, count })
            .collect();
        self.add_working_set(WorkingSet {
            resources: &resources,
            raster,
            compute: &[],
        })?;
        self.requirements = total;
        Ok(())
    }

    fn concurrent_count(&self, count_per_frame: usize) -> Result<usize, GraphError> {
        checked_mul(self.frames_in_flight.get(), count_per_frame)
    }
}

pub(crate) fn checked_mul(left: usize, right: usize) -> Result<usize, GraphError> {
    left.checked_mul(right).ok_or_else(overflow)
}

fn checked_add(left: usize, right: usize) -> Result<usize, GraphError> {
    left.checked_add(right).ok_or_else(overflow)
}

fn overflow() -> GraphError {
    GraphError::invalid_input("pipeline preparation capacity overflow")
}

fn add_class(classes: &mut Vec<ResourceClass>, class: ResourceClass) -> Result<(), GraphError> {
    let existing = match class {
        ResourceClass::Buffer { desc, count } => {
            if count == 0 {
                return Ok(());
            }
            if desc.size == 0 {
                return Err(GraphError::invalid_input(
                    "pipeline buffer reservation requires a nonzero size",
                ));
            }
            classes.iter_mut().find_map(|existing| match existing {
                ResourceClass::Buffer {
                    desc: existing_desc,
                    count: existing_count,
                } if *existing_desc == desc => Some((existing_count, count)),
                _ => None,
            })
        }
        ResourceClass::Image { desc, count } => {
            if count == 0 {
                return Ok(());
            }
            if desc.byte_len().is_none() {
                return Err(GraphError::invalid_input(
                    "invalid pipeline image reservation descriptor",
                ));
            }
            classes.iter_mut().find_map(|existing| match existing {
                ResourceClass::Image {
                    desc: existing_desc,
                    count: existing_count,
                } if *existing_desc == desc => Some((existing_count, count)),
                _ => None,
            })
        }
    };
    if let Some((existing_count, count)) = existing {
        *existing_count = checked_add(*existing_count, count)?;
    } else {
        classes.push(class);
    }
    Ok(())
}

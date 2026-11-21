//! Aggregate reservations at a configuration change boundary.

use std::num::NonZeroUsize;

use super::{ResourceClass, WorkingSet};
use crate::graph::{BufferDesc, GraphCapacity, GraphError, ImageDesc};
use crate::program::{ManagedProgram, RasterPipelineRequest};

mod operations;
pub use operations::RasterPassBudget;

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
    /// Initialized intervals captured by distinct imported resource declarations.
    pub initial_ranges: usize,
    pub name_bytes: usize,
    /// Bytes copied into the Graph-owned upload arena in one frame.
    pub upload_bytes: usize,
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
            initial_ranges: checked_add(self.initial_ranges, rhs.initial_ranges)?,
            name_bytes: checked_add(self.name_bytes, rhs.name_bytes)?,
            upload_bytes: checked_add(self.upload_bytes, rhs.upload_bytes)?,
        })
    }

    fn component_max(self, rhs: Self) -> Self {
        Self {
            resources: self.resources.max(rhs.resources),
            passes: self.passes.max(rhs.passes),
            compute_dispatches: self.compute_dispatches.max(rhs.compute_dispatches),
            accesses: self.accesses.max(rhs.accesses),
            draws: self.draws.max(rhs.draws),
            raster_constant_words: self.raster_constant_words.max(rhs.raster_constant_words),
            bindings: self.bindings.max(rhs.bindings),
            attachments: self.attachments.max(rhs.attachments),
            initial_ranges: self.initial_ranges.max(rhs.initial_ranges),
            name_bytes: self.name_bytes.max(rhs.name_bytes),
            upload_bytes: self.upload_bytes.max(rhs.upload_bytes),
        }
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

    /// Concurrent frame count used to scale per-frame reservations.
    pub fn frames_in_flight(&self) -> NonZeroUsize {
        self.frames_in_flight
    }

    /// Reserve this many buffers per in-flight frame.
    pub fn reserve_buffer(
        &mut self,
        desc: BufferDesc,
        count_per_frame: usize,
    ) -> Result<(), GraphError> {
        let count = self.concurrent_count(count_per_frame)?;
        merge_class(
            &mut self.resources,
            ResourceClass::Buffer { desc, count },
            checked_add,
        )
    }

    /// Reserve this many images per in-flight frame.
    pub fn reserve_image(
        &mut self,
        desc: ImageDesc,
        count_per_frame: usize,
    ) -> Result<(), GraphError> {
        let count = self.concurrent_count(count_per_frame)?;
        merge_class(
            &mut self.resources,
            ResourceClass::Image { desc, count },
            checked_add,
        )
    }

    /// Sum resource reservations that already include concurrency. This also
    /// accepts Program-only sets and reservations for diagnostic work in flight
    /// with normal frames. Raster variants with the same asset and sample count
    /// share a request. Compute requests share their exact Program identity.
    pub fn add_working_set(&mut self, working_set: WorkingSet<'_>) -> Result<(), GraphError> {
        if !working_set.resources.is_empty() {
            let mut resources = self.resources.clone();
            for &class in working_set.resources {
                merge_class(&mut resources, class, checked_add)?;
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
        for requested in working_set.compute {
            if !self
                .compute
                .iter()
                .any(|current| current.identity() == requested.identity())
            {
                self.compute.push(requested.clone());
            }
        }
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
    /// parts. Reserve a reader for each possible access and an initial analysis
    /// segment per resource. Cross-pass dependencies and additional interval
    /// fragmentation depend on the assembled Graph; budget these in `base`.
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
        base.initial_ranges = base.initial_ranges.max(requirements.initial_ranges);
        base.readers = base.readers.max(requirements.accesses);
        base.segments = base.segments.max(requirements.resources);
        base.name_bytes = base.name_bytes.max(requirements.name_bytes);
        base.upload_bytes = base.upload_bytes.max(requirements.upload_bytes);
        Ok(base)
    }

    /// Physical slots across the prepared classes, including frame concurrency.
    pub fn resource_count(&self) -> Result<usize, GraphError> {
        self.resources.iter().try_fold(0, |total, class| {
            let (ResourceClass::Buffer { count, .. } | ResourceClass::Image { count, .. }) = class;
            checked_add(total, *count)
        })
    }

    /// Raise the supplied runtime limits to accommodate the prepared Graph,
    /// frame concurrency, one result per in-flight frame, and all resource classes.
    /// Include additional retained results or external registrations in `base`.
    #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
    pub fn runtime_limits(
        &self,
        mut base: crate::runtime::RuntimeLimits,
    ) -> Result<crate::runtime::RuntimeLimits, GraphError> {
        base.graph = self.graph_capacity(base.graph)?;
        base.frames = base.frames.max(self.frames_in_flight.get());
        base.results = base.results.max(self.frames_in_flight.get());
        base.resource_capacity = base.resource_capacity.max(self.resource_count()?);
        Ok(base)
    }

    pub fn working_set(&self) -> WorkingSet<'_> {
        WorkingSet {
            resources: &self.resources,
            raster: &self.raster,
            compute: &self.compute,
        }
    }

    /// Add declaration bounds and a working set as one operation. Resource class
    /// counts in `working_set` are totals across concurrent frames. On error
    /// neither the declaration bounds nor the working set change.
    pub fn add_requirements(
        &mut self,
        requirements: GraphRequirements,
        working_set: WorkingSet<'_>,
    ) -> Result<(), GraphError> {
        let total = self.requirements.checked_add(requirements)?;
        self.add_working_set(working_set)?;
        self.requirements = total;
        Ok(())
    }

    /// Add the shared reservation for phases whose resource lifetimes do not
    /// overlap. Each alternative must have the same frame concurrency as this
    /// preparation. Take each resource and Graph limit's maximum and prepare
    /// all Programs in use. Register any overlapping retained resources or
    /// concurrent phases separately. An error preserves the previous budget.
    pub fn add_alternatives(&mut self, alternatives: &[&Self]) -> Result<(), GraphError> {
        let mut shared = Self::new(self.frames_in_flight);
        for alternative in alternatives {
            if alternative.frames_in_flight != self.frames_in_flight {
                return Err(GraphError::invalid_input(
                    "alternative preparations require matching frame concurrency",
                ));
            }
            shared.requirements = shared.requirements.component_max(alternative.requirements);
            for &class in &alternative.resources {
                merge_class(&mut shared.resources, class, |left, right| {
                    Ok(left.max(right))
                })?;
            }
            shared.add_working_set(WorkingSet {
                resources: &[],
                raster: &alternative.raster,
                compute: &alternative.compute,
            })?;
        }
        self.add_requirements(shared.requirements, shared.working_set())
    }

    fn concurrent_count(&self, count_per_frame: usize) -> Result<usize, GraphError> {
        checked_mul(self.frames_in_flight.get(), count_per_frame)
    }
}

fn checked_mul(left: usize, right: usize) -> Result<usize, GraphError> {
    left.checked_mul(right).ok_or_else(overflow)
}

fn checked_add(left: usize, right: usize) -> Result<usize, GraphError> {
    left.checked_add(right).ok_or_else(overflow)
}

fn overflow() -> GraphError {
    GraphError::invalid_input("pipeline preparation capacity overflow")
}

fn merge_class(
    classes: &mut Vec<ResourceClass>,
    class: ResourceClass,
    combine: impl FnOnce(usize, usize) -> Result<usize, GraphError>,
) -> Result<(), GraphError> {
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
        *existing_count = combine(*existing_count, count)?;
    } else {
        classes.push(class);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::SampleCount;
    use crate::program::ManagedRasterProgram;

    #[test]
    fn physical_reservations_include_concurrency_and_share_exact_raster_variants() {
        let mut preparation = Preparation::new(NonZeroUsize::new(2).unwrap());
        let color = ImageDesc::rgba16_float(4, 3);
        let globals = [
            ResourceClass::Image {
                desc: color,
                count: 3,
            },
            ResourceClass::Buffer {
                desc: BufferDesc { size: 32 },
                count: 4,
            },
        ];
        let program = ManagedRasterProgram::mesh_hdr();
        let raster = [
            program.clone().into(),
            program.clone().into(),
            RasterPipelineRequest {
                program,
                sample_count: SampleCount::X4,
            },
        ];
        preparation.reserve_image(color, 1).unwrap();
        preparation
            .reserve_buffer(BufferDesc { size: 32 }, 2)
            .unwrap();
        preparation
            .add_working_set(WorkingSet {
                resources: &globals,
                raster: &raster,
                compute: &[],
            })
            .unwrap();

        let set = preparation.working_set();
        assert!(matches!(
            set.resources,
            [ResourceClass::Image { desc, count: 5 }, ResourceClass::Buffer { count: 8, .. }]
                if *desc == color
        ));
        assert_eq!(set.raster.len(), 2);
        assert_eq!(set.raster[0].sample_count, SampleCount::X1);
        assert_eq!(set.raster[1].sample_count, SampleCount::X4);
    }

    #[test]
    fn nonoverlapping_phases_share_capacity_and_keep_all_programs() {
        let frames = NonZeroUsize::new(2).unwrap();
        let color = ImageDesc::rgba16_float(4, 3);
        let other = BufferDesc { size: 32 };
        let mut aggregate = Preparation::new(frames);
        aggregate.reserve_image(color, 1).unwrap();
        let mut first = Preparation::new(frames);
        let mut second = Preparation::new(frames);
        let encoder = [ManagedProgram::indexed_indirect_encoder()];
        let raster = ManagedRasterProgram::mesh_hdr();
        for (variant, count, samples, passes, upload_bytes) in [
            (&mut first, 2, SampleCount::X1, 4, 48),
            (&mut second, 3, SampleCount::X4, 2, 128),
        ] {
            variant.reserve_image(color, count).unwrap();
            variant
                .add_requirements(
                    GraphRequirements {
                        passes,
                        upload_bytes,
                        ..Default::default()
                    },
                    WorkingSet {
                        resources: &[],
                        raster: &[RasterPipelineRequest {
                            program: raster.clone(),
                            sample_count: samples,
                        }],
                        compute: &encoder,
                    },
                )
                .unwrap();
        }
        second.reserve_buffer(other, 1).unwrap();
        aggregate.add_alternatives(&[&first, &second]).unwrap();

        let set = aggregate.working_set();
        assert!(matches!(set.resources, [
            ResourceClass::Image { desc, count: 8 },
            ResourceClass::Buffer { desc: buffer, count: 2 },
        ] if *desc == color && *buffer == other));
        assert_eq!(set.raster.len(), 2);
        assert_eq!(set.compute.len(), 1);
        let expected = aggregate.graph_requirements();
        assert_eq!(expected.passes, 4);
        let graph = aggregate
            .graph_capacity(GraphCapacity {
                upload_bytes: 0,
                ..Default::default()
            })
            .unwrap();
        assert_eq!(graph.upload_bytes, 128);

        let incompatible = Preparation::new(NonZeroUsize::MIN);
        assert!(
            aggregate
                .add_alternatives(&[&first, &incompatible])
                .is_err()
        );
        assert_eq!(aggregate.graph_requirements(), expected);
        assert!(matches!(
            aggregate.working_set().resources[0],
            ResourceClass::Image { count: 8, .. }
        ));
    }

    #[test]
    fn failed_requirement_registration_keeps_resources_programs_and_bounds_available_for_retry() {
        let mut preparation = Preparation::new(NonZeroUsize::MIN);
        let hdr = ImageDesc::rgba16_float(4, 3);
        preparation.reserve_image(hdr, usize::MAX).unwrap();
        let failed = [
            ResourceClass::Buffer {
                desc: BufferDesc { size: 8 },
                count: 1,
            },
            ResourceClass::Image {
                desc: hdr,
                count: 1,
            },
        ];
        let raster = [ManagedRasterProgram::sdr_linear(false).into()];
        let bounds = GraphRequirements {
            passes: 2,
            ..GraphRequirements::default()
        };
        assert!(
            preparation
                .add_requirements(
                    bounds,
                    WorkingSet {
                        resources: &failed,
                        raster: &raster,
                        compute: &[],
                    },
                )
                .is_err()
        );
        assert_eq!(preparation.working_set().resources.len(), 1);

        assert!(preparation.working_set().raster.is_empty());
        assert_eq!(
            preparation.graph_requirements(),
            GraphRequirements::default()
        );

        preparation
            .add_requirements(
                bounds,
                WorkingSet {
                    resources: &failed[..1],
                    raster: &raster,
                    compute: &[],
                },
            )
            .unwrap();
        assert!(matches!(
            preparation.working_set().resources[1],
            ResourceClass::Buffer { count: 1, .. }
        ));
        assert_eq!(preparation.working_set().raster.len(), 1);
        assert_eq!(preparation.graph_requirements(), bounds);

        let unread_program = [ManagedRasterProgram::sdr_srgb(false).into()];
        assert!(
            preparation
                .add_requirements(
                    GraphRequirements {
                        passes: usize::MAX,
                        ..GraphRequirements::default()
                    },
                    WorkingSet {
                        resources: &failed[..1],
                        raster: &unread_program,
                        compute: &[],
                    },
                )
                .is_err()
        );
        assert!(matches!(
            preparation.working_set().resources[1],
            ResourceClass::Buffer { count: 1, .. }
        ));
        assert_eq!(preparation.working_set().raster.len(), 1);
        assert_eq!(preparation.graph_requirements(), bounds);
    }
}

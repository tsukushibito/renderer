//! Preparation-time counterparts of Graph declarations.

use super::{GraphRequirements, Preparation, checked_add, checked_mul};
use crate::{
    capacity::{ResourceClass, WorkingSet},
    graph::{GraphError, ImageDesc, SampleCount},
    program::{
        BindingLayout, BindingType, ManagedProgram, ManagedRasterProgram, RasterPipelineRequest,
    },
};

fn content_accesses(layout: &[BindingLayout]) -> usize {
    layout
        .iter()
        .filter(|slot| slot.kind.descriptor_class() != BindingType::Sampler)
        .count()
}

impl GraphRequirements {
    /// Logical imports and the maximum initialized intervals captured per
    /// import. Resource identity deduplicates repeated imports at recording.
    pub fn imports(name: &str, count: usize, ranges_per_import: usize) -> Result<Self, GraphError> {
        Ok(Self {
            resources: count,
            initial_ranges: checked_mul(count, ranges_per_import)?,
            name_bytes: checked_mul(count, name.len())?,
            ..Self::default()
        })
    }
}

impl Preparation {
    /// Budget the named declaration and physical class of one transient image per frame.
    pub fn reserve_transient_image(
        &mut self,
        name: &str,
        desc: ImageDesc,
    ) -> Result<(), GraphError> {
        self.add_requirements(
            GraphRequirements {
                resources: 1,
                name_bytes: name.len(),
                ..Default::default()
            },
            WorkingSet {
                resources: &[ResourceClass::Image {
                    desc,
                    count: self.frames_in_flight().get(),
                }],
                ..Default::default()
            },
        )
    }

    /// Budget one buffer or image copy between previously declared resources.
    pub fn reserve_copy(&mut self, name: &str) -> Result<(), GraphError> {
        self.add_graph_requirements(GraphRequirements {
            passes: 1,
            accesses: 2,
            name_bytes: name.len(),
            ..Default::default()
        })
    }

    /// Budget one buffer or image readback. Native staging bytes depend on the
    /// backend, region and retention policy and are configured separately.
    pub fn reserve_readback(&mut self, name: &str) -> Result<(), GraphError> {
        self.add_graph_requirements(GraphRequirements {
            passes: 1,
            accesses: 1,
            name_bytes: name.len(),
            ..Default::default()
        })
    }

    /// Budget one dispatch and its native Program from the shader binding layout.
    pub fn reserve_dispatch(
        &mut self,
        name: &str,
        program: &ManagedProgram,
    ) -> Result<(), GraphError> {
        let slots = program.bindings();
        self.add_requirements(
            GraphRequirements {
                passes: 1,
                compute_dispatches: 1,
                bindings: slots.len(),
                accesses: content_accesses(slots),
                name_bytes: name.len(),
                ..Default::default()
            },
            WorkingSet {
                compute: std::slice::from_ref(program),
                ..Default::default()
            },
        )
    }

    /// Reserve one Raster scope with all its possible batches. Inputs such as
    /// meshes and sampled textures are declared by their owner. A failed scope
    /// leaves the existing preparation available for retry.
    pub fn reserve_raster_pass(
        &mut self,
        name: &str,
        samples: SampleCount,
        declare: impl FnOnce(&mut RasterPassBudget) -> Result<(), GraphError>,
    ) -> Result<(), GraphError> {
        let mut pass = RasterPassBudget {
            preparation: Self::new(self.frames_in_flight()),
            samples,
        };
        pass.preparation.add_graph_requirements(GraphRequirements {
            passes: 1,
            name_bytes: name.len(),
            ..Default::default()
        })?;
        declare(&mut pass)?;
        let requirements = pass.preparation.graph_requirements();
        if requirements.attachments == 0 {
            return Err(GraphError::invalid_input(
                "Raster reservation requires an attachment",
            ));
        }
        self.add_requirements(requirements, pass.preparation.working_set())
    }
}

/// Maximum work for one Raster attachment scope. All methods run at preparation.
pub struct RasterPassBudget {
    preparation: Preparation,
    samples: SampleCount,
}

impl RasterPassBudget {
    /// Declare a new color or depth target for this pass.
    pub fn transient_attachment(&mut self, name: &str, desc: ImageDesc) -> Result<(), GraphError> {
        if desc.sample_count != self.samples {
            return Err(GraphError::invalid_input(
                "Raster attachment sample count differs from the pass",
            ));
        }
        self.preparation.reserve_transient_image(name, desc)?;
        self.attachment()
    }

    /// Use one previously declared color or depth target.
    pub fn attachment(&mut self) -> Result<(), GraphError> {
        self.preparation.add_graph_requirements(GraphRequirements {
            attachments: 1,
            accesses: 1,
            ..Default::default()
        })
    }

    /// Resolve one multisampled color into a previously reserved image.
    pub fn resolve(&mut self) -> Result<(), GraphError> {
        if self.samples == SampleCount::X1 {
            return Err(GraphError::invalid_input(
                "Raster color resolve requires multisampling",
            ));
        }
        self.preparation.add_graph_requirements(GraphRequirements {
            accesses: 1,
            ..Default::default()
        })
    }

    /// Record up to `count` indexed draws or generated fullscreen triangles.
    /// Each draw can supply a distinct set of bindings.
    pub fn draws(
        &mut self,
        program: &ManagedRasterProgram,
        count: usize,
    ) -> Result<(), GraphError> {
        self.add_draws(
            program,
            count,
            count,
            if program.is_fullscreen() { 0 } else { 2 },
        )
    }

    /// Budget repeated indexed draws sharing at most `sets` distinct
    /// Graph binding sets in this scope.
    pub fn draws_sharing_bindings(
        &mut self,
        program: &ManagedRasterProgram,
        count: usize,
        sets: usize,
    ) -> Result<(), GraphError> {
        if program.is_fullscreen()
            || sets > count
            || (count != 0 && sets == 0 && !program.bindings().is_empty())
        {
            return Err(GraphError::invalid_input(
                "invalid indexed Raster binding-set bound",
            ));
        }
        self.add_draws(program, count, sets, 2)
    }

    /// Each indirect batch records one representative draw, its vertex/index
    /// accesses and an indirect argument-buffer read.
    pub fn indirect_batches(
        &mut self,
        program: &ManagedRasterProgram,
        count: usize,
    ) -> Result<(), GraphError> {
        if program.is_fullscreen() {
            return Err(GraphError::invalid_input(
                "indirect batch requires an indexed Raster program",
            ));
        }
        self.add_draws(program, count, count, 3)
    }

    fn add_draws(
        &mut self,
        program: &ManagedRasterProgram,
        count: usize,
        sets: usize,
        geometry_accesses: usize,
    ) -> Result<(), GraphError> {
        let slots = program.bindings();
        let request = RasterPipelineRequest {
            program: program.clone(),
            sample_count: self.samples,
        };
        self.preparation.add_requirements(
            GraphRequirements {
                draws: count,
                accesses: checked_add(
                    checked_mul(count, geometry_accesses)?,
                    checked_mul(sets, content_accesses(slots))?,
                )?,
                bindings: checked_mul(sets, slots.len())?,
                raster_constant_words: checked_mul(
                    count,
                    program.parameter_size() as usize / size_of::<u32>(),
                )?,
                ..Default::default()
            },
            WorkingSet {
                raster: std::slice::from_ref(&request),
                ..Default::default()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{
        Attachment, BufferDesc, GraphCapacity, GraphPool, ImportBuffer, ImportRangesPool, LoadOp,
        RasterAttachments, RasterDraw, RasterParameters, ResourceKey, StoreOp,
    };
    use std::num::NonZeroUsize;

    #[test]
    fn external_mesh_draws_compile_using_only_the_reserved_operands() {
        let frames = NonZeroUsize::new(2).unwrap();
        let program = ManagedRasterProgram::mesh();
        let color = ImageDesc::rgba8(4, 4);
        let mut prep = Preparation::new(frames);
        prep.add_graph_requirements(GraphRequirements::imports("mesh", 4, 1).unwrap())
            .unwrap();
        prep.reserve_raster_pass("隰蜀怜愛", SampleCount::X1, |pass| {
            pass.transient_attachment("color", color)?;
            pass.draws(&program, 2)
        })
        .unwrap();
        prep.reserve_readback("capture").unwrap();
        assert_eq!(prep.resource_count().unwrap(), 2);
        let cap = prep
            .graph_capacity(GraphCapacity {
                resources: 0,
                passes: 0,
                accesses: 0,
                draws: 0,
                raster_constant_words: Some(0),
                bindings: 0,
                attachments: 0,
                initial_ranges: 0,
                name_bytes: 0,
                segments: 0,
                readers: 0,
                ..GraphCapacity::default()
            })
            .unwrap();
        let mut graph = GraphPool::new(cap, 1).unwrap().begin().unwrap();
        let mut snapshots = ImportRangesPool::new(1, 1).unwrap();
        let mut import = |index, bytes| {
            let ranges = snapshots
                .capture(std::slice::from_ref(&(0..bytes)))
                .unwrap();
            let source = ImportBuffer::new(
                ResourceKey {
                    owner: 1,
                    index,
                    generation: 1,
                },
                1,
                BufferDesc { size: bytes },
                ranges,
            )
            .unwrap();
            graph.import_buffer("mesh", &source).unwrap()
        };
        let draws = [0, 2].map(|id| RasterDraw {
            secondary_vertices: None,
            vertices: import(id, 84),
            indices: import(id + 1, 12),
            first_index: 0,
            index_count: 3,
            instance_count: 1,
            parameters: RasterParameters::default().into(),
            bindings: None,
        });
        let output = graph.image("color", color).unwrap();
        graph
            .raster(
                "隰蜀怜愛",
                RasterAttachments {
                    colors: &[Attachment {
                        view: graph.image_view(output, 0, 0).unwrap(),
                        load: LoadOp::Clear([0.; 4]),
                        store: StoreOp::Store,
                    }],
                    color_resolves: &[],
                    depth: None,
                },
                |pass| pass.draw(&program, draws),
            )
            .unwrap();
        graph.readback_image("capture", output).unwrap();
        assert_eq!(graph.compile().unwrap().pass_count(), 2);
    }

    #[test]
    fn shader_binding_sets_use_the_abi_and_overflow_is_transactional() {
        let mut prep = Preparation::new(NonZeroUsize::MIN);
        let program = ManagedRasterProgram::textured_mesh_hdr();
        prep.reserve_raster_pass("geometry", SampleCount::X4, |pass| {
            pass.attachment()?;
            pass.resolve()?;
            pass.draws_sharing_bindings(&program, 10, 1)
        })
        .unwrap();
        let requirements = prep.graph_requirements();
        assert_eq!(requirements.bindings, 3);
        // Vertex/index for each draw, one uniform/image pair, one attachment and one resolve.
        assert_eq!(requirements.accesses, 24);
        assert_eq!(requirements.raster_constant_words, 200);
        let invalid = prep.reserve_raster_pass("overflow", SampleCount::X1, |pass| {
            pass.transient_attachment("extra", ImageDesc::rgba8(1, 1))?;
            pass.draws(&ManagedRasterProgram::mesh(), usize::MAX)
        });
        assert!(invalid.is_err());
        assert_eq!(prep.graph_requirements(), requirements);
        assert_eq!(prep.resource_count().unwrap(), 0);
        assert_eq!(prep.working_set().raster.len(), 1);
    }
}

use super::*;

#[derive(Clone, Copy)]
struct GeometryRange {
    vertices: BufferHandle,
    indices: BufferHandle,
    index_end: u32,
}

/// Prepared host storage for one GPU instance count per independent mesh draw.
/// Counts, geometry and draw order may change on every Graph checkout.
pub struct IndexedInstanceBatch {
    ranges: Vec<GeometryRange>,
    templates: Box<[u8]>,
    limit: usize,
}

impl IndexedInstanceBatch {
    pub fn new(limit: usize) -> Result<Self, GraphError> {
        let [templates, _] = Self::required_buffers(limit)?;
        Ok(Self {
            ranges: Vec::with_capacity(limit),
            templates: vec![0; templates.size as usize].into_boxed_slice(),
            limit,
        })
    }

    pub fn clear(&mut self) {
        self.ranges.clear();
    }

    /// Maximum transient template and native command buffers to reserve at preparation.
    pub fn required_buffers(limit: usize) -> Result<[BufferDesc; 2], GraphError> {
        if limit > (INDIRECT_GROUP_SIZE * MAX_INDIRECT_GROUPS) as usize {
            return Err(GraphError::invalid_input(
                "indirect instance batch exceeds dispatch limits",
            ));
        }
        let padded = limit.next_multiple_of(INDIRECT_GROUP_SIZE as usize) as u64;
        Ok(
            [INDIRECT_TEMPLATE_BYTES, INDIRECT_COMMAND_BYTES].map(|stride| BufferDesc {
                size: padded * stride,
            }),
        )
    }

    /// Add a mesh range. The trusted encoder clamps GPU counts to max_instances
    /// and emits zero baseVertex/firstInstance. Geometry stays attached to its command.
    pub fn push(
        &mut self,
        vertices: BufferHandle,
        indices: BufferHandle,
        range: IndexedIndirectTemplate,
        max_instances: u32,
    ) -> Result<(), GraphError> {
        let slot = self.ranges.len();
        if slot >= self.limit {
            return Err(GraphError::invalid_input(
                "indirect instance batch exceeds prepared capacity",
            ));
        }
        let index_end = range
            .first_index
            .checked_add(range.index_count)
            .filter(|_| range.index_count != 0 && range.index_count.is_multiple_of(3))
            .ok_or_else(|| {
                GraphError::invalid_input("invalid triangle range in indirect instance batch")
            })?;
        let offset = slot * INDIRECT_TEMPLATE_BYTES as usize;
        self.templates[offset..offset + INDIRECT_TEMPLATE_BYTES as usize].copy_from_slice(
            [range.first_index, range.index_count, max_instances, 0]
                .map(u32::to_le_bytes)
                .as_flattened(),
        );
        self.ranges.push(GeometryRange {
            vertices,
            indices,
            index_end,
        });
        Ok(())
    }

    /// Encode all draws in one dispatch, consuming one initialized u32 count per
    /// entry rounded up to 64. The Graph owns the uploaded templates after return.
    pub fn encode(
        &mut self,
        graph: &mut Graph,
        name: &str,
        counts: BufferHandle,
    ) -> Result<EncodedIndexedInstances<'_>, GraphError> {
        if self.ranges.is_empty() {
            return Err(GraphError::invalid_input("empty indirect instance batch"));
        }
        let groups = (self.ranges.len() as u32).div_ceil(INDIRECT_GROUP_SIZE);
        let stride = INDIRECT_TEMPLATE_BYTES as usize;
        let len = (groups * INDIRECT_GROUP_SIZE) as usize * stride;
        let capacity = self.templates.len() / stride;
        let templates = &mut self.templates[..len];
        templates[self.ranges.len() * stride..].fill(0);
        let buffer = graph.encode_indirect_buffer(
            name,
            groups,
            capacity as u32,
            counts,
            0,
            |graph, dst| {
                graph.upload_buffer("upload indirect templates", dst, 0, templates)?;
                Ok(())
            },
        )?;
        Ok(EncodedIndexedInstances {
            buffer,
            ranges: &self.ranges,
        })
    }
}

/// Graph-local typed draw arguments. Each yielded command addresses only the
/// indexed range whose geometry and instance limit were used to encode it.
pub struct EncodedIndexedInstances<'a> {
    buffer: BufferHandle,
    ranges: &'a [GeometryRange],
}
impl EncodedIndexedInstances<'_> {
    pub fn commands(&self) -> impl ExactSizeIterator<Item = IndexedIndirectArgs> + '_ {
        self.ranges
            .iter()
            .enumerate()
            .map(|(i, range)| IndexedIndirectArgs {
                secondary_vertices: None,
                buffer: self.buffer,
                vertices: range.vertices,
                indices: range.indices,
                command_offset: i as u64 * INDIRECT_COMMAND_BYTES,
                command_count: 1,
                max_index_end: range.index_end,
            })
    }
}

use super::*;
use crate::passes::pbr::VERTEX_BYTES;
use crate::shader_binaries::packaged_shaders;
use sashiko_renderer_core::graph::{Binding, BindingResource};
use sashiko_renderer_core::program::{
    BindingLayout, BindingType, ComputeProgramDescriptor, DispatchContract, DispatchWorkgroups,
    ManagedProgram, ShaderStages,
};

const THREADS: usize = 64;
const MAX_GROUPS: u32 = 65_535;
const MAX_JOINTS: usize = 128;
const INPUT_BYTES: usize = VERTEX_BYTES + 8 + 16;
pub(super) const PALETTE_BYTES: usize = MAX_JOINTS * 96;
const PALETTE: &str = "skin palette";
const UPLOAD_PALETTE: &str = "upload skin palette";
const DISPATCH: &str = "compute skin vertices";

/// Prepared model-space linear blend skinning for host RenderGraphs.
/// The bind stream is immutable across poses. Upload it once, then pass its
/// imported buffer to each evaluation. Draws use the original mesh indices.
#[derive(Clone)]
pub struct PreparedComputeSkin {
    bind: Arc<[u8]>,
    groups: u32,
    joints: usize,
}

impl MeshData {
    /// Select resident compute skinning for pose instances of this mesh.
    pub fn with_compute_skin(mut self) -> Result<Self, GraphError> {
        if self
            .skin
            .as_ref()
            .is_some_and(|skin| skin.compute.is_some())
        {
            return Ok(self);
        }
        let prepared = self.prepare_compute_skin()?;
        self.geometry = self.geometry.with_storage(Arc::clone(&prepared.bind))?;
        let skin =
            Arc::get_mut(self.skin.as_mut().expect("prepared skin")).expect("owned mesh data");
        skin.compute = Some((prepared, JointBounds::from_skin(skin)));
        skin.vertices = Vec::new();
        Ok(self)
    }

    /// Pack normalized skin weights and PBR vertices for the compute pass.
    /// This shader supports up to 128 joints and 65,535 groups of 64 vertices.
    pub fn prepare_compute_skin(&self) -> Result<PreparedComputeSkin, GraphError> {
        let skin = self
            .skin
            .as_ref()
            .ok_or_else(|| GraphError::invalid_input("mesh has no skin"))?;
        if let Some((prepared, _)) = &skin.compute {
            return Ok(prepared.clone());
        }
        let groups = skin.vertices.len().div_ceil(THREADS);
        if skin.joints > MAX_JOINTS || groups > MAX_GROUPS as usize {
            return Err(GraphError::invalid_input(
                "skin exceeds compute shader capacity",
            ));
        }
        let mut bind = vec![0; groups * THREADS * INPUT_BYTES];
        for (record, (vertex, influence)) in bind
            .as_chunks_mut::<INPUT_BYTES>()
            .0
            .iter_mut()
            .zip(&skin.vertices)
        {
            record[..VERTEX_BYTES].copy_from_slice(&vertex.encode());
            record[48..56].copy_from_slice(influence.joints.map(u16::to_le_bytes).as_flattened());
            record[56..].copy_from_slice(influence.weights.map(f32::to_le_bytes).as_flattened());
        }
        Ok(PreparedComputeSkin {
            bind: bind.into(),
            groups: groups as u32,
            joints: skin.joints,
        })
    }
}

impl PreparedComputeSkin {
    pub fn bind_bytes(&self) -> &[u8] {
        &self.bind
    }

    fn output_desc(&self) -> BufferDesc {
        BufferDesc {
            size: u64::from(self.groups) * (THREADS * VERTEX_BYTES) as u64,
        }
    }

    /// Reserve one palette upload, dispatch, and vertex output per in-flight frame.
    /// The caller reserves the bind stream, its import and the consuming passes.
    pub fn reserve_pose(&self, prep: &mut Preparation) -> Result<(), GraphError> {
        prep.reserve_buffer(self.output_desc(), 1)?;
        prep.reserve_buffer(
            BufferDesc {
                size: PALETTE_BYTES as u64,
            },
            1,
        )?;
        prep.reserve_dispatch(DISPATCH, &program())?;
        prep.add_graph_requirements(GraphRequirements {
            resources: 2,
            passes: 1,
            accesses: 1,
            upload_bytes: PALETTE_BYTES,
            name_bytes: PALETTE.len() + UPLOAD_PALETTE.len() + STREAM.len(),
            ..Default::default()
        })
    }

    /// Write a model-space PBR vertex stream. Reuse the returned handle for
    /// every view and pass that uses this mesh and palette in the same graph.
    /// Supply a bind buffer initialized from this skin's `bind_bytes()`.
    /// Discard the partially built graph if a declaration fails.
    pub fn add_pose(
        &self,
        graph: &mut Graph,
        bind: BufferHandle,
        palette: &[Transform],
    ) -> Result<BufferHandle, GraphError> {
        if palette.len() != self.joints || graph.buffer_desc(bind)?.size < self.bind.len() as u64 {
            return Err(GraphError::invalid_input("incompatible compute skin input"));
        }
        let mut bytes = [0; PALETTE_BYTES];
        for (dest, joint) in bytes.as_chunks_mut::<96>().0.iter_mut().zip(palette) {
            *dest = joint.encode();
        }
        let transforms = graph.buffer(
            PALETTE,
            BufferDesc {
                size: PALETTE_BYTES as u64,
            },
        )?;
        graph.upload_buffer(UPLOAD_PALETTE, transforms, 0, &bytes)?;
        let output = graph.buffer(STREAM, self.output_desc())?;
        let binding = |binding, buffer, size| Binding {
            binding,
            group: 0,
            resource: BindingResource::Buffer {
                buffer,
                offset: 0,
                size,
            },
        };
        graph.dispatch_buffer_grid(
            DISPATCH,
            &program(),
            &[
                binding(0, transforms, PALETTE_BYTES as u64),
                binding(1, bind, self.bind.len() as u64),
                binding(2, output, self.output_desc().size),
            ],
            [self.groups, 1],
        )?;
        Ok(output)
    }
}

const fn layout(binding: u32, kind: BindingType, min_size: usize) -> BindingLayout {
    BindingLayout {
        binding,
        group: 0,
        stages: ShaderStages::COMPUTE,
        kind,
        min_size: min_size as u64,
    }
}

#[allow(unsafe_code)]
fn program() -> ManagedProgram {
    static DESCRIPTOR: ComputeProgramDescriptor = ComputeProgramDescriptor {
        entry: "main",
        workgroup_size: [THREADS as u32, 1, 1],
        bindings: &[
            layout(0, BindingType::Uniform, PALETTE_BYTES),
            layout(1, BindingType::StorageRead, THREADS * INPUT_BYTES),
            layout(2, BindingType::StorageWrite, THREADS * VERTEX_BYTES),
        ],
        dispatch: DispatchContract {
            workgroups: DispatchWorkgroups::BufferGrid2d {
                group: 0,
                binding: 2,
                read_bindings: &[(0, 1)],
                max_workgroups: [MAX_GROUPS, 1],
            },
            bytes: &[
                PALETTE_BYTES as u64,
                (THREADS * INPUT_BYTES) as u64,
                (THREADS * VERTEX_BYTES) as u64,
            ],
        },
        shaders: packaged_shaders!(module: "skin.comp"),
    };
    // SAFETY: Each lane reads its own 72 bytes, writes its complete 48-byte
    // record including padding lanes, and masks every palette index to 7 bits.
    // The grid bound keeps all raw byte offsets within u32.
    unsafe { ManagedProgram::from_trusted_descriptor(&DESCRIPTOR) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shader_limits_apply_only_to_compute_skins() {
        let vertex = PbrVertex::new([0.; 3], [0., 0., 1.], [1., 0., 0., 1.], [0.; 2]).unwrap();
        let mesh = |joint_count| {
            let weights = JointWeights {
                joints: [joint_count as u16 - 1; 4],
                weights: [1.; 4],
            };
            MeshData::skinned_pbr(&[vertex; 3], &[0, 1, 2], joint_count, &[weights; 3]).unwrap()
        };
        assert!(mesh(MAX_JOINTS + 1).prepare_compute_skin().is_err());
        let skin = mesh(MAX_JOINTS).prepare_compute_skin().unwrap();
        assert_eq!(skin.bind_bytes().len(), THREADS * INPUT_BYTES);
        let mut pool = sashiko_renderer_core::graph::GraphPool::new(Default::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let bind = graph.buffer("undersized", BufferDesc { size: 4 }).unwrap();
        assert!(
            skin.add_pose(&mut graph, bind, &[Transform::IDENTITY; MAX_JOINTS])
                .is_err()
        );
        let bind = graph
            .buffer(
                "bind",
                BufferDesc {
                    size: skin.bind_bytes().len() as u64,
                },
            )
            .unwrap();
        assert!(skin.add_pose(&mut graph, bind, &[]).is_err());
    }
}

//! Lower owned engine input into ordinary, GPU-independent Graph operations.

use std::ops::Range;

use super::{FrameSnapshot, MeshData, MeshHandle, SnapshotMesh};
use crate::graph::{
    BufferDesc, BufferHandle, Graph, GraphError, GraphErrorKind, GraphPool, ImageDesc, ImageHandle,
    MeshUpload, PassHandle,
};

/// Responsibility for the final output's runtime registration. This does not
/// imply GPU completion or ownership of the native resource itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameOutputOwnership {
    /// This frame creates the registration; release it after scheduling consumers.
    Frame,
    /// The output is an imported registration retained by its original caller.
    Caller,
}

/// A normal graph, its final linear RGBA8 output and optional readback pass.
/// The caller can inspect or extend it before compilation.
pub struct FrameGraph {
    pub graph: Graph,
    pub output: ImageHandle,
    pub readback: Option<PassHandle>,
    output_ownership: FrameOutputOwnership,
}

impl FrameGraph {
    /// Reports who releases the final output registration after GPU consumers
    /// have been scheduled. Imported outputs remain owned by their caller.
    pub fn output_ownership(&self) -> FrameOutputOwnership {
        self.output_ownership
    }
}

impl MeshData {
    /// Upload one aligned slice of an immutable mesh stream. Only this bridge
    /// establishes mesh provenance; arbitrary public byte uploads cannot do so.
    pub(crate) fn upload_graph_slice(
        &self,
        graph: &mut Graph,
        name: impl AsRef<str>,
        dst: BufferHandle,
        indices: bool,
        source: Range<usize>,
    ) -> Result<PassHandle, GraphError> {
        let (bytes, provenance) = if indices {
            (
                &self.indices,
                MeshUpload::Indices {
                    byte_len: self.indices.len() as u64,
                    maximum: self.index_maximum,
                },
            )
        } else {
            (&self.vertices, MeshUpload::Vertices)
        };
        graph.upload_mesh_slice(name.as_ref(), dst, bytes, source, provenance)
    }
}

impl FrameSnapshot {
    /// Builds a self-contained graph without a device or window. This standalone
    /// path uploads each distinct mesh once per graph. `VulkanEngine` reuses
    /// persistent GPU meshes instead. The extension runs during graph building,
    /// after opaque drawing and before output export, not during GPU recording.
    ///
    /// It receives complete linear RGBA8 color at the View extent. Return a
    /// complete, initialized image of the same extent. Existing Graph checks
    /// validate handles, declared operation purposes and contents normally.
    pub fn build_graph(
        &self,
        pool: &mut GraphPool,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<FrameGraph, GraphError> {
        let mut graph = pool.begin()?;
        let mut meshes = self
            .meshes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        meshes.clear();
        meshes.extend(
            self.draws
                .iter()
                .enumerate()
                .map(|(index, draw)| SnapshotMesh {
                    mesh: draw.mesh,
                    draw: index,
                    buffers: None,
                }),
        );
        meshes.sort_unstable_by_key(|entry| entry.mesh);
        meshes.dedup_by_key(|entry| entry.mesh);
        for entry in meshes.iter_mut() {
            let draw = &self.draws[entry.draw];
            let vertices = graph.buffer(
                "mesh vertices",
                BufferDesc {
                    size: draw.data.vertices.len() as u64,
                },
            )?;
            let indices = graph.buffer(
                "mesh indices",
                BufferDesc {
                    size: draw.data.indices.len() as u64,
                },
            )?;
            draw.data.upload_graph_slice(
                &mut graph,
                "mesh vertex upload",
                vertices,
                false,
                0..draw.data.vertices.len(),
            )?;
            draw.data.upload_graph_slice(
                &mut graph,
                "mesh index upload",
                indices,
                true,
                0..draw.data.indices.len(),
            )?;
            entry.buffers = Some((vertices, indices));
        }
        self.build_graph_with_meshes(
            graph,
            move |handle| {
                meshes
                    .binary_search_by_key(&handle, |entry| entry.mesh)
                    .ok()
                    .and_then(|index| meshes[index].buffers)
            },
            readback,
            extension,
        )
    }

    pub(crate) fn build_graph_with_meshes(
        &self,
        mut graph: Graph,
        mut resolve: impl FnMut(MeshHandle) -> Option<(BufferHandle, BufferHandle)>,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<FrameGraph, GraphError> {
        let desc = ImageDesc::rgba8(self.view.width, self.view.height);
        let color = graph.image("view color", desc)?;
        let depth = graph.image("view depth", ImageDesc::depth32(desc.width, desc.height))?;
        // The fixed opaque list shares one clear and rendering scope. An empty
        // list retains the clear-only path without requiring a Raster program.
        let draws = self.draws.iter().map(|draw| {
            let (vertices, indices) = resolve(draw.mesh).ok_or_else(|| {
                GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "missing resolved mesh buffers",
                )
            })?;
            Ok((vertices, indices, draw.data.index_count, draw.parameters))
        });
        graph.raster_batch(
            if self.draws.is_empty() {
                "view clear"
            } else {
                "view opaque batch"
            },
            draws,
            color,
            depth,
            self.view.clear_color,
            1.0,
        )?;
        // Mesh scratch belongs only to construction. Release it before running
        // user code, which may independently build another graph from this input.
        drop(resolve);
        let output = extension(&mut graph, color)?;
        let output_desc = graph
            .image_desc(output)
            .map_err(|error| error.with_pass_name("view output"))?;
        if output_desc != desc {
            return Err(GraphError::new(
                GraphErrorKind::InvalidInput,
                format!("frame extension output must match View RGBA8 description {desc:?}; got {output_desc:?}"),
            )
            .with_pass_name("view output")
            .with_resource(output.into(), Some(graph.resource_name(output.index))));
        }
        let output_ownership = if graph.resources[output.index].import.is_some() {
            FrameOutputOwnership::Caller
        } else {
            FrameOutputOwnership::Frame
        };
        graph.export_image(output)?;
        let readback = if readback {
            Some(graph.readback_image("view readback", output)?)
        } else {
            None
        };
        Ok(FrameGraph {
            graph,
            output,
            readback,
            output_ownership,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::engine::{
        Draw, Engine, EngineCapacity, IDENTITY, Material, MeshData, SnapshotPool, Vertex, View,
    };
    use crate::graph::{
        GraphCapacity, GraphErrorKind, GraphPool, ImageDesc, ImageHandle, ImportImage, ResourceKey,
    };

    use super::FrameOutputOwnership;

    fn graph_pool() -> GraphPool {
        GraphPool::new(
            GraphCapacity {
                resources: 512,
                passes: 512,
                ..GraphCapacity::default()
            },
            2,
        )
        .unwrap()
    }

    fn triangle() -> MeshData {
        MeshData::new(
            &[[-0.5, -0.5, 0.25], [0.5, -0.5, 0.25], [0.0, 0.5, 0.25]].map(|position| Vertex {
                position,
                color: [1.0; 4],
            }),
            &[0, 1, 2],
        )
        .unwrap()
    }

    fn view() -> View {
        View {
            width: 16,
            height: 16,
            clip_from_world: IDENTITY,
            clear_color: [0.0, 0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn immutable_mesh_upload_preserves_stream_provenance_and_failed_append_atomicity() {
        use crate::graph::{MeshUpload, Operation, UploadSource};
        use std::sync::Arc;

        let data = triangle();
        let mut graph_pool = graph_pool();
        let mut graph = graph_pool.begin().unwrap();
        let indices = graph
            .buffer(
                "indices",
                crate::graph::BufferDesc {
                    size: data.indices.len() as u64,
                },
            )
            .unwrap();
        let uploaded = data
            .upload_graph_slice(&mut graph, "index chunk", indices, true, 4..8)
            .unwrap();
        let Operation::UploadBuffer {
            data: UploadSource::Shared { bytes, source },
            offset,
            mesh,
            ..
        } = &graph.passes[uploaded.index].operation
        else {
            panic!("mesh chunk upload")
        };
        assert!(Arc::ptr_eq(bytes, &data.indices));
        assert_eq!(source, &(4..8));
        assert_eq!(*offset, 4);
        assert!(matches!(
            mesh,
            Some(MeshUpload::Indices {
                byte_len: 12,
                maximum: 2
            })
        ));
        assert_eq!(graph.passes.len(), 1);

        for invalid in [1..5, 12..16] {
            let error = data
                .upload_graph_slice(&mut graph, "invalid chunk", indices, true, invalid)
                .unwrap_err();
            assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
            assert_eq!(error.resource(), Some(indices.into()));
            assert_eq!(graph.passes.len(), 1, "failed mesh upload appends nothing");
        }
        graph
            .upload_buffer("raw bytes", indices, 0, &[0; 4])
            .unwrap();
        assert!(matches!(
            graph.passes.last().unwrap().operation,
            Operation::UploadBuffer { mesh: None, .. }
        ));
    }

    #[test]
    fn opaque_pass_count_is_independent_of_repeated_draw_count() {
        let mut graph_pool = graph_pool();
        let pool = SnapshotPool::new(256, 1).unwrap();
        let mut assets = Engine::new(EngineCapacity {
            meshes: 1,
            materials: 1,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let mesh = assets.create_mesh(triangle()).unwrap();
        let material = assets.create_material(Material::default()).unwrap();
        let draw = Draw {
            mesh,
            material,
            world_from_model: IDENTITY,
        };
        for count in [1, 32, 256] {
            let snapshot = assets.snapshot(&pool, &view(), &vec![draw; count]).unwrap();
            let frame = snapshot
                .build_graph(&mut graph_pool, false, |_, color| Ok(color))
                .unwrap();
            // Two shared mesh uploads and one opaque list; output is the list's color.
            assert_eq!(frame.graph.compile().unwrap().pass_count(), 3);
        }
    }

    #[test]
    fn opaque_pass_keeps_every_distinct_mesh_producer() {
        let mut graph_pool = graph_pool();
        let pool = SnapshotPool::new(256, 1).unwrap();
        const MESHES: usize = 128;
        let mut assets = Engine::new(EngineCapacity {
            meshes: MESHES,
            materials: 1,
            retired_mesh_versions: MESHES,
        })
        .unwrap();
        let material = assets.create_material(Material::default()).unwrap();
        let draws = (0..MESHES)
            .map(|_| Draw {
                mesh: assets.create_mesh(triangle()).unwrap(),
                material,
                world_from_model: IDENTITY,
            })
            .collect::<Vec<_>>();
        let snapshot = assets.snapshot(&pool, &view(), &draws).unwrap();
        let frame = snapshot
            .build_graph(&mut graph_pool, false, |_, color| Ok(color))
            .unwrap();
        let plan = frame.graph.compile().unwrap();
        assert_eq!(plan.pass_count(), MESHES * 2 + 1);
        #[cfg(feature = "graph-diagnostics")]
        assert!(plan.culled_pass_names().next().is_none());
    }

    #[test]
    fn empty_view_has_initialized_output_without_uploads() {
        let mut graph_pool = graph_pool();
        let pool = SnapshotPool::new(256, 1).unwrap();
        let assets = Engine::new(EngineCapacity {
            meshes: 1,
            materials: 1,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let snapshot = assets
            .snapshot(
                &pool,
                &View {
                    width: 4,
                    height: 3,
                    clip_from_world: IDENTITY,
                    clear_color: [0.25, 0.5, 0.75, 1.0],
                },
                &[],
            )
            .unwrap();
        let frame = snapshot
            .build_graph(&mut graph_pool, false, |_, color| Ok(color))
            .unwrap();
        assert_eq!(frame.output_ownership(), FrameOutputOwnership::Frame);
        assert!(frame.readback.is_none());
        let plan = frame.graph.compile().unwrap();
        assert_eq!(
            plan.ordered_pass_names().collect::<Vec<_>>(),
            ["view clear"]
        );
        assert_eq!(plan.pass_count(), 1);
    }

    #[test]
    fn extension_output_checks_owner_format_extent_and_initialization() {
        let mut graph_pool = graph_pool();
        let pool = SnapshotPool::new(256, 1).unwrap();
        let assets = Engine::new(EngineCapacity {
            meshes: 1,
            materials: 1,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let snapshot = assets.snapshot(&pool, &view(), &[]).unwrap();
        let mut foreign_graph = graph_pool.begin().unwrap();
        let foreign = foreign_graph
            .image("foreign", ImageDesc::rgba8(16, 16))
            .unwrap();
        let error = match snapshot.build_graph(&mut graph_pool, false, |_, _| Ok(foreign)) {
            Ok(_) => panic!("foreign output must fail during build"),
            Err(error) => error,
        };
        assert_eq!(error.kind(), GraphErrorKind::WrongOwner);
        assert_eq!(error.pass_name(), Some("view output"));
        assert_eq!(error.resource(), Some(foreign.into()));
        assert_eq!(error.resource_name(), None);

        for (name, desc) in [
            ("wrong format", ImageDesc::depth32(16, 16)),
            ("wrong extent", ImageDesc::rgba8(8, 16)),
        ] {
            let error = match snapshot
                .build_graph(&mut graph_pool, false, |graph, _| graph.image(name, desc))
            {
                Ok(_) => panic!("incompatible output must fail during build"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), GraphErrorKind::InvalidInput);
            assert_eq!(error.pass_name(), Some("view output"));
            assert_eq!(error.pass(), None);
            assert_eq!(error.resource_name(), Some(name));
            assert!(matches!(
                error.resource(),
                Some(crate::graph::ResourceHandle::Image(_))
            ));
        }

        let mut uninitialized = None::<ImageHandle>;
        let frame = snapshot
            .build_graph(&mut graph_pool, false, |graph, color| {
                let output = graph.image("uninitialized output", graph.image_desc(color)?)?;
                uninitialized = Some(output);
                Ok(output)
            })
            .unwrap();
        let error = frame.graph.compile().unwrap_err();
        assert_eq!(error.kind(), GraphErrorKind::UninitializedRead);
        assert_eq!(error.resource(), Some(uninitialized.unwrap().into()));
    }

    #[test]
    fn imported_extension_output_keeps_caller_registration_ownership() {
        let mut graph_pool = graph_pool();
        let pool = SnapshotPool::new(256, 1).unwrap();
        let assets = Engine::new(EngineCapacity {
            meshes: 1,
            materials: 1,
            retired_mesh_versions: 1,
        })
        .unwrap();
        let snapshot = assets.snapshot(&pool, &view(), &[]).unwrap();
        let imported = ImportImage {
            key: ResourceKey {
                owner: 9,
                index: 3,
                generation: 2,
            },
            revision: 4,
            desc: ImageDesc::rgba8(16, 16),
            initialized: true,
        };
        let mut returned = None;
        let frame = snapshot
            .build_graph(&mut graph_pool, false, |graph, _| {
                let handle = graph.import_image("caller image", &imported)?;
                returned = Some(handle);
                Ok(handle)
            })
            .unwrap();
        assert_eq!(frame.output, returned.unwrap());
        assert_eq!(frame.output_ownership(), FrameOutputOwnership::Caller);
        // No copy or rendering is required to export an already valid image.
        assert_eq!(frame.graph.compile().unwrap().pass_count(), 0);
    }
}

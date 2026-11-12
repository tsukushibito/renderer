//! Lower owned engine input into ordinary, GPU-independent Graph operations.

use std::collections::BTreeMap;

use super::{FrameSnapshot, MeshHandle};
use crate::graph::{
    BufferDesc, BufferHandle, Graph, GraphError, GraphErrorKind, ImageDesc, ImageHandle, PassHandle,
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
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<FrameGraph, GraphError> {
        let mut graph = Graph::new();
        let mut meshes = BTreeMap::new();
        for draw in &self.draws {
            if meshes.contains_key(&draw.mesh) {
                continue;
            }
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
            graph.upload_buffer_shared(
                "mesh vertex upload",
                vertices,
                0,
                draw.data.vertices.clone(),
            )?;
            graph.upload_buffer_shared(
                "mesh index upload",
                indices,
                0,
                draw.data.indices.clone(),
            )?;
            meshes.insert(draw.mesh, (vertices, indices));
        }
        self.build_graph_with_meshes(graph, &meshes, readback, extension)
    }

    pub(crate) fn build_graph_with_meshes(
        &self,
        mut graph: Graph,
        meshes: &BTreeMap<MeshHandle, (BufferHandle, BufferHandle)>,
        readback: bool,
        extension: impl FnOnce(&mut Graph, ImageHandle) -> Result<ImageHandle, GraphError>,
    ) -> Result<FrameGraph, GraphError> {
        let desc = ImageDesc::rgba8(self.view.width, self.view.height);
        let color = graph.image("view color", desc)?;
        let depth = graph.image("view depth", ImageDesc::depth32(desc.width, desc.height))?;
        // The fixed opaque list shares one clear and rendering scope. An empty
        // list retains the clear-only path without requiring a Raster program.
        let mut draws = Vec::with_capacity(self.draws.len());
        for draw in &self.draws {
            let &(vertices, indices) = meshes.get(&draw.mesh).ok_or_else(|| {
                GraphError::new(
                    GraphErrorKind::InvalidInput,
                    "missing resolved mesh buffers",
                )
            })?;
            draws.push((vertices, indices, draw.data.index_count, draw.parameters));
        }
        graph.raster_batch(
            if draws.is_empty() {
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
            .with_resource(output.into(), Some(&graph.resources[output.index].name)));
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
    use crate::engine::{Draw, Engine, IDENTITY, Material, MeshData, Vertex, View};
    use crate::graph::{Graph, GraphErrorKind, ImageDesc, ImageHandle, ImportImage, ResourceKey};

    use super::FrameOutputOwnership;

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
    fn opaque_pass_count_is_independent_of_repeated_draw_count() {
        let mut assets = Engine::new(1, 1).unwrap();
        let mesh = assets.create_mesh(triangle()).unwrap();
        let material = assets.create_material(Material::default()).unwrap();
        let draw = Draw {
            mesh,
            material,
            world_from_model: IDENTITY,
        };
        for count in [1, 32, 256] {
            let snapshot = assets.snapshot(&view(), &vec![draw; count]).unwrap();
            let frame = snapshot.build_graph(false, |_, color| Ok(color)).unwrap();
            // Two shared mesh uploads and one opaque list; output is the list's color.
            assert_eq!(frame.graph.compile().unwrap().pass_count(), 3);
        }
    }

    #[test]
    fn opaque_pass_keeps_every_distinct_mesh_producer() {
        const MESHES: usize = 128;
        let mut assets = Engine::new(MESHES, 1).unwrap();
        let material = assets.create_material(Material::default()).unwrap();
        let draws = (0..MESHES)
            .map(|_| Draw {
                mesh: assets.create_mesh(triangle()).unwrap(),
                material,
                world_from_model: IDENTITY,
            })
            .collect::<Vec<_>>();
        let snapshot = assets.snapshot(&view(), &draws).unwrap();
        let frame = snapshot.build_graph(false, |_, color| Ok(color)).unwrap();
        let plan = frame.graph.compile().unwrap();
        assert_eq!(plan.pass_count(), MESHES * 2 + 1);
        assert!(plan.culled_pass_names().is_empty());
    }

    #[test]
    fn empty_view_has_initialized_output_without_uploads() {
        let assets = Engine::new(1, 1).unwrap();
        let snapshot = assets
            .snapshot(
                &View {
                    width: 4,
                    height: 3,
                    clip_from_world: IDENTITY,
                    clear_color: [0.25, 0.5, 0.75, 1.0],
                },
                &[],
            )
            .unwrap();
        let frame = snapshot.build_graph(false, |_, color| Ok(color)).unwrap();
        let plan = frame.graph.compile().unwrap();
        let dump = plan.dump();
        assert!(dump.contains("view clear"));
        assert!(!dump.contains("upload"));
        assert_eq!(plan.pass_count(), 1);
        assert_eq!(frame.output_ownership(), FrameOutputOwnership::Frame);
        assert!(frame.readback.is_none());
    }

    #[test]
    fn extension_output_checks_owner_format_extent_and_initialization() {
        let assets = Engine::new(1, 1).unwrap();
        let snapshot = assets.snapshot(&view(), &[]).unwrap();
        let mut foreign_graph = Graph::new();
        let foreign = foreign_graph
            .image("foreign", ImageDesc::rgba8(16, 16))
            .unwrap();
        let error = match snapshot.build_graph(false, |_, _| Ok(foreign)) {
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
            let error = match snapshot.build_graph(false, |graph, _| graph.image(name, desc)) {
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
            .build_graph(false, |graph, color| {
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
        let assets = Engine::new(1, 1).unwrap();
        let snapshot = assets.snapshot(&view(), &[]).unwrap();
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
            .build_graph(false, |graph, _| {
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

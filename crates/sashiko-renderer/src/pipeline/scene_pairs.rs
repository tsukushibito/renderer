//! Track compatible mesh/material revisions at asset-change boundaries.
use super::{RenderObject, SceneInput, ViewPipeline};
use crate::{material::MaterialHandle, mesh::MeshHandle};
use sashiko_renderer_core::graph::GraphError;
use std::collections::HashMap;

/// Identifies one prepared pipeline registration in the view renderer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct PipelineId(pub(super) u64);

/// Immutable scene inputs checked for one pipeline in the current frame.
///
/// Bind the object slice once, then pass this value to every camera that uses
/// this pipeline and scene. The slice and both asset registries stay borrowed
/// through the last use, preserving the checked handles and revisions.
#[derive(Clone, Copy)]
pub struct FrameScene<'a> {
    input: SceneInput<'a>,
    graph: u64,
    pipeline: PipelineId,
}

impl<'a> FrameScene<'a> {
    pub(super) fn resolve(
        self,
        graph: u64,
        pipeline: PipelineId,
    ) -> Result<SceneInput<'a>, GraphError> {
        if self.graph != graph || self.pipeline != pipeline {
            return Err(GraphError::invalid_input(
                "bound scene belongs to another frame or pipeline",
            ));
        }
        Ok(self.input)
    }
}

type Pair = (MeshHandle, MaterialHandle);
type Revisions = (u64, u64);

fn revisions(scene: SceneInput<'_>, object: &RenderObject) -> Result<Revisions, GraphError> {
    Ok((
        scene.meshes.revision(object.mesh)?,
        scene.materials.revision(object.material)?,
    ))
}

#[derive(Default)]
pub(super) struct ScenePairs {
    registered: HashMap<Pair, Revisions>,
    staged: HashMap<Pair, Revisions>,
}

impl ScenePairs {
    /// Apply only the supplied additions/updates. A failed batch preserves all
    /// prior registrations, including the old revisions of modified pairs.
    pub(super) fn register<P: ViewPipeline + ?Sized>(
        &mut self,
        pipeline: &P,
        changed: SceneInput<'_>,
    ) -> Result<(), GraphError> {
        self.stage(pipeline, changed)?;
        self.publish();
        Ok(())
    }

    pub(super) fn stage<P: ViewPipeline + ?Sized>(
        &mut self,
        pipeline: &P,
        changed: SceneInput<'_>,
    ) -> Result<(), GraphError> {
        self.staged.clear();
        for obj in changed.objects {
            let pair = (obj.mesh, obj.material);
            if self.staged.contains_key(&pair) {
                continue;
            }
            let current = revisions(changed, obj)?;
            if self.registered.get(&pair) != Some(&current) {
                pipeline.check_object(changed, obj)?;
                self.staged.insert(pair, current);
            }
        }
        self.registered
            .try_reserve(self.staged.len())
            .map_err(|_| GraphError::invalid_input("unable to reserve scene pair registrations"))?;
        Ok(())
    }

    pub(super) fn publish(&mut self) {
        self.registered.extend(self.staged.drain());
    }

    pub(super) fn remove(&mut self, mesh: MeshHandle, material: MaterialHandle) -> bool {
        self.registered.remove(&(mesh, material)).is_some()
    }

    pub(super) fn bind<'a>(
        &self,
        scene: SceneInput<'a>,
        graph: u64,
        pipeline: PipelineId,
    ) -> Result<FrameScene<'a>, GraphError> {
        for obj in scene.objects {
            let current = revisions(scene, obj)?;
            if self.registered.get(&(obj.mesh, obj.material)) != Some(&current) {
                return Err(GraphError::invalid_input(
                    "register changed mesh/material pairs before binding the scene",
                ));
            }
        }
        Ok(FrameScene {
            input: scene,
            graph,
            pipeline,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        material::{Materials, UnlitParams},
        mesh::{MeshCapacity, MeshData, Meshes},
        pbr::SceneLimits,
        pipeline::StandardPipeline,
        unlit_forward::Vertex,
    };
    use std::{error::Error, num::NonZeroUsize};

    fn triangle() -> Result<MeshData, GraphError> {
        let vertices = [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]].map(|position| Vertex {
            position,
            color: [1.; 4],
        });
        MeshData::unlit(&vertices, &[0, 1, 2])
    }

    #[test]
    fn incremental_registration_and_frame_scene_lifetime() -> Result<(), Box<dyn Error>> {
        let pipeline = StandardPipeline::forward(
            [8, 8],
            SceneLimits {
                max_lightmapped_draws: 0,
                frames_in_flight: NonZeroUsize::MIN,
                max_draws: 3,
                max_materials: 2,
                max_shadowed_rect_lights: 0,
                max_local_lights: 0,
                max_shadow_casters: 0,
                max_transforms: 0,
            },
        )?;
        let mut meshes = Meshes::new(MeshCapacity {
            meshes: 2,
            retired_mesh_versions: 2,
        })?;
        let mut materials = Materials::default();
        let a = RenderObject::new(
            meshes.create(triangle()?)?,
            materials.create_unlit(UnlitParams::default())?,
        );
        let b = RenderObject::new(
            meshes.create(triangle()?)?,
            materials.create_unlit(UnlitParams::default())?,
        );
        let missing = materials.duplicate(b.material)?;
        materials.remove(missing)?;
        let mut pairs = ScenePairs::default();
        let group = PipelineId(1);
        let frame = 7;
        let usable = |pairs: &ScenePairs, meshes: &Meshes, materials: &Materials, object| {
            pairs
                .bind(SceneInput::new(meshes, materials, &[object]), frame, group)
                .is_ok()
        };
        pairs.register(&pipeline, SceneInput::new(&meshes, &materials, &[a]))?;
        let invalid = RenderObject::new(b.mesh, missing);
        assert!(
            pairs
                .register(
                    &pipeline,
                    SceneInput::new(&meshes, &materials, &[b, invalid])
                )
                .is_err()
        );
        assert!(usable(&pairs, &meshes, &materials, a));
        assert!(!usable(&pairs, &meshes, &materials, b));

        pairs.register(&pipeline, SceneInput::new(&meshes, &materials, &[b]))?;
        let selected = [
            b,
            a,
            RenderObject {
                tint: [0.25; 4],
                ..b
            },
        ];
        let bound = pairs.bind(
            SceneInput::new(&meshes, &materials, &selected),
            frame,
            group,
        )?;
        assert_eq!(bound.resolve(frame, group)?.objects[2].tint, [0.25; 4]);
        assert_eq!(bound.resolve(frame, group)?.objects[0].material, b.material);
        assert!(bound.resolve(frame + 1, group).is_err());
        assert!(bound.resolve(frame, PipelineId(2)).is_err());

        materials.update_unlit(a.material, UnlitParams { color: [0.5; 4] })?;
        assert!(!usable(&pairs, &meshes, &materials, a));
        pairs.register(&pipeline, SceneInput::new(&meshes, &materials, &[a]))?;
        meshes.update(b.mesh, triangle()?)?;
        assert!(!usable(&pairs, &meshes, &materials, b));
        assert!(usable(&pairs, &meshes, &materials, a));
        assert!(pairs.remove(a.mesh, a.material));
        assert!(!usable(&pairs, &meshes, &materials, a));
        Ok(())
    }
}

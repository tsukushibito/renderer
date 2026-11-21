#![cfg(feature = "graphics")]
use renderer::{
    material::{Materials, PbrParams, UnlitParams},
    mesh::{MeshCapacity, MeshData, Meshes},
    pbr::Transform,
    pipeline::RenderObject,
    unlit_forward::Vertex,
};
use training_yard::scene::Scene;

#[test]
fn mixed_scene_and_host_extraction_can_borrow_the_same_subset()
-> Result<(), Box<dyn std::error::Error>> {
    let vertices = [[-1., -1., 0.], [1., -1., 0.], [0., 1., 0.]].map(|position| Vertex {
        position,
        color: [1.; 4],
    });
    let mut meshes = Meshes::new(MeshCapacity {
        meshes: 1,
        retired_mesh_versions: 1,
    })?;
    let mesh = meshes.create(MeshData::unlit(&vertices, &[0, 1, 2])?)?;
    let mut materials = Materials::default();
    let toon = materials.create_unlit(UnlitParams { color: [1.; 4] })?;
    let standard = materials.create_pbr(PbrParams::color([1.; 4]))?;
    let target = RenderObject::new(mesh, toon);
    let prop = RenderObject::new(mesh, standard);
    let mut scene = Scene::default();
    let ids: Vec<_> = (0..1024)
        .map(|i| scene.spawn(if i % 2 == 0 { target } else { prop }))
        .collect();
    let moved = Transform::new([[1., 0., 0., 1.], [0., 1., 0., 0.], [0., 0., 1., 0.]])?;
    let all: Vec<_> = ids.iter().map(|&id| (id, moved)).collect();
    scene.update_transforms(&all).unwrap();
    let subset: Vec<_> = ids[..16]
        .iter()
        .map(|&id| (id, Transform::IDENTITY))
        .collect();
    scene.update_transforms(&subset).unwrap();
    scene.update_transforms(&[]).unwrap();
    let moved_targets =
        |object: &&RenderObject| object.material == toon && object.transform == moved;
    let host: Vec<_> = (0..1024)
        .map(|i| {
            let object = if i % 2 == 0 { target } else { prop };
            object.with_transform(if i < 16 { Transform::IDENTITY } else { moved })
        })
        .collect();
    assert_eq!(host.iter().filter(moved_targets).count(), 504);
    assert_eq!(
        scene
            .instances()
            .map(|(_, obj)| obj)
            .filter(moved_targets)
            .count(),
        504
    );
    scene.remove(ids[0]).unwrap();
    let replacement = scene.spawn(target);
    assert_ne!(ids[0], replacement);
    assert_eq!(
        scene.update_transforms(&[(replacement, moved), (ids[0], moved)]),
        Err(ids[0])
    );
    assert_eq!(
        scene.instance_mut(replacement).unwrap().transform,
        Transform::IDENTITY
    );
    assert!(scene.instance_mut(ids[0]).is_none());
    Ok(())
}

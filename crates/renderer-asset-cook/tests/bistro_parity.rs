use renderer_asset::{AssetId, Generation, ObjectStore};
use std::path::Path;

#[test]
fn asymmetric_uvs_reproduce_bistro_corner_tangent_frames() {
    let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../training-yard/assets/equipment/dist");
    let generation = Generation::open(&dist).unwrap();
    let id: AssetId = "00000000000000000000000000000005".parse().unwrap();
    let primitive = &generation.equipment(&id).unwrap().primitives[0];
    let store = ObjectStore::new(&dist);
    let bytes = store.load(&primitive.mesh).unwrap();
    let mesh = bytes.mesh().unwrap();

    // Indexed triangle corners from tools/bistro/cook.py cook_primitive with
    // the pinned MikkTSpace generator and an identity model transform. Compare
    // attributes in draw order so equivalent vertex table layouts are accepted.
    let expected = [
        [-0.35, 0., 0.9916721, -0.12878859, -0.25, 0.1],
        [0.35, 0., 0.99825555, 0.05904074, 0.75, 0.2],
        [-0.175, 0.75, 0.97282594, 0.23153758, -0.1, 0.95],
        [0.35, 0., 0.99825555, 0.05904074, 0.75, 0.2],
        [0.175, 0.75, 0.85285133, 0.5221538, 0.9, 0.8],
        [-0.175, 0.75, 0.97282594, 0.23153758, -0.1, 0.95],
    ];
    assert_eq!(mesh.indices.len(), expected.len());
    for (&index, [x, y, tx, ty, u, v]) in mesh.indices.iter().zip(expected) {
        let reference = [x, y, 0.5, 0., 0., -1., tx, ty, 0., 1., u, v];
        for (actual, expected) in mesh.vertices[index as usize].iter().zip(reference) {
            assert!((actual - expected).abs() <= 2e-6, "{actual} vs {expected}");
        }
    }

    // Source-level RGBA bytes from the comparison fixture, in UV row order.
    let color = store.load(primitive.images[0].as_ref().unwrap()).unwrap();
    let normal = store.load(primitive.images[1].as_ref().unwrap()).unwrap();
    let color = color.image().unwrap();
    let normal = normal.image().unwrap();
    assert!(color.srgb && !normal.srgb);
    assert_eq!(
        color.pixels[..16],
        [
            255, 0, 0, 0, 0, 255, 0, 255, 0, 0, 255, 255, 128, 128, 128, 255
        ]
    );
    assert_eq!(
        normal.pixels[..16],
        [
            255, 128, 128, 255, 128, 255, 128, 255, 128, 128, 255, 255, 128, 128, 255, 255
        ]
    );
    assert_eq!(primitive.images[0], primitive.images[3]);
    let bytes = store.load(&primitive.material).unwrap();
    let material = bytes.material().unwrap();
    let factors = material.factors().unwrap();
    assert_eq!(factors.alpha_mode, renderer::material::AlphaMode::Mask);
    assert_eq!(factors.alpha_cutoff, 0.5);
    assert!(factors.double_sided);
    assert_eq!(factors.emissive_nits, [1.0; 3]);
    assert_ne!(material.samplers[0], material.samplers[3]);
}

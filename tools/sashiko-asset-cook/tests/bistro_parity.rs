use sashiko_asset::{AssetId, Generation, ObjectStore};
#[path = "support/project.rs"]
mod project;

#[test]
fn cooked_material_preserves_bistro_maps_and_sampler_settings() -> project::Result {
    let fixture = project::Project::new()?;
    fixture.report()?;
    let dist = fixture.0.join("dist");
    let generation = Generation::open(&dist).unwrap();
    let id: AssetId = "00000000000000000000000000000005".parse().unwrap();
    let primitive = &generation.equipment(&id).unwrap().primitives[0];
    let store = ObjectStore::new(&dist);
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
    let factors = material.factors;
    assert_eq!(
        factors.alpha_mode,
        sashiko_renderer::material::AlphaMode::Mask
    );
    assert_eq!(factors.alpha_cutoff, 0.5);
    assert!(factors.double_sided);
    assert_eq!(factors.emissive_nits, [1.0; 3]);
    assert_ne!(material.samplers[0], material.samplers[3]);
    Ok(())
}

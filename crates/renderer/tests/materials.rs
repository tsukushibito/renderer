use renderer::{
    material::{
        AlphaMode, MaterialDefinition, MaterialInstance, Materials, Parameter, PbrParams,
        UnlitParams, Value,
    },
    pbr,
    pipeline::{StandardConfig, StandardPipeline},
};
use renderer_core::{graph::SampleCount, program::ManagedRasterProgram};

fn tinted() -> MaterialDefinition {
    MaterialDefinition::new(
        ManagedRasterProgram::mesh_hdr(),
        Some(ManagedRasterProgram::depth_only()),
        AlphaMode::Opaque,
        vec![Parameter::Constant {
            name: "color".into(),
            word: 16,
            default: Value::Color([1.; 4]),
        }],
    )
    .unwrap()
}
#[test]
fn revisions_are_shared_until_edited_and_survive_removal() {
    let mut materials = Materials::default();
    let handle = materials.create_unlit(Default::default()).unwrap();
    let copy = materials.duplicate(handle).unwrap();
    let first = materials.snapshot(handle).unwrap();
    assert!(std::sync::Arc::ptr_eq(
        &first,
        &materials.snapshot(copy).unwrap()
    ));
    materials
        .update_unlit(handle, UnlitParams { color: [0.; 4] })
        .unwrap();
    assert!(matches!(materials.get(handle),Ok(MaterialInstance::Unlit(p)) if p.color==[0.;4]));
    assert!(
        materials
            .update_unlit(
                handle,
                UnlitParams {
                    color: [f32::NAN; 4]
                }
            )
            .is_err()
    );
    materials.remove(handle).unwrap();
    assert!(materials.get(handle).is_err());
    assert!(matches!(&*first,MaterialInstance::Unlit(p) if p.color==[1.;4]));
    assert!(Materials::default().get(copy).is_err());
    let pbr = materials.create_pbr(PbrParams::default()).unwrap();
    assert_eq!(materials.get(pbr).unwrap().alpha_mode(), AlphaMode::Opaque);
}
#[test]
fn overrides_are_typed_and_updates_are_atomic() {
    let def = tinted();
    let mut materials = Materials::default();
    let handle = materials.create_custom(&def, &[]).unwrap();
    let value = |m: &Materials| match m.get(handle).unwrap() {
        MaterialInstance::Custom(c) => match c.value("color") {
            Some(Value::Color(v)) => *v,
            _ => panic!(),
        },
        _ => panic!(),
    };
    assert_eq!(value(&materials), [1.; 4]);
    assert!(
        materials
            .update_custom(
                handle,
                &[
                    ("color", Value::Color([0.; 4])),
                    ("missing", Value::Float(1.))
                ]
            )
            .is_err()
    );
    assert!(
        materials
            .update_custom(handle, &[("color", Value::Float(0.))])
            .is_err()
    );
    assert_eq!(value(&materials), [1.; 4]);
    materials
        .update_custom(handle, &[("color", Value::Color([0.; 4]))])
        .unwrap();
    assert_eq!(value(&materials), [0.; 4]);
}
#[test]
fn rejects_incompatible_abis_and_scene_modes_at_preparation() {
    assert!(
        MaterialDefinition::new(
            ManagedRasterProgram::mesh(),
            None,
            AlphaMode::Opaque,
            vec![]
        )
        .is_err()
    );
    assert!(
        MaterialDefinition::new(
            ManagedRasterProgram::mesh_hdr(),
            None,
            AlphaMode::Opaque,
            vec![Parameter::Constant {
                name: "overlap".into(),
                word: 14,
                default: Value::Color([0.; 4])
            }]
        )
        .is_err()
    );
    let config = StandardConfig {
        extent: [16; 2],
        clear_color: [0.; 4],
        shadows: None,
        samples: SampleCount::X4,
    };
    let limits = pbr::SceneLimits {
        frames_in_flight: std::num::NonZeroUsize::MIN,
        max_draws: 1,
        max_materials: 1,
        max_transforms: 0,
        max_shadowed_rect_lights: 0,
        max_local_lights: 0,
        max_shadow_casters: 0,
    };
    assert!(StandardPipeline::new(pbr::Technique::Forward, config, limits, &[tinted()]).is_err());
}

#[test]
fn texture_slots_can_be_declared_before_gpu_upload() {
    let slot = |name: &str, group, binding, default| Parameter::Binding {
        name: name.into(),
        group,
        binding,
        default,
    };
    let def = MaterialDefinition::new(
        ManagedRasterProgram::textured_mesh_hdr(),
        None,
        AlphaMode::Opaque,
        vec![
            slot("offset", 0, 0, None),
            slot("map", 1, 0, None),
            slot(
                "sampler",
                1,
                1,
                Some(Value::Sampler(
                    renderer_core::program::SamplerDescriptor::LinearRepeat,
                )),
            ),
        ],
    )
    .unwrap();
    let mut materials = Materials::default();
    assert!(materials.create_custom(&def, &[]).is_err());
    assert!(
        materials
            .create_custom(
                &def,
                &[("offset", Value::Float(2.)), ("map", Value::Color([1.; 4]))]
            )
            .is_err()
    );
}

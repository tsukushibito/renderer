use glam::{Mat4, Quat};
use sashiko_animation::{Clip, LocalPose, Playback, PoseBatch, PoseWorkspace, Skeleton};

#[test]
fn mixed_phases_and_fades_preserve_partial_hierarchies_and_order() {
    let rig = Skeleton::new(vec![Some(2), Some(2), None]).unwrap();
    let rate = 60.try_into().unwrap();
    let make = |angle: f32| {
        let frames = (0..6)
            .map(|i| {
                LocalPose::new(
                    [i as f32, 0.4, 1.],
                    Quat::from_rotation_z(angle + i as f32 * 0.25).to_array(),
                    [1.2, 0.6, 1.7],
                )
            })
            .collect();
        Clip::new(rig.clone(), frames, rate, true, vec![], vec![]).unwrap()
    };
    let source = make(0.5);
    let target = make(4.5);
    let mut player = Playback::new(&source, rate);
    player.crossfade(&target, 8.try_into().unwrap()).unwrap();
    let mut subset = PoseWorkspace::subset(&rig, &[0]).unwrap();
    let mut batch = PoseBatch::new(&subset, 5);
    for tick in [0, 1, 4, 8] {
        player.advance_to(tick, &mut []).unwrap();
        let samples = [
            source.sample_at(tick, rate),
            player.sample(),
            target.sample_at(tick + 1, rate),
            player.sample(),
            source.sample_at(tick + 1, rate),
        ];
        for n in [0, 1, 3, 4, 5] {
            let outputs = batch.evaluate(&samples[..n]).unwrap();
            assert_eq!(outputs.len(), n);
            for (out, sample) in outputs.iter().zip(samples) {
                sample.evaluate(&mut subset).unwrap();
                for i in [0, 2] {
                    assert!(
                        Mat4::from_cols_array_2d(&out.joint(i).unwrap())
                            .abs_diff_eq(Mat4::from_cols_array_2d(&subset.joint(i).unwrap()), 2e-5)
                    );
                }
                assert!(out.joint(1).is_none());
            }
        }
        assert!(batch.evaluate(&[player.sample(); 6]).is_err());
    }
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn scoped_packed_worlds_match_serial_with_fades_and_scalar_tail() {
    let joints = 19;
    let rig = Skeleton::new(
        (0..joints)
            .map(|i| if i == 0 { None } else { Some((i - 1) / 2) })
            .collect(),
    )
    .unwrap();
    let rate = 60.try_into().unwrap();
    let make = |angle: f32| {
        Clip::new(
            rig.clone(),
            (0..joints)
                .map(|i| {
                    LocalPose::new(
                        [i as f32, 0.5, 1.0],
                        Quat::from_rotation_x(angle + i as f32 * 0.01).to_array(),
                        [1.2, 0.9, 0.6],
                    )
                })
                .collect(),
            rate,
            true,
            vec![],
            vec![],
        )
        .unwrap()
    };
    let a = make(0.2);
    let b = make(1.7);
    let mut fade = Playback::new(&a, rate);
    fade.crossfade(&b, 8.try_into().unwrap()).unwrap();
    fade.advance_to(3, &mut []).unwrap();
    let samples: Vec<_> = (0..47)
        .map(|i| {
            if i % 3 == 0 {
                fade.sample()
            } else {
                b.sample_at(i, rate)
            }
        })
        .collect();
    let layout = PoseWorkspace::subset(&rig, &[12, 18]).unwrap();
    let mut sequential = PoseBatch::new(&layout, samples.len());
    let mut parallel = PoseBatch::new(&layout, samples.len());
    let expected = sequential.evaluate(&samples).unwrap();
    let mut jobs = sashiko_jobs::JobSystem::new(4.try_into().unwrap()).unwrap();
    let actual = jobs
        .scope(|s| parallel.evaluate_scoped(&samples, s))
        .unwrap();
    for (a, b) in actual.iter().zip(expected) {
        for j in 0..joints {
            assert_eq!(a.joint(j), b.joint(j));
        }
    }
}

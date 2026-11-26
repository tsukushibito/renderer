use sashiko_animation::{Clip, LocalPose, Playback, PoseBatch, PoseWorkspace, Skeleton};

pub fn check(jobs: &mut sashiko_jobs::JobSystem) {
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
                        {
                            let (sin, cos) = ((angle + i as f32 * 0.01) * 0.5).sin_cos();
                            [sin, 0., 0., cos]
                        },
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
    let actual = jobs
        .scope(|s| parallel.evaluate_scoped(&samples, s))
        .unwrap();
    for (a, b) in actual.iter().zip(expected) {
        for j in 0..joints {
            assert_eq!(a.joint(j), b.joint(j));
        }
    }
}

use sashiko_animation::*;
use std::sync::Arc;

fn clip(rig: &Arc<Skeleton>, xs: &[f32], looping: bool) -> Clip {
    Clip::new(
        rig.clone(),
        xs.iter()
            .map(|&x| LocalPose::new([x, 0., 0.], [0., 0., 0., 1.], [1.; 3]))
            .collect(),
        30.try_into().unwrap(),
        looping,
        vec![],
        vec![],
    )
    .unwrap()
}
#[test]
fn bounded_interpolation_is_independent_of_simd_lane() {
    let rig = Skeleton::new(vec![None]).unwrap();
    let source = clip(&rig, &[f32::MAX, -f32::MAX], true);
    let rate = 60.try_into().unwrap();
    let mut out = PoseWorkspace::all(&rig);
    let mut batch = PoseBatch::new(&out, 5);
    let middle = source.interpolated_at(1, rate);
    for out in batch.evaluate(&[middle; 5]).unwrap() {
        assert_eq!(out.joint(0).unwrap()[3][0], 0.);
    }
    for (asset, tick, x) in [(true, 3, 0.), (false, 3, -f32::MAX)] {
        clip(&rig, &[f32::MAX, -f32::MAX], asset)
            .interpolated_at(tick, rate)
            .evaluate(&mut out)
            .unwrap();
        assert_eq!(out.joint(0).unwrap()[3][0], x);
    }
}
#[test]
fn layers_mix_in_local_space_and_preserve_unmasked_joints() {
    let rig = Skeleton::new(vec![None, Some(0)]).unwrap();
    let a = clip(&rig, &[2., 1.], true);
    let b = clip(&rig, &[4., 3.], true);
    let rate = 30.try_into().unwrap();
    let [a, b] = [&a, &b].map(|c| c.sample_at(0, rate));
    let mask = JointMask::new(&rig, vec![0., 1.]).unwrap();
    let mut mixer = LocalBuffer::new(&rig);
    let mut out = PoseWorkspace::all(&rig);
    let line = BlendLine::new(vec![2., 4.]).unwrap();
    mixer
        .mix(&line.weights(3.).unwrap().map(|(i, w)| ([a, b][i], w)))
        .unwrap();
    mixer.overlay(b, &mask, 1.).unwrap();
    mixer.additive(b, a, &mask, 0.).unwrap();
    mixer.sample().evaluate(&mut out).unwrap();
    assert_eq!(out.joint(0).unwrap()[3][0], 3.);
    assert_eq!(out.joint(1).unwrap()[3][0], 6.);
    mixer.additive(b, a, &mask, 0.5).unwrap();
    mixer.sample().evaluate(&mut out).unwrap();
    assert_eq!(out.joint(1).unwrap()[3][0], 7.);
    let space = BlendSpace::new(vec![[0., 0.], [1., 0.], [0., 1.]], vec![[0, 1, 2]]).unwrap();
    for (pos, expected) in [
        ([0.25, 0.25], [0.5, 0.25, 0.25]),
        ([2., 2.], [0., 0.5, 0.5]),
    ] {
        let mut weights = [0.; 3];
        for (i, w) in space.weights(pos).unwrap() {
            weights[i] = w;
        }
        assert_eq!(weights, expected);
    }
}
#[test]
fn timeline_and_terminal_fade_agree_across_long_catchup() {
    let rig = Skeleton::new(vec![None]).unwrap();
    let rate = 60.try_into().unwrap();
    let timeline = ClipTimeline::new(
        1,
        30.try_into().unwrap(),
        false,
        vec![],
        vec![[0.; 3], [16_777_217., 0., 0.]],
    )
    .unwrap();
    let end = timeline
        .interval(0, u64::MAX, rate, &mut [])
        .unwrap()
        .translation;
    assert_eq!(end, [16_777_217., 0., 0.]);
    assert!(timeline.interval(2, 1, rate, &mut []).is_err());
    let a = Clip::from_timeline(
        rig,
        vec![LocalPose::new([0.; 3], [0., 0., 0., 1.], [1.; 3])],
        timeline,
    )
    .unwrap();
    let mut p = Playback::new(&a, rate);
    p.crossfade(&a, u64::MAX.try_into().unwrap()).unwrap();
    assert!(p.is_transitioning());
    assert_eq!(
        p.advance_to(1, &mut []).unwrap().translation,
        [8_388_608.5, 0., 0.]
    );
    assert!(!p.is_finished());
    assert_eq!(
        p.advance_to(u64::MAX, &mut []).unwrap().translation,
        [8_388_608.5, 0., 0.]
    );
    assert!(p.is_finished());
    assert!(!p.is_transitioning());
    assert_eq!(p.tick(), u64::MAX);
}
#[test]
fn markers_map_across_the_seam_in_cyclic_id_order() {
    let make = |marks: Vec<(usize, u32)>| {
        ClipTimeline::new(100, 60.try_into().unwrap(), true, vec![], vec![])
            .unwrap()
            .with_sync_markers(
                marks
                    .into_iter()
                    .map(|(frame, id)| Marker { frame, id })
                    .collect(),
            )
            .unwrap()
    };
    let map =
        PhaseMap::between(&make(vec![(20, 1), (70, 2)]), &make(vec![(10, 2), (80, 1)])).unwrap();
    for (a, b) in [(0.2, 0.8), (0.7, 1.1), (1.2, 1.8), (0.95, 1.45)] {
        assert!((map.map(a) - b).abs() < 1e-10);
    }
}

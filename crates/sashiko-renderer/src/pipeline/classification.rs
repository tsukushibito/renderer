//! Pure per-object camera and shadow decisions, shared by history and drawing.
use super::*;
use sashiko_renderer_core::jobs::SharedJobs;
use std::{num::NonZeroUsize, sync::Mutex};

#[derive(Clone, Copy, Default)]
pub(super) struct Visibility {
    pub bounds: Option<Bounds>,
    pub camera: bool,
    pub caster: bool,
    pub cascades: [bool; pbr::CASCADE_COUNT],
    pub rectangle: bool,
    pub sort_depth: Option<f64>,
}

struct Classify<'a> {
    view: &'a PreparedView,
    materials: &'a Materials,
    lights: &'a [LocalLight],
    cascades: bool,
}

impl Classify<'_> {
    fn object(&self, draw: &Draw) -> Result<Visibility, GraphError> {
        let instance = self.materials.get(draw.material)?;
        let blend = instance.alpha_mode() == AlphaMode::Blend;
        let caster = draw.casts_shadow
            && match instance {
                MaterialInstance::Pbr(_) => !blend,
                MaterialInstance::Custom(m) => m.definition().shadow().is_some(),
                MaterialInstance::Unlit(_) => true,
            };
        let bounds = draw.world_bounds(instance);
        let camera = draw.camera_visible && bounds.is_none_or(|b| self.view.visible(b));
        let cascades = std::array::from_fn(|i| {
            self.cascades
                && caster
                && bounds.is_none_or(|b| self.view.cascade_overlaps(i, b) != Some(false))
        });
        let rectangle = caster
            && self
                .lights
                .iter()
                .filter_map(LocalLight::shadow_emitter)
                .any(|light| bounds.is_none_or(|b| pbr::rect_shadows::affects(light, b)));
        let sort_depth = (camera && blend)
            .then(|| draw.view_depth(self.view))
            .transpose()?;
        Ok(Visibility {
            bounds,
            camera,
            caster,
            cascades,
            rectangle,
            sort_depth,
        })
    }
}

pub(super) struct Classification {
    pub results: Box<[Visibility]>,
    jobs: Option<(SharedJobs, NonZeroUsize)>,
}

impl Classification {
    pub fn new(capacity: usize) -> Self {
        Self {
            results: vec![Visibility::default(); capacity].into_boxed_slice(),
            jobs: None,
        }
    }

    pub fn run(
        &mut self,
        draws: &[Draw],
        materials: &Materials,
        view: &PreparedView,
        cascades: bool,
        lights: &[LocalLight],
        allow_jobs: bool,
    ) -> Result<(), GraphError> {
        #[cfg(feature = "tracy")]
        let _span = tracy_client::Client::running()
            .map(|client| client.span(tracy_client::span_location!("renderer.scene.classify"), 0));
        let capacity = self.results.len();
        let out = self.results.get_mut(..draws.len()).ok_or_else(|| {
            GraphError::capacity_error(
                sashiko_renderer_core::graph::GraphCapacityKind::Draws,
                capacity,
                Some(draws.len()),
            )
        })?;
        let classify = Classify {
            view,
            materials,
            lights: if lights.iter().any(|light| light.shadow_emitter().is_some()) {
                lights
            } else {
                &[]
            },
            cascades,
        };
        if allow_jobs
            && let Some((team, min)) = &self.jobs
            && draws.len() >= min.get()
            && team.parallelism().get() > 1
        {
            let first_error = Mutex::new(None::<(usize, GraphError)>);
            team.with(|jobs| {
                let pieces = jobs.parallelism().get().saturating_mul(4);
                let width = NonZeroUsize::new(draws.len().div_ceil(pieces)).unwrap();
                jobs.scope(|scope| {
                    scope.for_each_chunk_mut(out, width, |start, chunk| {
                        for (i, output) in chunk.iter_mut().enumerate() {
                            let index = start + i;
                            match classify.object(&draws[index]) {
                                Ok(result) => *output = result,
                                Err(error) => {
                                    let mut prior = first_error.lock().unwrap();
                                    if prior.as_ref().is_none_or(|(first, _)| index < *first) {
                                        *prior = Some((index, error));
                                    }
                                    break;
                                }
                            }
                        }
                    })
                });
            });
            if let Some((_, error)) = first_error.into_inner().unwrap() {
                return Err(error);
            }
        } else {
            for (output, draw) in out.iter_mut().zip(draws) {
                *output = classify.object(draw)?;
            }
        }
        Ok(())
    }
}

impl StandardPipeline {
    /// Classify views with a shared CPU team when the input reaches `min_objects`.
    /// Disjoint chunks write prepared slots; graph imports and draw order belong to the host.
    pub fn set_scene_jobs(&mut self, jobs: SharedJobs, min_objects: NonZeroUsize) {
        self.auto_jobs = None;
        self.classification.jobs = Some((jobs, min_objects));
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;
    use crate::{
        material::{MaterialFactors, PbrParams},
        unlit_forward::IDENTITY,
    };
    use sashiko_renderer_core::{
        graph::{BufferDesc, GraphPool, ImageDesc},
        jobs::JobSystem,
    };

    #[test]
    fn jobs_match_serial_visibility_and_recover_after_invalid_input() -> Result<(), GraphError> {
        let mut pool = GraphPool::new(Default::default(), 1)?;
        let mut graph = pool.begin()?;
        let image = graph.image("environment", ImageDesc::rgba16_float(1, 1))?;
        let map = graph.image_view(image, 0, 0)?;
        let buffer = graph.buffer("geometry", BufferDesc { size: 64 })?;
        let view = PreparedView::prepare(
            ViewInput::new(
                IDENTITY,
                [0., 0., -1.],
                [0., 0., 1.],
                [1., 2.],
                Projection::Orthographic,
            )?,
            Sun::from_lux([0.4, 1., 0.3], [1.; 3], 1.)?,
            Some(Bounds::new([-8.; 3], [8.; 3])?),
            Environment {
                irradiance: map,
                reflection_metadata: buffer,
                local_reflections: None,
                diffuse_lighting: Default::default(),
                reflection: map,
                brdf_lut: map,
                nits_per_unit: 1.,
                yaw_radians: 0.,
                max_prefilter_lod: 0,
            },
            [32, 32],
            Some(ShadowSettings {
                resolution: 32,
                ..Default::default()
            }),
        )?;
        let mut materials = Materials::default();
        let opaque = materials.create_pbr(PbrParams::default())?;
        let glass = materials.create_pbr(PbrParams {
            factors: MaterialFactors {
                alpha_mode: AlphaMode::Blend,
                ..Default::default()
            },
            ..Default::default()
        })?;
        let base = Draw::new(
            Mesh {
                bounds: Bounds::new([0., 0., 0.4], [0.1, 0.1, 0.6])?,
                vertex_stride: 16,
                vertices: buffer,
                indices: buffer,
                first_index: 0,
                index_count: 3,
            },
            opaque,
        );
        let mut draws = [base; 64];
        draws[0].material = glass;
        draws[3].camera_visible = false;
        draws[9].transform = Some(Transform::new([
            [1., 0., 0., 4.],
            [0., 1., 0., 0.],
            [0., 0., 1., 0.],
        ])?);
        let mut reference = Classification::new(draws.len());
        let mut candidate = Classification::new(draws.len());
        candidate.jobs = Some((
            SharedJobs::new(JobSystem::new(4.try_into().unwrap()).unwrap()),
            NonZeroUsize::MIN,
        ));
        reference.run(&draws, &materials, &view, true, &[], true)?;
        assert!(reference.results[0].sort_depth.is_some());
        assert!(!reference.results[0].caster);
        assert!(!reference.results[3].camera);
        assert!(reference.results[3].cascades.contains(&true));
        assert!(!reference.results[9].camera);
        draws[0].sort_center[0] = f32::NAN;
        draws[63].material = materials.create_pbr(PbrParams::default())?;
        materials.remove(draws[63].material)?;
        assert_eq!(
            candidate
                .run(&draws, &materials, &view, true, &[], true)
                .unwrap_err()
                .message,
            "nonfinite transparent sort center"
        );
        draws[0].sort_center = base.sort_center;
        draws[63] = base;
        candidate.run(&draws, &materials, &view, true, &[], true)?;
        for (expected, actual) in reference.results.iter().zip(candidate.results.iter()) {
            assert_eq!(expected.camera, actual.camera);
            assert_eq!(expected.cascades, actual.cascades);
            assert_eq!(expected.sort_depth, actual.sort_depth);
            assert_eq!(
                expected.bounds.map(|b| (b.min(), b.max())),
                actual.bounds.map(|b| (b.min(), b.max()))
            );
        }
        Ok(())
    }
}

//! Draw a small offscreen scene with typed PBR vertices and material defaults.
//! Use `--copy-hdr` to compose a custom Graph pass before SDR conversion.
//! Use `--bloom` to add Bloom and `--reuse-shadows` to draw two frames using
//! the first frame's accepted shadow maps. Use `--msaa 4` to resolve the color
//! from a multisampled scene.

#[cfg(any(test, feature = "vulkan", all(windows, feature = "d3d12")))]
mod scene {
    use std::num::NonZeroUsize;

    use renderer::{
        capacity::{Preparation, ResourceClass},
        geometry::GeometryData,
        graph::{
            BufferDesc, CompiledGraph, Graph, GraphError, GraphPool, ImageDesc, PassHandle,
            SampleCount,
        },
    };
    #[cfg(test)]
    use renderer_pipeline::pbr::Transform;
    use renderer_pipeline::{
        passes::{
            bloom::{Bloom, PreparedBloom},
            output::{SdrEncoding, SdrOutput},
            pbr::{MATERIAL_BYTES, VERTEX_BYTES},
        },
        pbr::{
            Bounds, CASCADE_COUNT, Camera, CameraStorage, DefaultResources, DefaultViews, Draw,
            Frame, IndexedDraw, Material, MaterialFactors, PbrVertex, SceneLimits, ShadowHistory,
            ShadowSettings, ShadowUpdates, Sun,
        },
        pbr_forward::{self, Config, Prepared},
    };

    pub const EXTENT: [u32; 2] = [64, 48];
    pub const SHADOW_RESOLUTION: u32 = 64;
    const COPY_PASS: &str = "application HDR copy";

    pub struct Triangle {
        geometry: GeometryData,
        material: [u8; MATERIAL_BYTES],
    }

    impl Triangle {
        pub fn new(glowing: bool, placement: Placement) -> Result<Self, GraphError> {
            let (normal, tangent, object_scale) = match placement {
                #[cfg(test)]
                Placement::Tilted(scale_exponent) => (
                    [0.8, 0.0, 0.6],
                    [0.0, 1.0, 0.0, 1.0],
                    scale_exponent.map_or(1.0, |exponent| 2.0_f32.powi(-exponent)),
                ),
                _ => ([0.0, 0.0, 1.0], [1.0, 0.0, 0.0, 1.0], 1.0),
            };
            let positions = [
                ([-0.8_f32, -0.7, 0.0], [0.0, 1.0]),
                ([0.8, -0.7, 0.0], [1.0, 1.0]),
                ([0.0, 0.7, 0.0], [0.5, 0.0]),
            ];
            let mut vertices = [0; 3 * VERTEX_BYTES];
            for ((position, uv), bytes) in positions.into_iter().zip(vertices.as_chunks_mut().0) {
                let position = position.map(|value| value * object_scale);
                *bytes = PbrVertex::new(position, normal, tangent, uv)?.encode();
            }
            Ok(Self {
                geometry: GeometryData::new(VERTEX_BYTES as u32, &vertices, &[0, 1, 2])?,
                material: MaterialFactors {
                    base_color: [0.75, 0.015, 0.009, 1.0],
                    emissive: [if glowing { 2.0 } else { 0.0 }, 0.0, 0.0],
                    ..MaterialFactors::default()
                }
                .encode()?,
            })
        }

        fn buffer_descs(&self) -> [BufferDesc; 3] {
            [
                self.geometry.vertex_byte_len(),
                self.geometry.index_byte_len(),
                self.material.len() as u64,
            ]
            .map(|size| BufferDesc { size })
        }

        pub fn reserve(
            &self,
            preparation: &mut Preparation,
            copy_hdr: bool,
        ) -> Result<(), GraphError> {
            for desc in self.buffer_descs() {
                preparation.reserve_buffer(desc, 1)?;
            }
            if copy_hdr {
                preparation.reserve_image(hdr_desc(), 1)?;
            }
            Ok(())
        }

        fn upload(
            &self,
            graph: &mut Graph,
            defaults: &DefaultViews,
        ) -> Result<(IndexedDraw, [Material; 1]), GraphError> {
            let [vertex_desc, index_desc, material_desc] = self.buffer_descs();
            let vertices = graph.buffer("triangle vertices", vertex_desc)?;
            let indices = graph.buffer("triangle indices", index_desc)?;
            let factors = graph.buffer("triangle material factors", material_desc)?;
            self.geometry
                .upload_vertices(graph, "upload vertices", vertices)?;
            self.geometry
                .upload_indices(graph, "upload indices", indices)?;
            graph.upload_buffer("upload material factors", factors, 0, &self.material)?;

            Ok((
                IndexedDraw {
                    vertices,
                    indices,
                    first_index: 0,
                    index_count: self.geometry.index_count(),
                    material: 0,
                },
                [Material {
                    factors,
                    factor_offset: 0,
                    textures: defaults.material_textures([None; 4]),
                }],
            ))
        }
    }

    fn hdr_desc() -> ImageDesc {
        ImageDesc::rgba16_float(EXTENT[0], EXTENT[1])
    }

    #[derive(Clone, Copy, Eq, PartialEq)]
    pub enum Placement {
        Single,
        #[cfg(test)]
        Individual,
        #[cfg(test)]
        Instanced,
        #[cfg(test)]
        Tilted(Option<i32>),
    }

    impl Placement {
        fn uses_transforms(self) -> bool {
            match self {
                Self::Single => false,
                #[cfg(test)]
                Self::Tilted(exponent) => exponent.is_some(),
                #[cfg(test)]
                Self::Individual | Self::Instanced => true,
            }
        }
    }

    #[derive(Clone, Copy)]
    pub struct Settings {
        pub samples: SampleCount,
        pub placement: Placement,
    }

    impl Settings {
        pub fn new(samples: SampleCount) -> Self {
            Self {
                samples,
                placement: Placement::Single,
            }
        }
    }

    pub fn prepare(
        frames_in_flight: NonZeroUsize,
        settings: Settings,
    ) -> Result<Prepared, GraphError> {
        let config = Config {
            extent: EXTENT,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            shadows: Some(ShadowSettings {
                resolution: SHADOW_RESOLUTION,
                ..ShadowSettings::default()
            }),
            output: SdrOutput {
                encoding: SdrEncoding::Srgb,
                fxaa: false,
            },
            samples: settings.samples,
            camera_storage: CameraStorage::Transient,
        };
        let count = if settings.placement == Placement::Single {
            1
        } else {
            2
        };
        let limits = SceneLimits {
            frames_in_flight,
            max_draws: count,
            max_materials: 1,
            max_casters_per_cascade: count,
        };
        if settings.placement.uses_transforms() {
            pbr_forward::prepare_with_transforms(config, limits)
        } else {
            pbr_forward::prepare(config, limits)
        }
    }

    pub struct PostProcess {
        copy_hdr: bool,
        bloom: Option<PreparedBloom>,
    }

    pub fn reserve(
        prepared: &Prepared,
        triangle: &Triangle,
        copy_hdr: bool,
        bloom: Bloom,
    ) -> Result<(Preparation, PostProcess), GraphError> {
        let recipe = prepared.preparation();
        let mut preparation = Preparation::new(recipe.frames_in_flight());
        preparation.add_requirements(recipe.graph_requirements(), recipe.working_set())?;
        DefaultResources::reserve(&mut preparation)?;
        triangle.reserve(&mut preparation, copy_hdr)?;
        let bloom = if bloom == Bloom::Off {
            None
        } else {
            Some(bloom.prepare(&mut preparation, hdr_desc())?)
        };
        Ok((preparation, PostProcess { copy_hdr, bloom }))
    }

    pub fn resource_count(preparation: &Preparation) -> usize {
        preparation
            .working_set()
            .resources
            .iter()
            .map(|class| match *class {
                ResourceClass::Buffer { count, .. } | ResourceClass::Image { count, .. } => count,
            })
            .sum()
    }

    pub struct Readbacks {
        pub sdr: PassHandle,
        pub depth: Option<PassHandle>,
        #[cfg(test)]
        pub shadow_maps: [Option<PassHandle>; CASCADE_COUNT],
    }

    pub fn build_graph(
        pool: &mut GraphPool,
        prepared: &mut Prepared,
        triangle: &Triangle,
        defaults: Option<&DefaultResources>,
        post: &PostProcess,
        placement: Placement,
        shadow_history: Option<(&ShadowHistory, [u64; CASCADE_COUNT])>,
    ) -> Result<(CompiledGraph, Readbacks, Option<ShadowUpdates>), GraphError> {
        let mut graph = pool.begin()?;
        let defaults = match defaults {
            Some(defaults) => defaults.import(&mut graph)?,
            None => DefaultViews::upload(&mut graph)?,
        };
        let (indexed, materials) = triangle.upload(&mut graph, &defaults)?;
        let single = [Draw::Indexed(indexed)];
        #[cfg(test)]
        let (test_draws, transforms) = placed_draws(indexed, placement)?;
        let draws = match placement {
            Placement::Single => &single[..],
            #[cfg(test)]
            Placement::Individual => &test_draws[..],
            #[cfg(test)]
            Placement::Instanced | Placement::Tilted(Some(_)) => &test_draws[..1],
            #[cfg(test)]
            Placement::Tilted(None) => &single[..],
        };
        let view = prepared.prepare_view(
            Camera {
                position: [0.0, 0.0, 3.0],
                target: [0.0; 3],
                fov_y_degrees: 55.0,
                near: 0.1,
                far: 10.0,
            },
            Sun::new([0.0, 0.0, 1.0], [1.5; 3])?,
            Bounds::new([-1.0, -0.7, -0.01], [1.0, 0.7, 0.01])?,
            defaults.environment(),
        )?;
        let shadows = shadow_history
            .map(|(history, revisions)| history.begin_view(&mut graph, &view, revisions))
            .transpose()?;
        let mut frame = Frame::new(&view, &materials, draws, &[], draws);
        #[cfg(test)]
        if placement.uses_transforms() {
            frame.transforms = &transforms;
        }
        if let Some(shadows) = &shadows {
            frame.shadows =
                renderer_pipeline::pbr::FrameShadows::Cascades(std::array::from_fn(|cascade| {
                    shadows.source(cascade, draws)
                }));
        }
        let (sdr, scene_depth, shadow_images) = if post.copy_hdr || post.bloom.is_some() {
            let scene = prepared.add_scene(&mut graph, frame)?;
            let mut hdr = scene.hdr;
            if post.copy_hdr {
                let target = graph.image("copied HDR", hdr_desc())?;
                graph.copy_image(COPY_PASS, hdr, target)?;
                hdr = target;
            }
            if let Some(bloom) = &post.bloom {
                hdr = bloom.add_passes(&mut graph, hdr)?;
            }
            (
                prepared.add_output(&mut graph, hdr, 1.0)?.sdr,
                scene.depth,
                scene.shadows,
            )
        } else {
            let images = prepared.add_passes(&mut graph, frame)?;
            (images.sdr, images.depth, images.shadows)
        };
        #[cfg(test)]
        let mut shadow_maps = [None; CASCADE_COUNT];
        #[cfg(test)]
        if placement != Placement::Single {
            for (target, image) in shadow_maps
                .iter_mut()
                .zip(shadow_images.expect("shadow setup"))
            {
                *target = Some(graph.readback_image("read cascade", image)?);
            }
        }
        #[cfg(not(test))]
        let _ = shadow_images;
        let readbacks = Readbacks {
            sdr: graph.readback_image("read SDR", sdr)?,
            depth: scene_depth
                .map(|depth| graph.readback_image("read depth", depth))
                .transpose()?,
            #[cfg(test)]
            shadow_maps,
        };
        let updates = shadows
            .map(|shadows| shadows.export(&mut graph))
            .transpose()?;
        Ok((graph.compile()?, readbacks, updates))
    }

    #[cfg(test)]
    fn placed_draws(
        indexed: IndexedDraw,
        placement: Placement,
    ) -> Result<([Draw; 2], [Transform; 2]), GraphError> {
        if let Placement::Tilted(Some(exponent)) = placement {
            let scale = 2.0_f32.powi(exponent);
            let transform = Transform::new([
                [scale, 0.0, 0.0, 0.0],
                [0.0, scale, 0.0, 0.0],
                [0.0, 0.0, scale, 0.0],
            ])?;
            let draw = Draw::Instanced {
                geometry: indexed,
                first_transform: 0,
                instance_count: 1,
            };
            return Ok(([draw; 2], [transform, Transform::IDENTITY]));
        }
        let place = |x| {
            Transform::new([
                [0.5, 0.0, 0.0, x],
                [0.0, 0.65, 0.0, 0.0],
                [0.0, 0.0, 0.5, 0.0],
            ])
        };
        Ok((
            [
                Draw::Instanced {
                    geometry: indexed,
                    first_transform: 0,
                    instance_count: if placement == Placement::Instanced {
                        2
                    } else {
                        1
                    },
                },
                Draw::Instanced {
                    geometry: indexed,
                    first_transform: 1,
                    instance_count: 1,
                },
            ],
            [place(-0.45)?, place(0.45)?],
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use renderer::graph::GraphCapacity;

        #[test]
        fn application_copy_is_between_shading_and_sdr_conversion() {
            let triangle = Triangle::new(false, Placement::Single).unwrap();
            let mut prepared = prepare(NonZeroUsize::MIN, Settings::new(SampleCount::X1)).unwrap();
            for (copy_hdr, bloom) in [(false, Bloom::Off), (true, Bloom::Off), (true, Bloom::Tent)]
            {
                let (preparation, post) = reserve(&prepared, &triangle, copy_hdr, bloom).unwrap();
                assert!(resource_count(&preparation) >= 10);
                let capacity = preparation
                    .graph_capacity(GraphCapacity::default())
                    .unwrap();
                let mut pool = GraphPool::new(capacity, 1).unwrap();
                let (compiled, readbacks, _) = build_graph(
                    &mut pool,
                    &mut prepared,
                    &triangle,
                    None,
                    &post,
                    Placement::Single,
                    None,
                )
                .unwrap();
                let depth = readbacks.depth.expect("1× exposes scene depth");
                assert_ne!(readbacks.sdr, depth);
                let passes = compiled.ordered_pass_names().collect::<Vec<_>>();
                let tone_map = passes.iter().position(|&name| name == "tone_map").unwrap();
                let write_sdr = passes.iter().position(|&name| name == "write_sdr").unwrap();
                assert!(tone_map < write_sdr);
                if copy_hdr {
                    let position = passes.iter().position(|&name| name == COPY_PASS).unwrap();
                    assert!(position < tone_map);
                    if bloom != Bloom::Off {
                        let combine = passes
                            .iter()
                            .position(|&name| name == "Bloom combine")
                            .unwrap();
                        assert!(position < combine && combine < tone_map);
                    }
                } else {
                    assert!(!passes.contains(&COPY_PASS));
                }
                assert!(passes.contains(&"read SDR") && passes.contains(&"read depth"));
            }
        }

        #[test]
        fn four_samples_resolve_color_without_a_depth_readback() {
            let triangle = Triangle::new(false, Placement::Single).unwrap();
            let mut prepared = prepare(NonZeroUsize::MIN, Settings::new(SampleCount::X4)).unwrap();
            let (preparation, post) = reserve(&prepared, &triangle, false, Bloom::Off).unwrap();
            let capacity = preparation
                .graph_capacity(GraphCapacity::default())
                .unwrap();
            let mut pool = GraphPool::new(capacity, 1).unwrap();
            let (compiled, readbacks, _) = build_graph(
                &mut pool,
                &mut prepared,
                &triangle,
                None,
                &post,
                Placement::Single,
                None,
            )
            .unwrap();
            assert!(compiled.ordered_pass_names().any(|name| name == "read SDR"));
            assert!(readbacks.depth.is_none());
        }

        #[test]
        fn transformed_scene_declares_all_cascades() {
            for placement in [Placement::Individual, Placement::Tilted(Some(-40))] {
                let settings = Settings {
                    samples: SampleCount::X1,
                    placement,
                };
                let triangle = Triangle::new(false, placement).unwrap();
                let mut prepared = prepare(NonZeroUsize::MIN, settings).unwrap();
                let (preparation, post) = reserve(&prepared, &triangle, false, Bloom::Off).unwrap();
                let capacity = preparation
                    .graph_capacity(GraphCapacity::default())
                    .unwrap();
                let mut pool = GraphPool::new(capacity, 1).unwrap();
                let (compiled, readbacks, _) = build_graph(
                    &mut pool,
                    &mut prepared,
                    &triangle,
                    None,
                    &post,
                    placement,
                    None,
                )
                .unwrap();
                assert!(readbacks.shadow_maps.iter().all(Option::is_some));
                assert_eq!(
                    compiled
                        .ordered_pass_names()
                        .filter(|name| name.starts_with("shadow_"))
                        .count(),
                    CASCADE_COUNT,
                );
            }
        }
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
mod native {
    use std::{
        error::Error,
        fs::File,
        io::{BufWriter, Write},
        num::NonZeroUsize,
        path::{Path, PathBuf},
        time::Duration,
    };

    use renderer::{
        graph::{GraphCapacity, GraphPool, SampleCount},
        runtime::{Runtime, RuntimeConfig, RuntimeLimits},
    };
    use renderer_pipeline::{
        passes::bloom::Bloom,
        pbr::{CASCADE_COUNT, DefaultResources, ShadowHistory},
    };

    #[cfg(test)]
    use super::scene::Placement;
    use super::scene::{self, EXTENT, Settings, Triangle};

    #[derive(Clone, Copy, Debug)]
    enum Api {
        #[cfg(feature = "vulkan")]
        Vulkan,
        #[cfg(all(windows, feature = "d3d12"))]
        D3D12,
    }

    impl Api {
        fn parse(name: &str) -> Result<Self, Box<dyn Error>> {
            match name {
                #[cfg(feature = "vulkan")]
                "vulkan" => Ok(Self::Vulkan),
                #[cfg(all(windows, feature = "d3d12"))]
                "d3d12" => Ok(Self::D3D12),
                _ => Err(format!("requested backend {name:?} is unavailable in this build").into()),
            }
        }

        fn config(self) -> RuntimeConfig {
            match self {
                #[cfg(feature = "vulkan")]
                Self::Vulkan => RuntimeConfig::Vulkan(Default::default()),
                #[cfg(all(windows, feature = "d3d12"))]
                Self::D3D12 => RuntimeConfig::D3D12(Default::default()),
            }
        }
    }

    #[derive(Debug, Eq, PartialEq)]
    struct Output {
        sdr: Vec<u8>,
        depth: Option<Vec<u8>>,
        #[cfg(test)]
        shadow_maps: [Vec<u8>; CASCADE_COUNT],
    }

    struct CapturedFrame {
        output: Output,
        reused: [bool; CASCADE_COUNT],
        #[cfg(test)]
        shadow_passes: usize,
    }

    fn render(
        api: Api,
        settings: Settings,
        copy_hdr: bool,
        bloom: Bloom,
        caster_revisions: &[[u64; CASCADE_COUNT]],
    ) -> Result<Vec<CapturedFrame>, Box<dyn Error>> {
        let frames = caster_revisions.len().max(1);
        // Give enabled Bloom an emitter above its scene-linear threshold.
        let triangle = Triangle::new(bloom != Bloom::Off, settings.placement)?;
        let mut prepared = scene::prepare(NonZeroUsize::new(frames).unwrap(), settings)?;
        let (preparation, post) = scene::reserve(&prepared, &triangle, copy_hdr, bloom)?;
        // Graph declarations include initialization uploads and readbacks in
        // this example. The WorkingSet reserves physical resources per slot.
        let graph = preparation.graph_capacity(GraphCapacity::default())?;
        let mut pool = GraphPool::new(graph, frames)?;
        let mut runtime = Runtime::new(
            api.config(),
            RuntimeLimits {
                graph,
                frames,
                results: frames,
                resource_capacity: scene::resource_count(&preparation),
                ..RuntimeLimits::default()
            },
            preparation.working_set(),
        )?;
        #[cfg(feature = "vulkan")]
        if let Some(vulkan) = runtime.as_vulkan()
            && vulkan.device_info().device_type == "CPU"
        {
            return Err("the Vulkan example requires a hardware adapter".into());
        }
        // Upload the shared 1×1 images at startup and import them each frame.
        let (mut defaults, initialize) = DefaultResources::initialize(&mut runtime)?;
        drop(
            runtime
                .wait_for_frame(initialize, Duration::from_secs(10))?
                .ok_or("default texture uploads timed out")?,
        );
        let [width, height] = EXTENT.map(|dimension| dimension as usize);
        let mut captured: Vec<_> = (0..frames)
            .map(|_| CapturedFrame {
                output: Output {
                    sdr: vec![0; width * height * 4],
                    depth: (settings.samples == SampleCount::X1)
                        .then(|| vec![0; width * height * 4]),
                    #[cfg(test)]
                    shadow_maps: std::array::from_fn(|_| {
                        if settings.placement == Placement::Single {
                            Vec::new()
                        } else {
                            vec![0; (scene::SHADOW_RESOLUTION as usize).pow(2) * 4]
                        }
                    }),
                },
                reused: [false; CASCADE_COUNT],
                #[cfg(test)]
                shadow_passes: 0,
            })
            .collect();
        let mut pending = Vec::with_capacity(frames);
        let mut history = ShadowHistory::default();
        for (index, captured) in captured.iter_mut().enumerate() {
            let shadow_history = caster_revisions.get(index).map(|&keys| (&history, keys));
            let (compiled, readbacks, updates) = scene::build_graph(
                &mut pool,
                &mut prepared,
                &triangle,
                Some(&defaults),
                &post,
                settings.placement,
                shadow_history,
            )?;
            #[cfg(test)]
            {
                captured.shadow_passes = compiled
                    .ordered_pass_names()
                    .filter(|name| name.starts_with("shadow_"))
                    .count();
            }
            let submission = if let Some(mut updates) = updates {
                captured.reused = updates.reused();
                history.submit(&mut runtime, &compiled, &mut updates)?
            } else {
                runtime.submit(&compiled)?
            };
            // Accepted snapshots are available to the next Graph immediately.
            pending.push((submission.id(), readbacks));
        }
        for (captured, (id, readbacks)) in captured.iter_mut().zip(pending) {
            let mut completed = runtime
                .wait_for_frame(id, Duration::from_secs(10))?
                .ok_or("GPU completion timed out")?;
            completed.readback_into(readbacks.sdr, &mut captured.output.sdr)?;
            if let Some(depth) = readbacks.depth {
                let output = captured
                    .output
                    .depth
                    .as_mut()
                    .ok_or("unexpected depth readback")?;
                completed.readback_into(depth, output)?;
            }
            #[cfg(test)]
            for (pass, output) in readbacks
                .shadow_maps
                .into_iter()
                .zip(&mut captured.output.shadow_maps)
            {
                if let Some(pass) = pass {
                    completed.readback_into(pass, output)?;
                }
            }
        }
        history.clear(&mut runtime)?;
        defaults.clear(&mut runtime)?;
        check_validation(runtime)?;
        Ok(captured)
    }

    fn check_validation(runtime: Runtime) -> Result<(), Box<dyn Error>> {
        let strict = cfg!(feature = "development-validation");
        #[cfg(feature = "vulkan")]
        if let Some(vulkan) = runtime.as_vulkan() {
            let monitor = vulkan.validation_monitor();
            drop(runtime);
            let report = monitor.report();
            if !report.is_clean()
                || (strict
                    && (report.core_validation != Some(true)
                        || report.synchronization_validation != Some(true)))
            {
                return Err(format!("unexpected Vulkan validation report {report:#?}").into());
            }
            return Ok(());
        }
        #[cfg(all(windows, feature = "d3d12"))]
        if let Some(d3d12) = runtime.as_d3d12() {
            let monitor = d3d12.validation_monitor();
            drop(runtime);
            let report = monitor.snapshot();
            if !report.is_clean()
                || report.debug_layer_enabled != strict
                || report.gpu_based_validation_enabled != strict
                || (strict && !report.teardown_checked)
            {
                return Err(format!("unexpected D3D12 validation report {report:#?}").into());
            }
            return Ok(());
        }
        unreachable!("a Runtime has a compiled backend");
    }

    fn probe(output: &Output) -> Result<([u8; 4], Option<f32>), Box<dyn Error>> {
        let [width, height] = EXTENT.map(|dimension| dimension as usize);
        if output.sdr.len() != width * height * 4
            || output
                .depth
                .as_ref()
                .is_some_and(|depth| depth.len() != width * height * 4)
        {
            return Err("unexpected SDR or depth readback size".into());
        }
        let center = (height / 2 * width + width / 2) * 4;
        let rgba: [u8; 4] = output.sdr[center..center + 4].try_into().unwrap();
        let depth = output
            .depth
            .as_ref()
            .map(|bytes| f32::from_le_bytes(bytes[center..center + 4].try_into().unwrap()));
        if !matches!(rgba, [r, g, b, 255] if r > 80 && r > g && r > b)
            || depth.is_some_and(|depth| !(0.0..1.0).contains(&depth))
            || output.sdr[..4] != [0, 0, 0, 255]
            || output
                .depth
                .as_ref()
                .is_some_and(|bytes| bytes[..4] != 1.0_f32.to_le_bytes())
        {
            return Err(format!(
                "unexpected PBR output {rgba:?}, depth {depth:?}, corner {:?}",
                &output.sdr[..4]
            )
            .into());
        }
        Ok((rgba, depth))
    }

    fn write_ppm(path: &Path, rgba: &[u8]) -> Result<(), std::io::Error> {
        let [width, height] = EXTENT;
        let mut file = BufWriter::new(File::create(path)?);
        write!(file, "P6\n{width} {height}\n255\n")?;
        for pixel in rgba.as_chunks::<4>().0 {
            file.write_all(&pixel[..3])?;
        }
        file.flush()
    }

    pub fn run() -> Result<(), Box<dyn Error>> {
        let mut args = std::env::args_os().skip(1);
        let mut api = None;
        let mut output = None;
        let mut copy_hdr = false;
        let mut samples = SampleCount::X1;
        let mut bloom = Bloom::Off;
        let mut reuse_shadows = false;
        while let Some(argument) = args.next() {
            match argument.to_str() {
                Some("--help" | "-h") => {
                    println!(
                        "pbr_forward [--api vulkan|d3d12] [--msaa 1|4] [--copy-hdr] [--bloom] [--reuse-shadows] [--output FILE.ppm]"
                    );
                    return Ok(());
                }
                Some("--api") => {
                    let name = args.next().ok_or("expected an API name")?;
                    api = Some(Api::parse(name.to_str().ok_or("API name must be UTF-8")?)?);
                }
                Some("--copy-hdr") => copy_hdr = true,
                Some("--msaa") => {
                    samples = match args.next().as_deref().and_then(|value| value.to_str()) {
                        Some("1") => SampleCount::X1,
                        Some("4") => SampleCount::X4,
                        _ => return Err("--msaa requires 1 or 4".into()),
                    };
                }
                Some("--bloom") => bloom = Bloom::Tent,
                Some("--reuse-shadows") => reuse_shadows = true,
                Some("--output") => {
                    output = Some(PathBuf::from(args.next().ok_or("expected an output path")?))
                }
                _ => return Err(format!("unknown argument {argument:?}").into()),
            }
        }
        #[cfg(feature = "vulkan")]
        let default = Api::Vulkan;
        #[cfg(all(not(feature = "vulkan"), windows, feature = "d3d12"))]
        let default = Api::D3D12;
        let api = api.unwrap_or(default);
        let revisions = [[0; CASCADE_COUNT]; 2];
        let revisions = if reuse_shadows { &revisions[..] } else { &[] };
        let rendered = render(api, Settings::new(samples), copy_hdr, bloom, revisions)?;
        if reuse_shadows {
            for (index, frame) in rendered.iter().enumerate() {
                let count = frame.reused.into_iter().filter(|&hit| hit).count();
                println!(
                    "Frame {} reused {count}/{CASCADE_COUNT} shadow maps",
                    index + 1
                );
            }
        }
        let output_frame = &rendered.last().unwrap().output;
        let (rgba, depth) = probe(output_frame)?;
        if let Some(path) = output {
            write_ppm(&path, &output_frame.sdr)?;
        }
        if let Some(depth) = depth {
            println!("{api:?} center SDR {rgba:?} and depth {depth:.6}");
        } else {
            println!("{api:?} center SDR {rgba:?} after 4× MSAA resolve");
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn compare(api: Api) {
            for samples in [SampleCount::X1, SampleCount::X4] {
                let settings = Settings::new(samples);
                let full = render(api, settings, false, Bloom::Off, &[]).unwrap();
                let split = render(api, settings, true, Bloom::Off, &[]).unwrap();
                let (_, depth) = probe(&full[0].output).unwrap();
                assert_eq!(depth.is_some(), samples == SampleCount::X1);
                assert_eq!(full[0].output, split[0].output);
            }
        }

        fn bloom_and_history(api: Api) {
            let draws = render(
                api,
                Settings::new(SampleCount::X1),
                true,
                Bloom::Tent,
                &[[0; 4], [0; 4], [0, 0, 1, 0]],
            )
            .unwrap();
            assert_eq!(draws[0].reused, [false; 4]);
            assert_eq!(draws[1].reused, [true; 4]);
            assert_eq!(draws[2].reused, [true, true, false, true]);
            assert_eq!(draws[0].shadow_passes, 4);
            assert_eq!(draws[1].shadow_passes, 0);
            assert_eq!(draws[2].shadow_passes, 1);
            assert_eq!(draws[0].output, draws[1].output);
            assert_eq!(draws[0].output, draws[2].output);

            let image = &draws[0].output;
            probe(image).unwrap();
            // The HDR emitter spreads color into at least one background pixel.
            assert!(
                image
                    .sdr
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(image.depth.as_ref().unwrap().as_chunks::<4>().0)
                    .any(|(effect, depth)| *depth == 1.0_f32.to_le_bytes()
                        && effect[0] > 0
                        && effect[3] == 255)
            );
        }

        fn transformed_instances_match_individual_draws(api: Api) {
            let render = |placement| {
                let settings = Settings {
                    samples: SampleCount::X1,
                    placement,
                };
                render(api, settings, false, Bloom::Off, &[]).unwrap()
            };
            let individual = render(Placement::Individual);
            let instanced = render(Placement::Instanced);
            let output = &instanced[0].output;
            assert_eq!(individual[0].output, *output);

            // Both objects must contribute distinct covered pixels.
            let depth = output.depth.as_ref().unwrap();
            for x in [25, 39] {
                let offset = (26 * EXTENT[0] as usize + x) * 4;
                let rgba: [u8; 4] = output.sdr[offset..offset + 4].try_into().unwrap();
                assert!(matches!(rgba, [r, g, b, 255] if r > 80 && r > g && r > b));
                let z = f32::from_le_bytes(depth[offset..offset + 4].try_into().unwrap());
                assert!((0.0..1.0).contains(&z));
            }
            assert!(output.shadow_maps.iter().any(|image| {
                image
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| *pixel != 1.0_f32.to_le_bytes())
            }));
        }

        fn extreme_scales_preserve_normal_direction(api: Api) {
            let capture = |placement| {
                let settings = Settings {
                    samples: SampleCount::X1,
                    placement,
                };
                render(api, settings, false, Bloom::Off, &[])
                    .unwrap()
                    .pop()
                    .unwrap()
                    .output
            };
            let center = ((EXTENT[1] / 2) * EXTENT[0] + EXTENT[0] / 2) as usize * 4;
            let upright = capture(Placement::Single);
            let reference = capture(Placement::Tilted(None));
            assert!(reference.sdr[center] > 60);
            assert!(reference.sdr[center].abs_diff(upright.sdr[center]) > 8);

            // Inverse powers of two preserve the world-space vertex positions
            // exactly while the transformed normal crosses the length limits.
            for exponent in [-40, 40] {
                let scaled = capture(Placement::Tilted(Some(exponent)));
                assert_eq!(reference.depth, scaled.depth, "scale 2^{exponent}");
                assert_eq!(
                    reference.shadow_maps, scaled.shadow_maps,
                    "scale 2^{exponent}"
                );
                let expected = &reference.sdr[center..center + 4];
                let actual = &scaled.sdr[center..center + 4];
                assert!(
                    expected
                        .iter()
                        .zip(actual)
                        .all(|(&a, &b)| a.abs_diff(b) <= 1),
                    "scale 2^{exponent}: expected {expected:?}, got {actual:?}",
                );
            }
        }

        #[cfg(feature = "vulkan")]
        #[test]
        #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
        fn vulkan_full_and_split_recipes_match() {
            compare(Api::Vulkan);
        }

        #[cfg(all(windows, feature = "d3d12"))]
        #[test]
        #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
        fn d3d12_full_and_split_recipes_match() {
            compare(Api::D3D12);
        }

        #[cfg(feature = "vulkan")]
        #[test]
        #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
        fn vulkan_bloom_and_shadow_history() {
            bloom_and_history(Api::Vulkan);
        }

        #[cfg(all(windows, feature = "d3d12"))]
        #[test]
        #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
        fn d3d12_bloom_and_shadow_history() {
            bloom_and_history(Api::D3D12);
        }

        #[cfg(feature = "vulkan")]
        #[test]
        #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
        fn vulkan_transformed_casters_match_instancing() {
            transformed_instances_match_individual_draws(Api::Vulkan);
        }

        #[cfg(all(windows, feature = "d3d12"))]
        #[test]
        #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
        fn d3d12_transformed_casters_match_instancing() {
            transformed_instances_match_individual_draws(Api::D3D12);
        }

        #[cfg(feature = "vulkan")]
        #[test]
        #[ignore = "requires a hardware Vulkan adapter and requested native validation"]
        fn vulkan_extreme_scales_preserve_normal_direction() {
            extreme_scales_preserve_normal_direction(Api::Vulkan);
        }

        #[cfg(all(windows, feature = "d3d12"))]
        #[test]
        #[ignore = "requires a hardware D3D12 adapter and requested native validation"]
        fn d3d12_extreme_scales_preserve_normal_direction() {
            extreme_scales_preserve_normal_direction(Api::D3D12);
        }
    }
}

#[cfg(any(feature = "vulkan", all(windows, feature = "d3d12")))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    native::run()
}

#[cfg(not(any(feature = "vulkan", all(windows, feature = "d3d12"))))]
fn main() -> Result<(), &'static str> {
    Err("compile the example with the vulkan feature or, on Windows, the d3d12 feature")
}

//! Draw a small offscreen scene with the public PBR Forward recipe.
//! Use `--copy-hdr` to compose a custom Graph pass before SDR conversion.
//! Use `--bloom` to add Bloom and `--reuse-shadows` to draw two frames using
//! the first frame's accepted shadow maps.

#[cfg(any(test, feature = "vulkan", all(windows, feature = "d3d12")))]
mod scene {
    use std::num::NonZeroUsize;

    use renderer::{
        capacity::{Preparation, ResourceClass},
        geometry::GeometryData,
        graph::{
            BufferDesc, CompiledGraph, Graph, GraphError, GraphPool, ImageDesc, ImageView,
            PassHandle,
        },
    };
    use renderer_pipeline::{
        passes::{
            bloom::{Bloom, PreparedBloom},
            output::{SdrEncoding, SdrOutput},
            pbr::{MATERIAL_BYTES, VERTEX_BYTES},
        },
        pbr_forward::{
            self, Bounds, CASCADE_COUNT, Camera, Config, Draw, Environment, Frame, Material,
            MaterialFactors, Prepared, SceneLimits, ShadowHistory, ShadowSettings, ShadowUpdates,
            Sun,
        },
    };

    pub const EXTENT: [u32; 2] = [64, 48];
    const SHADOW_RESOLUTION: u32 = 64;
    const COPY_PASS: &str = "application HDR copy";

    pub struct Triangle {
        geometry: GeometryData,
        material: [u8; MATERIAL_BYTES],
    }

    impl Triangle {
        pub fn new(glowing: bool) -> Result<Self, GraphError> {
            let vertices = [
                ([-0.8_f32, -0.7, 0.0], [0.0, 1.0]),
                ([0.8, -0.7, 0.0], [1.0, 1.0]),
                ([0.0, 0.7, 0.0], [0.5, 0.0]),
            ]
            .into_iter()
            .flat_map(|(position, uv)| {
                position
                    .into_iter()
                    .chain([0.0, 0.0, 1.0]) // normal
                    .chain([1.0, 0.0, 0.0, 1.0]) // tangent and handedness
                    .chain(uv)
                    .flat_map(f32::to_le_bytes)
            })
            .collect::<Vec<_>>();
            Ok(Self {
                geometry: GeometryData::new(VERTEX_BYTES as u32, &vertices, &[0, 1, 2])?,
                material: MaterialFactors {
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
            let [linear, srgb, environment] = pixel_descs();
            preparation.reserve_image(linear, 2)?;
            preparation.reserve_image(srgb, 2)?;
            preparation.reserve_image(environment, 3)?;
            if copy_hdr {
                preparation.reserve_image(hdr_desc(), 1)?;
            }
            Ok(())
        }

        fn upload(
            &self,
            graph: &mut Graph,
        ) -> Result<([Draw; 1], [Material; 1], Environment), GraphError> {
            let [vertex_desc, index_desc, material_desc] = self.buffer_descs();
            let vertices = graph.buffer("triangle vertices", vertex_desc)?;
            let indices = graph.buffer("triangle indices", index_desc)?;
            let factors = graph.buffer("triangle material factors", material_desc)?;
            self.geometry
                .upload_vertices(graph, "upload vertices", vertices)?;
            self.geometry
                .upload_indices(graph, "upload indices", indices)?;
            graph.upload_buffer("upload material factors", factors, 0, &self.material)?;

            let [linear, srgb, environment] = pixel_descs();
            let textures = [
                upload_pixel(graph, "base color", srgb, &[220, 32, 24, 255])?,
                upload_pixel(graph, "normal", linear, &[128, 128, 255, 255])?,
                upload_pixel(
                    graph,
                    "occlusion roughness metallic",
                    linear,
                    &[255, 180, 0, 255],
                )?,
                upload_pixel(graph, "emissive", srgb, &[255, 0, 0, 255])?,
            ];
            let ibl = half_pixel([0x3400, 0x3400, 0x3400, 0x3c00]);
            let brdf = half_pixel([0x3800, 0x3000, 0, 0x3c00]);
            let environment = Environment {
                irradiance: upload_pixel(graph, "IBL diffuse", environment, &ibl)?,
                reflection: upload_pixel(graph, "IBL specular", environment, &ibl)?,
                brdf_lut: upload_pixel(graph, "IBL BRDF LUT", environment, &brdf)?,
                strength: 0.2,
                yaw_radians: 0.0,
                max_prefilter_lod: 0,
            };
            Ok((
                [Draw {
                    vertices,
                    indices,
                    first_index: 0,
                    index_count: self.geometry.index_count(),
                    material: 0,
                }],
                [Material {
                    factors,
                    factor_offset: 0,
                    textures,
                }],
                environment,
            ))
        }
    }

    fn upload_pixel(
        graph: &mut Graph,
        name: &str,
        desc: ImageDesc,
        bytes: &[u8],
    ) -> Result<ImageView, GraphError> {
        let image = graph.image(name, desc)?;
        graph.upload_image(name, image, bytes)?;
        graph.image_view(image, 0, 0)
    }

    fn half_pixel(components: [u16; 4]) -> [u8; 8] {
        let [r, g, b, a] = components.map(u16::to_le_bytes);
        [r[0], r[1], g[0], g[1], b[0], b[1], a[0], a[1]]
    }

    fn pixel_descs() -> [ImageDesc; 3] {
        [
            ImageDesc::rgba8(1, 1),
            ImageDesc::rgba8_srgb(1, 1),
            ImageDesc::rgba16_float(1, 1),
        ]
    }

    fn hdr_desc() -> ImageDesc {
        ImageDesc::rgba16_float(EXTENT[0], EXTENT[1])
    }

    pub fn prepare(frames_in_flight: NonZeroUsize) -> Result<Prepared, GraphError> {
        pbr_forward::prepare(
            Config {
                extent: EXTENT,
                clear_color: [0.0, 0.0, 0.0, 1.0],
                shadows: ShadowSettings {
                    resolution: SHADOW_RESOLUTION,
                    ..ShadowSettings::default()
                },
                output: SdrOutput {
                    encoding: SdrEncoding::Srgb,
                    fxaa: false,
                },
            },
            SceneLimits {
                frames_in_flight,
                max_draws: 1,
                max_materials: 1,
                max_casters_per_cascade: 1,
            },
        )
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
        pub depth: PassHandle,
    }

    pub fn build_graph(
        pool: &mut GraphPool,
        prepared: &mut Prepared,
        triangle: &Triangle,
        post: &PostProcess,
        shadow_history: Option<(&ShadowHistory, [u64; CASCADE_COUNT])>,
    ) -> Result<(CompiledGraph, Readbacks, Option<ShadowUpdates>), GraphError> {
        let mut graph = pool.begin()?;
        let (draws, materials, environment) = triangle.upload(&mut graph)?;
        let view = prepared.prepare_view(
            Camera {
                position: [0.0, 0.0, 3.0],
                target: [0.0; 3],
                fov_y_degrees: 55.0,
                near: 0.1,
                far: 10.0,
            },
            Sun::new([0.0, 0.0, 1.0], [1.5; 3])?,
            Bounds::new([-0.8, -0.7, -0.01], [0.8, 0.7, 0.01])?,
            environment,
        )?;
        let shadows = shadow_history
            .map(|(history, revisions)| history.begin_view(&mut graph, &view, revisions))
            .transpose()?;
        let mut frame = Frame::new(&view, &materials, &draws, &[], &draws);
        if let Some(shadows) = &shadows {
            frame.shadows = std::array::from_fn(|cascade| shadows.source(cascade, &draws));
        }
        let (sdr, depth) = if post.copy_hdr || post.bloom.is_some() {
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
            (prepared.add_output(&mut graph, hdr, 1.0)?.sdr, scene.depth)
        } else {
            let images = prepared.add_passes(&mut graph, frame)?;
            (images.sdr, images.depth)
        };
        let readbacks = Readbacks {
            sdr: graph.readback_image("read SDR", sdr)?,
            depth: graph.readback_image("read depth", depth)?,
        };
        let updates = shadows
            .map(|shadows| shadows.export(&mut graph))
            .transpose()?;
        Ok((graph.compile()?, readbacks, updates))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use renderer::graph::GraphCapacity;

        #[test]
        fn application_copy_is_between_shading_and_sdr_conversion() {
            let triangle = Triangle::new(false).unwrap();
            let mut prepared = prepare(NonZeroUsize::MIN).unwrap();
            for (copy_hdr, bloom) in [(false, Bloom::Off), (true, Bloom::Off), (true, Bloom::Tent)]
            {
                let (preparation, post) = reserve(&prepared, &triangle, copy_hdr, bloom).unwrap();
                assert!(resource_count(&preparation) >= 10);
                let capacity = preparation
                    .graph_capacity(GraphCapacity::default())
                    .unwrap();
                let mut pool = GraphPool::new(capacity, 1).unwrap();
                let (compiled, readbacks, _) =
                    build_graph(&mut pool, &mut prepared, &triangle, &post, None).unwrap();
                assert_ne!(readbacks.sdr, readbacks.depth);
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
        graph::{GraphCapacity, GraphPool},
        runtime::{Runtime, RuntimeConfig, RuntimeLimits},
    };
    use renderer_pipeline::{
        passes::bloom::Bloom,
        pbr_forward::{CASCADE_COUNT, ShadowHistory},
    };

    use super::scene::{self, EXTENT, Triangle};

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
        depth: Vec<u8>,
    }

    struct CapturedFrame {
        output: Output,
        reused: [bool; CASCADE_COUNT],
        #[cfg(test)]
        shadow_passes: usize,
    }

    fn render(
        api: Api,
        copy_hdr: bool,
        bloom: Bloom,
        caster_revisions: &[[u64; CASCADE_COUNT]],
    ) -> Result<Vec<CapturedFrame>, Box<dyn Error>> {
        let frames = caster_revisions.len().max(1);
        // Give enabled Bloom an emitter above its scene-linear threshold.
        let triangle = Triangle::new(bloom != Bloom::Off)?;
        let mut prepared = scene::prepare(NonZeroUsize::new(frames).unwrap())?;
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
        let [width, height] = EXTENT.map(|dimension| dimension as usize);
        let mut captured: Vec<_> = (0..frames)
            .map(|_| CapturedFrame {
                output: Output {
                    sdr: vec![0; width * height * 4],
                    depth: vec![0; width * height * 4],
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
            let (compiled, readbacks, updates) =
                scene::build_graph(&mut pool, &mut prepared, &triangle, &post, shadow_history)?;
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
            completed.readback_into(readbacks.depth, &mut captured.output.depth)?;
        }
        history.clear(&mut runtime)?;
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

    fn probe(output: &Output) -> Result<([u8; 4], f32), Box<dyn Error>> {
        let [width, height] = EXTENT.map(|dimension| dimension as usize);
        if output.sdr.len() != width * height * 4 || output.depth.len() != width * height * 4 {
            return Err("unexpected SDR or depth readback size".into());
        }
        let center = (height / 2 * width + width / 2) * 4;
        let rgba: [u8; 4] = output.sdr[center..center + 4].try_into().unwrap();
        let depth = f32::from_le_bytes(output.depth[center..center + 4].try_into().unwrap());
        if !matches!(rgba, [r, g, b, 255] if r > 80 && r > g && r > b)
            || !(0.0..1.0).contains(&depth)
            || output.sdr[..4] != [0, 0, 0, 255]
            || output.depth[..4] != 1.0_f32.to_le_bytes()
        {
            return Err(format!(
                "unexpected PBR output {rgba:?}, depth {depth}, corner {:?}",
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
        let mut bloom = Bloom::Off;
        let mut reuse_shadows = false;
        while let Some(argument) = args.next() {
            match argument.to_str() {
                Some("--help" | "-h") => {
                    println!(
                        "pbr_forward [--api vulkan|d3d12] [--copy-hdr] [--bloom] [--reuse-shadows] [--output FILE.ppm]"
                    );
                    return Ok(());
                }
                Some("--api") => {
                    let name = args.next().ok_or("expected an API name")?;
                    api = Some(Api::parse(name.to_str().ok_or("API name must be UTF-8")?)?);
                }
                Some("--copy-hdr") => copy_hdr = true,
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
        let rendered = render(api, copy_hdr, bloom, revisions)?;
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
        println!("{api:?} center SDR {rgba:?} and depth {depth:.6}");
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn compare(api: Api) {
            let full = render(api, false, Bloom::Off, &[]).unwrap();
            let split = render(api, true, Bloom::Off, &[]).unwrap();
            probe(&full[0].output).unwrap();
            assert_eq!(full[0].output, split[0].output);
        }

        fn bloom_and_history(api: Api) {
            let draws = render(api, true, Bloom::Tent, &[[0; 4], [0; 4], [0, 0, 1, 0]]).unwrap();
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
                    .zip(image.depth.as_chunks::<4>().0)
                    .any(|(effect, depth)| *depth == 1.0_f32.to_le_bytes()
                        && effect[0] > 0
                        && effect[3] == 255)
            );
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

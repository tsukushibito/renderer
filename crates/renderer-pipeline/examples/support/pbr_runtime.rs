use std::{error::Error, num::NonZeroUsize, time::Duration};

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
pub enum Api {
    #[cfg(feature = "vulkan")]
    Vulkan,
    #[cfg(all(windows, feature = "d3d12"))]
    D3D12,
}

impl Api {
    #[cfg(not(test))]
    pub fn parse(name: &str) -> Result<Self, Box<dyn Error>> {
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
pub struct Output {
    pub sdr: Vec<u8>,
    pub depth: Option<Vec<u8>>,
    #[cfg(test)]
    pub shadow_maps: [Vec<u8>; CASCADE_COUNT],
}

pub struct CapturedFrame {
    pub output: Output,
    pub reused: [bool; CASCADE_COUNT],
    #[cfg(test)]
    pub shadow_passes: usize,
}

pub fn render(
    api: Api,
    settings: Settings,
    copy_hdr: bool,
    bloom: Bloom,
    caster_revisions: &[[u64; CASCADE_COUNT]],
) -> Result<Vec<CapturedFrame>, Box<dyn Error>> {
    let frames = caster_revisions.len().max(1);
    // Give enabled Bloom an emitter above its scene-linear threshold.
    let triangle = Triangle::for_view(bloom != Bloom::Off, settings.placement, settings.view)?;
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
                depth: (settings.samples == SampleCount::X1).then(|| vec![0; width * height * 4]),
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

pub fn probe(output: &Output) -> Result<([u8; 4], Option<f32>), Box<dyn Error>> {
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

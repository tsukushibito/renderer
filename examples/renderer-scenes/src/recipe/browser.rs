//! Canvas host for the standard reference scene recipe.
use super::{
    Error, Options, RectLights,
    asset::Scene,
    backend,
    camera::{self, Camera, Conditions},
    ibl::Environment,
    pipeline::{self, Budget, Captures, Resident, Workspace},
};
use crate::shared::web_assets;
use js_sys::{Function, Object, Reflect, Uint8Array};
use sashiko_renderer_core::{
    graph::GraphPool,
    runtime::{FrameId, Runtime},
    webgpu::{CanvasTarget, wgpu},
};
use wasm_bindgen::prelude::*;
use web_sys::HtmlCanvasElement;

fn js_error(error: impl std::fmt::Display) -> JsValue {
    js_sys::Error::new(&error.to_string()).into()
}

pub(super) fn progress(callback: &Function, message: &str) -> Result<(), Error> {
    callback
        .call1(&JsValue::UNDEFINED, &JsValue::from_str(message))
        .map_err(|value| format!("load callback failed: {value:?}"))?;
    Ok(())
}

fn device_requirements(budget: &Budget) -> Result<(wgpu::Features, wgpu::Limits), Error> {
    use sashiko_renderer_core::program::{BindingType, ShaderStages};

    let mut features = wgpu::Features::empty();
    if budget
        .images
        .iter()
        .any(|(desc, _)| desc.format.block_extent() != [1, 1])
    {
        features |= wgpu::Features::TEXTURE_COMPRESSION_BC;
    }
    if budget
        .images
        .iter()
        .any(|(desc, _)| desc.format == sashiko_renderer_core::ImageFormat::Rgba32Float)
    {
        features |= wgpu::Features::FLOAT32_FILTERABLE;
    }
    let mut limits = wgpu::Limits::default();
    limits.max_buffer_size = limits
        .max_buffer_size
        .max(budget.max_buffer_bytes)
        .max(budget.upload_bytes as u64)
        .max(budget.readback_bytes);
    for (desc, _) in &budget.images {
        limits.max_texture_dimension_2d = limits
            .max_texture_dimension_2d
            .max(desc.width)
            .max(desc.height);
    }
    let prepared = budget
        .prepared
        .as_ref()
        .ok_or("missing prepared pipeline")?;
    for request in prepared.working_set().raster {
        let slots = request.program.bindings();
        for stage in [ShaderStages::VERTEX, ShaderStages::FRAGMENT] {
            let count = slots
                .iter()
                .filter(|slot| {
                    slot.stages.contains(stage) && slot.kind == BindingType::SampledImage
                })
                .count();
            limits.max_sampled_textures_per_shader_stage = limits
                .max_sampled_textures_per_shader_stage
                .max(u32::try_from(count)?);
        }
    }
    Ok((features, limits))
}

#[wasm_bindgen(js_name = openScene)]
pub async fn open_scene(
    canvas: HtmlCanvasElement,
    url: String,
    on_progress: Function,
) -> Result<SceneSession, JsValue> {
    load(canvas, &url, &on_progress).await.map_err(js_error)
}

async fn load(
    canvas: HtmlCanvasElement,
    url: &str,
    on_progress: &Function,
) -> Result<SceneSession, Error> {
    let mut options = Options::defaults();
    options.api = "webgpu".into();
    options.width = canvas.width().max(1);
    options.height = canvas.height().max(1);
    options.fixture = url == "fixture";
    options.inflight = 1;
    let (mut scene, mut environment, lightmaps) = if options.fixture {
        options.view = "fixture".into();
        (Scene::fixture(false), Environment::fixture(), None)
    } else {
        let data = web_assets::load(url, on_progress).await?;
        options.look = data.look.parse()?;
        options.view = data.view;
        options.baked_lighting = Some(url.into());
        options.reflection_probes = Some(url.into());
        (data.scene, data.environment, Some(data.lighting))
    };
    let mut conditions = Conditions::parse(&serde_json::from_slice(options.condition_bytes())?)?;
    if !options.fixture && !conditions.ambient_occlusion {
        scene.disable_environment_occlusion();
    }
    options.look.apply(&mut conditions, &mut environment);
    if options.baked_lighting.is_some() {
        let light = camera::BakeScenario::for_look(options.scenario_blend, options.look);
        conditions.light_direction = light.direction;
        conditions.light_color = light.sun;
        conditions.background = light.sky;
    }
    let camera = if options.fixture {
        Camera::fixture()
    } else {
        conditions.camera(&options.view, 0)?
    };
    let mut budget = Budget::new(&scene, &environment, &options, conditions.background)?;
    let (features, limits) = device_requirements(&budget)?;

    progress(on_progress, "Preparing WebGPU pipelines")?;
    let target = AttachedCanvas(Some(
        CanvasTarget::attach(canvas.clone(), features, limits).await?,
    ));
    let mut runtime = backend::canvas_runtime(
        target.0.as_ref().expect("attached canvas"),
        &options,
        &budget,
    )
    .await?;
    let input = options.camera_lighting(&conditions, camera, 0)?;
    let resident = Resident::upload_scene(
        &mut runtime,
        &scene,
        &environment,
        &budget,
        &input,
        lightmaps.as_ref(),
        on_progress,
    )
    .await?;
    scene.vertices = Vec::new();
    scene.indices = Vec::new();
    scene.lightmap_uv = Vec::new();
    let pool = GraphPool::new(budget.graph, options.inflight + 1)?;
    let mut workspace = Workspace::new(
        &scene,
        budget.prepared.take().ok_or("missing prepared pipeline")?,
    );
    workspace.enable_shadow_reuse();
    Ok(SceneSession {
        runtime,
        resident,
        pool,
        workspace,
        budget,
        scene,
        conditions,
        options,
        camera,
        canvas,
        target,
        sequence: 0,
        pending: None,
    })
}

#[wasm_bindgen]
pub struct SceneSession {
    runtime: Runtime,
    resident: Resident,
    pool: GraphPool,
    workspace: Workspace,
    budget: Budget,
    scene: Scene,
    conditions: Conditions,
    options: Options,
    camera: Camera,
    canvas: HtmlCanvasElement,
    sequence: usize,
    pending: Option<FrameId>,
    // Dispose after all scene resource owners and the runtime have been released.
    target: AttachedCanvas,
}

struct AttachedCanvas(Option<CanvasTarget>);

impl Drop for AttachedCanvas {
    fn drop(&mut self) {
        if let Some(target) = self.0.take() {
            target.dispose();
        }
    }
}

#[wasm_bindgen]
impl SceneSession {
    /// Select a native reference camera, including its exact target and field of view.
    pub fn preset(&mut self, name: &str) -> Result<(), JsValue> {
        self.camera = if self.options.fixture && name == "fixture" {
            Camera::fixture()
        } else {
            self.conditions.camera(name, 0).map_err(js_error)?
        };
        Ok(())
    }

    /// Advance the camera and present one frame when the previous work has retired.
    pub fn frame(
        &mut self,
        dt: f32,
        forward: f32,
        right: f32,
        up: f32,
        yaw: f32,
        pitch: f32,
    ) -> Result<bool, JsValue> {
        self.advance(dt, [forward, right, up], yaw, pitch)
            .map_err(js_error)?;
        self.target
            .0
            .as_ref()
            .expect("live target")
            .check_device()
            .map_err(js_error)?;
        if !self.retire().map_err(js_error)? {
            return Ok(false);
        }
        if !self.resize().map_err(js_error)? {
            return Ok(false);
        }
        self.submit(false)
            .map(|(presented, _)| presented)
            .map_err(js_error)
    }

    #[wasm_bindgen(js_name = setRectLights)]
    pub async fn set_rect_lights(&mut self, mode: &str) -> Result<(), JsValue> {
        if self.options.look != camera::Look::O3deAtrium {
            return Err(js_error("rectangle lights require the Sponza atrium"));
        }
        let mode = match mode {
            "off" => RectLights::Off,
            "unshadowed" => RectLights::Unshadowed,
            "shadowed" => RectLights::Shadowed,
            _ => return Err(js_error("unknown rectangle light mode")),
        };
        if self.options.rect_lights != mode {
            self.drain().await.map_err(js_error)?;
            let mut options = self.options.clone();
            options.rect_lights = mode;
            self.reconfigure(options).map_err(js_error)?;
            self.resident.set_rect_lights(mode);
        }
        Ok(())
    }

    /// Read back linear HDR, display-encoded SDR and depth from the same rendered frame.
    pub async fn capture(&mut self) -> Result<JsValue, JsValue> {
        self.capture_frame().await.map_err(js_error)
    }
}

impl SceneSession {
    fn advance(&mut self, dt: f32, axes: [f32; 3], yaw: f32, pitch: f32) -> Result<(), Error> {
        if ![dt, yaw, pitch].into_iter().chain(axes).all(f32::is_finite) {
            return Err("non-finite camera input".into());
        }
        if axes == [0.; 3] && yaw == 0. && pitch == 0. {
            return Ok(());
        }
        let direction = camera::normalize(std::array::from_fn(|i| {
            self.camera.target[i] - self.camera.position[i]
        }))?;
        let heading = direction[0].atan2(-direction[2]) + yaw;
        let elevation = (direction[1].asin() + pitch).clamp(-1.55, 1.55);
        let (s, c) = heading.sin_cos();
        let (v, h) = elevation.sin_cos();
        let forward = [h * s, v, -h * c];
        let right = [c, 0., s];
        let up = [-v * s, h, v * c];
        let length = axes.iter().map(|a| a * a).sum::<f32>().sqrt().max(1.);
        let step = dt.clamp(0., 0.1) * 4. / length;
        for i in 0..3 {
            self.camera.position[i] +=
                step * (axes[0] * forward[i] + axes[1] * right[i] + axes[2] * up[i]);
            self.camera.target[i] = self.camera.position[i] + forward[i];
        }
        Ok(())
    }

    fn retire(&mut self) -> Result<bool, Error> {
        if let Some(id) = self.pending {
            if self
                .runtime
                .wait_for_frame(id, std::time::Duration::ZERO)?
                .is_none()
            {
                return Ok(false);
            }
            self.pending = None;
        }
        Ok(true)
    }

    async fn drain(&mut self) -> Result<(), Error> {
        if let Some(id) = self.pending {
            self.runtime.wait_for_webgpu_frame(id).await?;
            self.pending = None;
        }
        Ok(())
    }

    fn reconfigure(&mut self, options: Options) -> Result<(), Error> {
        let prepared =
            self.budget
                .prepare_frame(&options, &self.scene, self.conditions.background)?;
        self.workspace.clear_shadow_reuse(&mut self.runtime)?;
        backend::resize(&mut self.runtime, &options, &self.budget, &prepared)?;
        self.workspace = Workspace::new(&self.scene, prepared);
        self.workspace.enable_shadow_reuse();
        self.options = options;
        Ok(())
    }

    fn resize(&mut self) -> Result<bool, Error> {
        let size = (self.canvas.width(), self.canvas.height());
        if size.0 == 0 || size.1 == 0 {
            return Ok(false);
        }
        if size != (self.options.width, self.options.height) {
            let mut options = self.options.clone();
            (options.width, options.height) = size;
            self.reconfigure(options)?;
        }
        Ok(true)
    }

    fn submit(&mut self, capture: bool) -> Result<(bool, Option<Captures>), Error> {
        let input = self
            .options
            .camera_lighting(&self.conditions, self.camera, self.sequence)?;
        let built = pipeline::build(
            &mut self.pool,
            &mut self.workspace,
            &self.resident,
            &self.scene,
            &self.options,
            &input,
            self.sequence,
            capture,
        )?;
        let graph = built.graph.compile()?;
        let (receipt, owner) = self
            .runtime
            .submit_with_image(&graph, built.output.raw_image())?;
        self.pending = Some(receipt.id());
        self.workspace
            .accept_shadow_updates(&receipt, built.shadow_updates, built.rect_updates)?;
        self.resident.accept_camera(&receipt, built.camera_update)?;
        let presented = self
            .target
            .0
            .as_mut()
            .expect("live target")
            .present(&self.runtime, owner.snapshot())?;
        self.sequence = self.sequence.wrapping_add(1);
        Ok((presented, built.captures))
    }

    async fn capture_frame(&mut self) -> Result<JsValue, Error> {
        self.drain().await?;
        if !self.resize()? {
            return Err("cannot capture an empty canvas".into());
        }
        let (_, captures) = self.submit(true)?;
        let captures = captures.ok_or("scene readback missing")?;
        let mut result = self
            .runtime
            .wait_for_webgpu_frame(self.pending.ok_or("missing capture frame")?)
            .await?;
        self.pending = None;
        let object = Object::new();
        for (name, pass) in [
            ("hdr", captures.hdr),
            ("sdr", captures.sdr),
            ("depth", captures.depth.ok_or("missing scene depth")?),
        ] {
            let mut bytes = vec![0; result.readback_len(pass)?];
            result.readback_into(pass, &mut bytes)?;
            Reflect::set(&object, &name.into(), &Uint8Array::from(bytes.as_slice()))
                .map_err(|e| format!("capture result: {e:?}"))?;
        }
        for (name, value) in [
            ("width", self.options.width),
            ("height", self.options.height),
        ] {
            Reflect::set(&object, &name.into(), &value.into())
                .map_err(|e| format!("capture size: {e:?}"))?;
        }
        Ok(object.into())
    }
}

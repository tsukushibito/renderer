//! Browser-owned canvas and asynchronous WebGPU device initialization.
use std::sync::{Arc, Mutex};

use web_sys::HtmlCanvasElement;
pub use wgpu;
use wgpu::{CurrentSurfaceTexture, TextureFormat};
mod bindings;
mod compute;
pub(crate) mod engine;
mod errors;
mod present;
mod raster;

/// Presentation target for an existing browser canvas. Create a fresh target after device loss.
pub struct CanvasTarget {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    canvas: HtmlCanvasElement,
    failure: Arc<Mutex<Option<String>>>,
    _errors: errors::Subscription,
    configured: bool,
    display: present::DisplayPass,
}

impl CanvasTarget {
    /// Negotiate the features and limits required by a prepared scene.
    pub async fn attach(
        canvas: HtmlCanvasElement,
        features: wgpu::Features,
        limits: wgpu::Limits,
    ) -> Result<Self, String> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
            backends: wgpu::Backends::BROWSER_WEBGPU,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        });
        let surface = instance
            .create_surface(wgpu::SurfaceTarget::Canvas(canvas.clone()))
            .map_err(|e| e.to_string())?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                compatible_surface: Some(&surface),
                power_preference: wgpu::PowerPreference::HighPerformance,
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let missing = features - adapter.features();
        if !missing.is_empty() {
            return Err(format!("WebGPU adapter lacks {missing:?}"));
        }
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                required_features: features,
                required_limits: limits,
                ..Default::default()
            })
            .await
            .map_err(|e| e.to_string())?;
        let failure = Arc::new(Mutex::new(None));
        let notice = Arc::clone(&failure);
        let errors = errors::observe(
            &device,
            Arc::new(move |_, message| {
                notice
                    .lock()
                    .expect("device failure")
                    .get_or_insert(message);
            }),
        )?;
        let mut config = surface
            .get_default_config(&adapter, 1, 1)
            .ok_or("WebGPU canvas has no presentation configuration")?;
        // The final SDR pass already encoded display values. Use a linear attachment
        // to avoid applying the sRGB transfer function a second time.
        config.format = surface
            .get_capabilities(&adapter)
            .formats
            .into_iter()
            .find(|f| matches!(f, TextureFormat::Bgra8Unorm | TextureFormat::Rgba8Unorm))
            .ok_or("WebGPU canvas has no eight-bit SDR format")?;
        // Finish error scopes for display pipeline creation before exposing the target.
        let memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let display = present::DisplayPass::new(&device, config.format);
        let scoped = [
            validation.pop().await,
            internal.pop().await,
            memory.pop().await,
        ];
        if let Some(error) = scoped.into_iter().flatten().next() {
            device.destroy();
            return Err(error.to_string());
        }
        Ok(Self {
            device,
            queue,
            surface,
            canvas,
            config,
            failure,
            _errors: errors,
            configured: false,
            display,
        })
    }

    pub fn device(&self) -> &wgpu::Device {
        &self.device
    }
    pub fn queue(&self) -> &wgpu::Queue {
        &self.queue
    }
    pub fn format(&self) -> TextureFormat {
        self.config.format
    }

    /// Surface device failures reported asynchronously by the browser, including during loading.
    pub fn check_device(&self) -> Result<(), String> {
        match &*self.failure.lock().expect("device failure") {
            Some(message) => Err(message.clone()),
            None => Ok(()),
        }
    }

    /// Explicitly retire this target's dedicated device when its host detaches.
    pub fn dispose(self) {
        self.device.destroy();
    }

    /// Present the current scheduled contents of an exported SDR image. The host can
    /// release the image owner once the presentation commands have been accepted.
    pub fn present(
        &mut self,
        runtime: &crate::runtime::Runtime,
        image: &crate::graph::ImportImage,
    ) -> Result<bool, String> {
        if runtime.webgpu_device() != Some(&self.device) {
            return Err("WebGPU canvas and renderer belong to different devices".into());
        }
        let texture = runtime.webgpu_texture(image).map_err(|e| e.to_string())?;
        let submitted = self.present_image(texture)?;
        if submitted {
            runtime
                .mark_webgpu_presented(image)
                .map_err(|e| e.to_string())?;
        }
        Ok(submitted)
    }

    fn present_image(&mut self, image: &wgpu::Texture) -> Result<bool, String> {
        let Some(frame) = self.acquire()? else {
            return Ok(false);
        };
        if image.format() != TextureFormat::Rgba8Unorm
            || image.width() != self.config.width
            || image.height() != self.config.height
            || image.sample_count() != 1
        {
            return Err(
                "canvas presentation needs a matching single-sampled RGBA8 SDR image".into(),
            );
        }
        let target = frame.texture.create_view(&Default::default());
        let mut encoder = self.device.create_command_encoder(&Default::default());
        self.display
            .record(&self.device, &mut encoder, image, &target);
        self.queue.submit([encoder.finish()]);
        self.queue.present(frame);
        Ok(true)
    }

    /// A minimized canvas has no drawable extent. A subsequent frame retries acquisition.
    pub fn acquire(&mut self) -> Result<Option<wgpu::SurfaceTexture>, String> {
        self.check_device()?;
        let width = self.canvas.width();
        let height = self.canvas.height();
        if width == 0 || height == 0 {
            return Ok(None);
        }
        if !self.configured || (width, height) != (self.config.width, self.config.height) {
            if width.max(height) > self.device.limits().max_texture_dimension_2d {
                return Err(format!(
                    "canvas dimensions exceed the device limit: {width} x {height}"
                ));
            }
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
            self.display.resized();
            self.configured = true;
        }
        match self.surface.get_current_texture() {
            CurrentSurfaceTexture::Success(texture) => Ok(Some(texture)),
            CurrentSurfaceTexture::Suboptimal(texture) => {
                self.configured = false;
                Ok(Some(texture))
            }
            CurrentSurfaceTexture::Outdated => {
                self.configured = false;
                Ok(None)
            }
            CurrentSurfaceTexture::Timeout | CurrentSurfaceTexture::Occluded => Ok(None),
            CurrentSurfaceTexture::Lost => Err("WebGPU canvas lost; reattach the canvas".into()),
            CurrentSurfaceTexture::Validation => {
                Err("WebGPU canvas acquisition failed validation".into())
            }
        }
    }
}

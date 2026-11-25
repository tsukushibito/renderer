use wgpu::{
    BindGroup, BindGroupDescriptor, BindGroupEntry, BindingResource, Device, RenderPipeline,
    Texture, TextureFormat,
};

pub(super) struct DisplayPass {
    pipeline: RenderPipeline,
    // The frame renderer rotates among a bounded number of SDR targets.
    sources: Vec<(Texture, BindGroup)>,
    next: usize,
}

impl DisplayPass {
    pub(super) fn new(device: &Device, format: TextureFormat) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Sashiko canvas display"),
            source: wgpu::ShaderSource::Wgsl(include_str!("present.wgsl").into()),
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("Sashiko canvas display"),
            layout: None,
            vertex: wgpu::VertexState {
                module: &module,
                entry_point: Some("vertices"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &module,
                entry_point: Some("display"),
                targets: &[Some(format.into())],
                compilation_options: Default::default(),
            }),
            primitive: Default::default(),
            multisample: Default::default(),
            depth_stencil: None,
            multiview_mask: None,
            cache: None,
        });
        Self {
            pipeline,
            sources: Vec::with_capacity(4),
            next: 0,
        }
    }

    pub(super) fn resized(&mut self) {
        self.sources.clear();
        self.next = 0;
    }

    pub(super) fn record(
        &mut self,
        device: &Device,
        encoder: &mut wgpu::CommandEncoder,
        source: &Texture,
        target: &wgpu::TextureView,
    ) {
        let index = match self.sources.iter().position(|(image, _)| image == source) {
            Some(index) => index,
            None => {
                let view = source.create_view(&Default::default());
                let group = device.create_bind_group(&BindGroupDescriptor {
                    label: Some("Sashiko canvas source"),
                    layout: &self.pipeline.get_bind_group_layout(0),
                    entries: &[BindGroupEntry {
                        binding: 0,
                        resource: BindingResource::TextureView(&view),
                    }],
                });
                if self.sources.len() == self.sources.capacity() {
                    let index = self.next;
                    self.sources[index] = (source.clone(), group);
                    self.next = (index + 1) % self.sources.len();
                    index
                } else {
                    self.sources.push((source.clone(), group));
                    self.sources.len() - 1
                }
            }
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("Sashiko canvas display"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: target,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                    store: wgpu::StoreOp::Store,
                },
            })],
            ..Default::default()
        });
        pass.set_pipeline(&self.pipeline);
        pass.set_bind_group(0, &self.sources[index].1, &[]);
        pass.draw(0..3, 0..1);
    }
}

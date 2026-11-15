//! Full-coverage clears for values outside an image's optimized native clear.
//!
//! D3D12 reports nonmatching native clears as performance warnings. A finite
//! per-frame value cannot be part of the immutable resource's optimized hint.
//! Keep the native fast path for its exact hint and use this prepared draw for
//! other values, without reallocating images or filtering native diagnostics.

use super::{
    D3D12RuntimeError,
    bindings::*,
    device::DeviceOwner,
    raster::{INLINE_ROOT, RasterPipeline},
    raster_storage::{D3D12ImageClass, GraphImage},
    resources::DeviceBuffer,
};
use crate::{graph::ImageFormat, program::ManagedRasterProgram};

pub(super) struct ClearPrograms {
    pipelines: [Option<RasterPipeline>; 3],
    vertices: DeviceBuffer,
    indices: DeviceBuffer,
}
// SAFETY: The immutable UPLOAD geometry is written before publication and never
// mapped or modified again. Its free-threaded buffers and PSOs are retained by
// FramePool through every recording/GPU consumer, including abnormal teardown.
unsafe impl Send for ClearPrograms {}
// SAFETY: Concurrent readers only bind immutable geometry and native PSOs.
unsafe impl Sync for ClearPrograms {}

fn format_index(format: ImageFormat) -> usize {
    match format {
        ImageFormat::Rgba8Unorm => 0,
        ImageFormat::Rgba16Float => 1,
        ImageFormat::Depth32Float => 2,
    }
}

impl ClearPrograms {
    pub(super) fn new(
        owner: &DeviceOwner,
        classes: &[D3D12ImageClass],
    ) -> Result<Self, D3D12RuntimeError> {
        let mut pipelines = std::array::from_fn(|_| None);
        for class in classes {
            let index = format_index(class.desc.format);
            if pipelines[index].is_none() {
                let (program, depth) = match class.desc.format {
                    ImageFormat::Rgba8Unorm => (ManagedRasterProgram::mesh(), false),
                    ImageFormat::Rgba16Float => (ManagedRasterProgram::mesh_hdr(), false),
                    ImageFormat::Depth32Float => (ManagedRasterProgram::depth_only(), true),
                };
                pipelines[index] = Some(RasterPipeline::clear(owner, &program, depth)?);
            }
        }
        // One oversized triangle covers the viewport with one sample and no
        // diagonal seam. All vertices have the same color, w=1 and depth=0;
        // the inline matrix selects the exact requested normal/zero depth.
        let floats: [f32; 21] = [
            -1.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, 3.0, -1.0, 0.0, 1.0, 1.0, 1.0, 1.0, -1.0, 3.0,
            0.0, 1.0, 1.0, 1.0, 1.0,
        ];
        let mut vertex_bytes = [0u8; 84];
        for (destination, value) in vertex_bytes.as_chunks_mut::<4>().0.iter_mut().zip(floats) {
            destination.copy_from_slice(&value.to_le_bytes());
        }
        let mut index_bytes = [0u8; 12];
        for (destination, value) in index_bytes
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip([0u32, 1, 2])
        {
            destination.copy_from_slice(&value.to_le_bytes());
        }
        let vertices = DeviceBuffer::new(owner, 84, D3D12_HEAP_TYPE_UPLOAD)?;
        let indices = DeviceBuffer::new(owner, 12, D3D12_HEAP_TYPE_UPLOAD)?;
        // SAFETY: Newly owned UPLOAD buffers exactly cover the stack byte arrays
        // and have no GPU readers. No mutable access survives publication.
        unsafe {
            vertices.write(owner, &vertex_bytes)?;
            indices.write(owner, &index_bytes)?;
        }
        Ok(Self {
            pipelines,
            vertices,
            indices,
        })
    }

    pub(super) fn record(
        &self,
        list: &ID3D12GraphicsCommandList,
        image: &GraphImage,
        color: [f32; 4],
        depth: Option<f32>,
    ) {
        let pipeline = self.pipelines[format_index(image.desc.format)]
            .as_ref()
            .unwrap();
        let mut words = [0u32; 20];
        for index in [0, 5, 10, 15] {
            words[index] = 1.0f32.to_bits();
        }
        words[14] = depth.unwrap_or(0.0).to_bits();
        for (destination, value) in words[16..].iter_mut().zip(color) {
            *destination = value.to_bits();
        }
        let viewport = D3D12_VIEWPORT {
            TopLeftX: 0.0,
            TopLeftY: 0.0,
            Width: image.desc.width as f32,
            Height: image.desc.height as f32,
            MinDepth: 0.0,
            MaxDepth: 1.0,
        };
        let rect = D3D12_RECT {
            left: 0,
            top: 0,
            right: image.desc.width as i32,
            bottom: image.desc.height as i32,
        };
        // SAFETY: The caller transitioned this full image to its attachment
        // state. Preparation established the format/extent, PSO and immutable
        // geometry. A color clear binds no depth; a depth clear binds no color
        // and uses ALWAYS with depth writes. Thus other attachments are intact.
        // The triangle covers every sample; no culling, blending or stencil can
        // suppress the overwrite. The pool retains every operand through GPU use.
        unsafe {
            list.SetPipelineState(&*pipeline.pipeline);
            list.SetGraphicsRootSignature(&*pipeline.signature);
            list.OMSetRenderTargets(
                image.rtv.is_some() as u32,
                image.rtv.as_ref().map(std::ptr::from_ref),
                false,
                image.dsv.as_ref().map(std::ptr::from_ref),
            );
            list.RSSetViewports(&[viewport]);
            list.RSSetScissorRects(&[rect]);
            list.IASetPrimitiveTopology(D3D12_PRIMITIVE_TOPOLOGY(
                D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST,
            ));
            let vertices = D3D12_VERTEX_BUFFER_VIEW {
                BufferLocation: self.vertices.raw().GetGPUVirtualAddress(),
                SizeInBytes: 84,
                StrideInBytes: 28,
            };
            let indices = D3D12_INDEX_BUFFER_VIEW {
                BufferLocation: self.indices.raw().GetGPUVirtualAddress(),
                SizeInBytes: 12,
                Format: DXGI_FORMAT_R32_UINT,
            };
            list.IASetVertexBuffers(0, Some(&[vertices]));
            list.IASetIndexBuffer(Some(&indices));
            list.SetGraphicsRoot32BitConstants(INLINE_ROOT, 20, words.as_ptr().cast(), 0);
            list.DrawIndexedInstanced(3, 1, 0, 0, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        backends::d3d12::{D3D12Runtime, D3D12RuntimeConfig, D3D12ValidationMode},
        graph::{
            Attachment, GraphCapacity, GraphPool, ImageDesc, LoadOp, RasterAttachments, StoreOp,
        },
    };
    use std::time::Duration;

    #[test]
    #[ignore = "requires real D3D12 hardware and requested native diagnostics"]
    fn d3d12_variable_clears_preserve_other_attachments_and_cover_uneven_extents() {
        let strict = cfg!(feature = "development-validation");
        let mut runtime = D3D12Runtime::new(D3D12RuntimeConfig {
            validation: if strict {
                D3D12ValidationMode::Required
            } else {
                D3D12ValidationMode::Disabled
            },
            ..Default::default()
        })
        .unwrap();
        let monitor = runtime.validation_monitor();
        let color_desc = ImageDesc::rgba8(3, 5);
        let depth_desc = ImageDesc::depth32(3, 5);
        runtime
            .prepare_raster_working_set(
                &[
                    D3D12ImageClass {
                        desc: color_desc,
                        count: 1,
                    },
                    D3D12ImageClass {
                        desc: depth_desc,
                        count: 1,
                    },
                ],
                &[],
            )
            .unwrap();
        let mut pool = GraphPool::new(GraphCapacity::default(), 1).unwrap();
        let mut graph = pool.begin().unwrap();
        let color = graph.image("uneven color", color_desc).unwrap();
        let depth = graph.image("uneven depth", depth_desc).unwrap();
        let color_view = graph.image_view(color, 0, 0).unwrap();
        let depth_view = graph.image_view(depth, 0, 0).unwrap();
        let mut expected = Vec::new();
        for (name, color_load, depth_load, pixel, z) in [
            (
                "initial",
                LoadOp::Clear([1.0, 0.0, 0.0, 1.0]),
                LoadOp::Clear(0.25),
                [255, 0, 0, 255],
                0.25f32,
            ),
            // A larger replacement depth detects accidentally retaining LESS.
            (
                "replace depth",
                LoadOp::Load,
                LoadOp::Clear(0.75),
                [255, 0, 0, 255],
                0.75,
            ),
            (
                "replace color",
                LoadOp::Clear([0.0, 1.0, 0.0, 1.0]),
                LoadOp::Load,
                [0, 255, 0, 255],
                0.75,
            ),
            (
                "zero depth",
                LoadOp::Load,
                LoadOp::Clear(0.0),
                [0, 255, 0, 255],
                0.0,
            ),
            (
                "default depth",
                LoadOp::Load,
                LoadOp::Clear(1.0),
                [0, 255, 0, 255],
                1.0,
            ),
            (
                "default color",
                LoadOp::Clear([0.0, 0.0, 0.0, 1.0]),
                LoadOp::Load,
                [0, 0, 0, 255],
                1.0,
            ),
        ] {
            graph
                .attachment_pass(
                    name,
                    RasterAttachments {
                        colors: &[Attachment {
                            view: color_view,
                            load: color_load,
                            store: StoreOp::Store,
                        }],
                        depth: Some(Attachment {
                            view: depth_view,
                            load: depth_load,
                            store: StoreOp::Store,
                        }),
                    },
                )
                .unwrap();
            let read_color = graph.readback_image("color result", color).unwrap();
            let read_depth = graph.readback_image("depth result", depth).unwrap();
            expected.push((read_color, pixel.repeat(15)));
            expected.push((read_depth, z.to_le_bytes().repeat(15)));
        }
        let plan = graph.compile().unwrap();
        let mut submission = runtime.submit(&plan).unwrap();
        let mut result = submission.wait(Duration::from_secs(30)).unwrap().unwrap();
        for (pass, expected) in expected {
            let mut actual = vec![0; expected.len()];
            result.readback_into(pass, &mut actual).unwrap();
            assert_eq!(actual, expected);
        }
        drop((result, submission, plan, pool));
        runtime.shutdown().unwrap();
        let report = monitor.snapshot();
        assert!(report.is_clean(), "{report:#?}");
        if strict {
            assert!(
                report.teardown_checked
                    && report.debug_layer_enabled
                    && report.gpu_based_validation_enabled
            );
        }
    }
}

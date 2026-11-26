//! Per-submission packed transfers. Queue writes populate only the upload arena:
//! the encoder performs each destination write at its position in the graph.
use super::{Resources, capacity};
use crate::{
    ImageFormat,
    capacity::{CapacityRecovery, RuntimeCapacityKind},
    graph::{CompiledGraph, ImageDesc, ImageRegion, Operation, ResourceDesc},
    runtime::{ExecutionError, ExecutionErrorKind},
};
use std::{
    cell::Cell,
    ops::Range,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    task::Waker,
};

type Result<T> = std::result::Result<T, ExecutionError>;

fn size_error() -> ExecutionError {
    ExecutionError::invalid("WebGPU transfer byte count overflow")
}

fn arena_size(device: &wgpu::Device, bytes: u64) -> Result<(usize, u64)> {
    let size = bytes
        .checked_next_multiple_of(4)
        .ok_or_else(size_error)?
        .max(4);
    if size > device.limits().max_buffer_size || size > isize::MAX as u64 {
        return Err(ExecutionError::webgpu(
            ExecutionErrorKind::Unsupported,
            "transfer arena exceeds the device or host buffer limit",
        ));
    }
    Ok((usize::try_from(bytes).map_err(|_| size_error())?, size))
}

fn staging_range(
    cursor: usize,
    alignment: usize,
    size: usize,
    limit: usize,
    kind: RuntimeCapacityKind,
) -> Result<Range<usize>> {
    let start = cursor
        .checked_next_multiple_of(alignment)
        .ok_or_else(size_error)?;
    let end = start.checked_add(size).ok_or_else(size_error)?;
    if end > limit {
        return Err(capacity(kind, CapacityRecovery::Reconfigure, limit, end));
    }
    Ok(start..end)
}

// Bytes in a WebGPU buffer copy are word-aligned. Graph permits a partial final
// word. Merge the two words on the GPU so a neighbouring byte written by an
// earlier pass isn't lost. A same-buffer copy also needs an intermediate buffer.
struct BufferCopies {
    scratch: wgpu::Buffer,
    merge: wgpu::ComputePipeline,
    words: wgpu::BindGroup,
}

impl BufferCopies {
    const CHUNK: u64 = 64 * 1024;

    fn new(device: &wgpu::Device) -> Self {
        let scratch = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Graph buffer copy scratch"),
            size: Self::CHUNK,
            usage: wgpu::BufferUsages::COPY_SRC
                | wgpu::BufferUsages::COPY_DST
                | wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Graph partial-word buffer copy"),
            source: wgpu::ShaderSource::Wgsl(r#"
                @group(0) @binding(0) var<storage, read_write> words: array<u32, 2>;
                @compute @workgroup_size(1)
                fn merge(@builtin(workgroup_id) group: vec3u, @builtin(num_workgroups) count: vec3u) {
                    if group.x == 0u {
                        let mask = (1u << (count.x * 8u)) - 1u;
                        words[0] = (words[0] & ~mask) | (words[1] & mask);
                    }
                }
            "#.into()),
        });
        let merge = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Graph partial-word buffer copy"),
            layout: None,
            module: &shader,
            entry_point: Some("merge"),
            compilation_options: Default::default(),
            cache: None,
        });
        let words = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &merge.get_bind_group_layout(0),
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: scratch.as_entire_binding(),
            }],
        });
        Self {
            scratch,
            merge,
            words,
        }
    }

    fn copy(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        src: &wgpu::Buffer,
        src_offset: u64,
        dst: &wgpu::Buffer,
        dst_offset: u64,
        size: u64,
    ) {
        let aligned = size / 4 * 4;
        if aligned != 0 {
            if src == dst {
                let mut at = 0;
                while at < aligned {
                    let chunk = (aligned - at).min(Self::CHUNK);
                    encoder.copy_buffer_to_buffer(src, src_offset + at, &self.scratch, 0, chunk);
                    encoder.copy_buffer_to_buffer(&self.scratch, 0, dst, dst_offset + at, chunk);
                    at += chunk;
                }
            } else {
                encoder.copy_buffer_to_buffer(src, src_offset, dst, dst_offset, aligned);
            }
        }
        let tail = size - aligned;
        if tail != 0 {
            encoder.copy_buffer_to_buffer(dst, dst_offset + aligned, &self.scratch, 0, 4);
            encoder.copy_buffer_to_buffer(src, src_offset + aligned, &self.scratch, 4, 4);
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor::default());
                pass.set_pipeline(&self.merge);
                pass.set_bind_group(0, &self.words, &[]);
                pass.dispatch_workgroups(tail as u32, 1, 1);
            }
            encoder.copy_buffer_to_buffer(&self.scratch, 0, dst, dst_offset + aligned, 4);
        }
    }
}

pub(super) struct Upload {
    buffer: wgpu::Buffer,
    copies: BufferCopies,
    bytes: Vec<u8>,
    limit: usize,
}

impl Upload {
    pub(super) fn new(device: &wgpu::Device, bytes: u64) -> Result<Self> {
        let (limit, size) = arena_size(device, bytes)?;
        let mut host = Vec::new();
        host.try_reserve_exact(limit).map_err(|_| {
            capacity(
                RuntimeCapacityKind::HostMemory,
                CapacityRecovery::Reconfigure,
                0,
                limit,
            )
        })?;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Graph upload"),
            size,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            buffer,
            copies: BufferCopies::new(device),
            bytes: host,
            limit,
        })
    }

    fn reserve(&mut self, alignment: usize, size: usize) -> Result<Range<usize>> {
        let range = staging_range(
            self.bytes.len(),
            alignment,
            size,
            self.limit,
            RuntimeCapacityKind::UploadBytes,
        )?;
        self.bytes.resize(range.end, 0);
        Ok(range)
    }

    pub(super) fn clear(&mut self) {
        self.bytes.clear();
    }

    pub(super) fn flush(&self, queue: &wgpu::Queue) {
        if !self.bytes.is_empty() {
            queue.write_buffer(&self.buffer, 0, &self.bytes);
        }
    }

    fn image(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        texture: &wgpu::Texture,
        desc: ImageDesc,
        region: ImageRegion,
        bytes: &[u8],
    ) -> Result<()> {
        let rows = Rows::image(desc.format, region.extent)?;
        let range = self.reserve(desc.format.block_bytes() as usize, rows.span)?;
        let destination = &mut self.bytes[range.clone()];
        if rows.packed == rows.pitch {
            destination.copy_from_slice(bytes);
        } else {
            for (dst, src) in destination
                .chunks_mut(rows.pitch)
                .zip(bytes.chunks_exact(rows.packed))
            {
                dst[..rows.packed].copy_from_slice(src);
            }
        }
        encoder.copy_buffer_to_texture(
            rows.buffer(&self.buffer, range.start),
            image_copy(texture, region, wgpu::TextureAspect::All),
            physical_extent(desc.format, region.extent),
        );
        Ok(())
    }
}

#[derive(Clone, Copy)]
struct Rows {
    packed: usize,
    pitch: usize,
    count: usize,
    span: usize,
}

impl Rows {
    fn image(format: ImageFormat, extent: [u32; 2]) -> Result<Self> {
        let packed =
            usize::try_from(format.packed_row_bytes(extent[0])).map_err(|_| size_error())?;
        let pitch = packed
            .checked_next_multiple_of(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize)
            .ok_or_else(size_error)?;
        let count = format.packed_rows(extent[1]) as usize;
        let span = (count - 1)
            .checked_mul(pitch)
            .and_then(|n| n.checked_add(packed))
            .ok_or_else(size_error)?;
        Ok(Self {
            packed,
            pitch,
            count,
            span,
        })
    }

    fn bytes(size: u64) -> Result<Self> {
        let packed = usize::try_from(size).map_err(|_| size_error())?;
        let span = packed.checked_next_multiple_of(4).ok_or_else(size_error)?;
        Ok(Self {
            packed,
            pitch: span,
            count: 1,
            span,
        })
    }

    fn buffer<'a>(&self, buffer: &'a wgpu::Buffer, offset: usize) -> wgpu::TexelCopyBufferInfo<'a> {
        wgpu::TexelCopyBufferInfo {
            buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: offset as u64,
                bytes_per_row: Some(self.pitch as u32),
                rows_per_image: None,
            },
        }
    }
}

fn image_copy(
    texture: &wgpu::Texture,
    region: ImageRegion,
    aspect: wgpu::TextureAspect,
) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: region.mip_level,
        origin: wgpu::Origin3d {
            x: region.origin[0],
            y: region.origin[1],
            z: 0,
        },
        aspect,
    }
}

fn physical_extent(format: ImageFormat, [width, height]: [u32; 2]) -> wgpu::Extent3d {
    let [x, y] = format.block_extent();
    wgpu::Extent3d {
        width: width.next_multiple_of(x),
        height: height.next_multiple_of(y),
        depth_or_array_layers: 1,
    }
}

struct Readback {
    pass: usize,
    range: Range<usize>,
    rows: Rows,
}

pub(super) struct Readbacks {
    buffer: wgpu::Buffer,
    entries: Vec<Readback>,
    pass_limit: usize,
    limit: usize,
    used: usize,
    mapping_requested: Cell<bool>,
}

impl Readbacks {
    pub(super) fn new(device: &wgpu::Device, bytes: u64, pass_capacity: usize) -> Result<Self> {
        let (limit, size) = arena_size(device, bytes)?;
        let mut entries = Vec::new();
        entries.try_reserve_exact(pass_capacity).map_err(|_| {
            capacity(
                RuntimeCapacityKind::HostMemory,
                CapacityRecovery::Reconfigure,
                0,
                pass_capacity,
            )
        })?;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Graph readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            buffer,
            entries,
            pass_limit: pass_capacity,
            limit,
            used: 0,
            mapping_requested: Cell::new(false),
        })
    }

    fn reserve(&mut self, pass: usize, rows: Rows, alignment: usize) -> Result<usize> {
        if self.entries.len() == self.pass_limit {
            return Err(capacity(
                RuntimeCapacityKind::GraphMetadata,
                CapacityRecovery::Reconfigure,
                self.pass_limit,
                self.entries.len() + 1,
            ));
        }
        let range = staging_range(
            self.used,
            alignment.max(wgpu::MAP_ALIGNMENT as usize),
            rows.span,
            self.limit,
            RuntimeCapacityKind::ReadbackBytes,
        )?;
        let start = range.start;
        self.used = range.end;
        self.entries.push(Readback { pass, range, rows });
        Ok(start)
    }

    pub(super) fn start_map(&self, ready: Arc<AtomicU8>, waker: Arc<Mutex<Option<Waker>>>) {
        let signal = move |status| {
            ready.store(status, Ordering::Release);
            let pending = waker.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(waker) = pending {
                waker.wake();
            }
        };
        if self.used == 0 {
            signal(1);
        } else {
            self.mapping_requested.set(true);
            self.buffer
                .slice(..self.used as u64)
                .map_async(wgpu::MapMode::Read, move |status| {
                    signal(if status.is_ok() { 1 } else { 2 });
                });
        }
    }

    pub(super) fn reset(&mut self) {
        self.unmap();
        self.entries.clear();
        self.used = 0;
    }

    pub(super) fn unmap(&mut self) {
        if self.mapping_requested.replace(false) {
            self.buffer.unmap();
        }
    }

    fn entry_index(&self, pass: usize) -> Result<usize> {
        self.entries
            .iter()
            .position(|r| r.pass == pass)
            .ok_or_else(|| ExecutionError::invalid("pass has no unconsumed WebGPU readback"))
    }

    pub(super) fn readback_len(&self, pass: usize) -> Result<usize> {
        let rows = self.entries[self.entry_index(pass)?].rows;
        Ok(rows.count * rows.packed)
    }

    pub(super) fn readback_into(&mut self, pass: usize, output: &mut [u8]) -> Result<()> {
        let index = self.entry_index(pass)?;
        let entry = &self.entries[index];
        let rows = entry.rows;
        if output.len() != rows.count * rows.packed {
            return Err(ExecutionError::invalid(
                "WebGPU readback destination has the wrong size",
            ));
        }
        let mapped = self
            .buffer
            .slice(entry.range.start as u64..entry.range.end as u64)
            .get_mapped_range()
            .map_err(|e| ExecutionError::webgpu(ExecutionErrorKind::Native, e.to_string()))?;
        let array = mapped.as_uint8array();
        if rows.packed == rows.pitch {
            array.copy_to(output);
        } else {
            for (row, dst) in output.chunks_exact_mut(rows.packed).enumerate() {
                let start = row * rows.pitch;
                array
                    .subarray(start as u32, (start + rows.packed) as u32)
                    .copy_to(dst);
            }
        }
        drop(mapped);
        self.entries.swap_remove(index);
        Ok(())
    }
}

fn description(graph: &CompiledGraph, resource: usize) -> ImageDesc {
    let ResourceDesc::Image(desc) = graph.data().resources[resource].desc else {
        unreachable!("image transfer built from a checked Graph image handle");
    };
    desc
}

pub(super) fn record(
    graph: &CompiledGraph,
    pass: usize,
    resources: &Resources<'_>,
    encoder: &mut wgpu::CommandEncoder,
    upload: &mut Upload,
    result: &mut Readbacks,
) -> Result<()> {
    let storage = graph.data();
    let op = &storage.passes[pass].operation;
    match *op {
        Operation::UploadBuffer { dst, offset, .. } => {
            let bytes = storage.upload_data(op);
            let size = bytes
                .len()
                .checked_next_multiple_of(4)
                .ok_or_else(size_error)?;
            let range = upload.reserve(4, size)?;
            upload.bytes[range.start..range.start + bytes.len()].copy_from_slice(bytes);
            upload.copies.copy(
                encoder,
                &upload.buffer,
                range.start as u64,
                resources.buffer(dst)?,
                offset,
                bytes.len() as u64,
            );
        }
        Operation::CopyBuffer {
            src,
            dst,
            src_offset,
            dst_offset,
            size,
        } => {
            upload.copies.copy(
                encoder,
                resources.buffer(src)?,
                src_offset,
                resources.buffer(dst)?,
                dst_offset,
                size,
            );
        }
        Operation::UploadImage { dst, region, .. } => {
            let desc = description(graph, dst);
            let texture = resources.texture(dst)?;
            let mut bytes = storage.upload_data(op);
            if let Some(region) = region {
                upload.image(encoder, texture, desc, region, bytes)?;
            } else {
                for mip_level in 0..desc.mip_levels {
                    let extent = desc.mip_extent(mip_level).expect("Graph image mip");
                    let size = usize::try_from(
                        desc.format
                            .packed_byte_len(extent[0], extent[1])
                            .expect("Graph image byte length"),
                    )
                    .map_err(|_| size_error())?;
                    let (mip, rest) = bytes.split_at(size);
                    upload.image(
                        encoder,
                        texture,
                        desc,
                        ImageRegion {
                            mip_level,
                            origin: [0, 0],
                            extent,
                        },
                        mip,
                    )?;
                    bytes = rest;
                }
            }
        }
        Operation::CopyImage {
            src,
            dst,
            source,
            destination,
        } => {
            encoder.copy_texture_to_texture(
                image_copy(resources.texture(src)?, source, wgpu::TextureAspect::All),
                image_copy(
                    resources.texture(dst)?,
                    destination,
                    wgpu::TextureAspect::All,
                ),
                physical_extent(description(graph, src).format, source.extent),
            );
        }
        Operation::ReadbackBuffer { src, offset, size } => {
            let rows = Rows::bytes(size)?;
            let start = result.reserve(pass, rows, 4)?;
            encoder.copy_buffer_to_buffer(
                resources.buffer(src)?,
                offset,
                &result.buffer,
                start as u64,
                rows.span as u64,
            );
        }
        Operation::ReadbackImage { src } => {
            let desc = description(graph, src);
            let extent = [desc.width, desc.height];
            let rows = Rows::image(desc.format, extent)?;
            let offset = result.reserve(pass, rows, desc.format.block_bytes() as usize)?;
            let aspect = if desc.format == ImageFormat::Depth32Float {
                wgpu::TextureAspect::DepthOnly
            } else {
                wgpu::TextureAspect::All
            };
            encoder.copy_texture_to_buffer(
                image_copy(
                    resources.texture(src)?,
                    ImageRegion {
                        mip_level: 0,
                        origin: [0, 0],
                        extent,
                    },
                    aspect,
                ),
                rows.buffer(&result.buffer, offset),
                physical_extent(desc.format, extent),
            );
        }
        Operation::Declared(_) => {
            return Err(ExecutionError::webgpu(
                ExecutionErrorKind::Unsupported,
                "a live analysis-only pass cannot execute on WebGPU",
            ));
        }
        _ => unreachable!("raster and compute have dedicated WebGPU encoders"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_edge_mips_copy_blocks_and_return_only_packed_rows() {
        let rows = Rows::image(ImageFormat::Bc7RgbaUnorm, [7, 5]).unwrap();
        assert_eq!(
            (rows.packed, rows.pitch, rows.count, rows.span),
            (32, 256, 2, 288)
        );
        assert_eq!(
            physical_extent(ImageFormat::Bc7RgbaUnorm, [7, 5]),
            wgpu::Extent3d {
                width: 8,
                height: 8,
                depth_or_array_layers: 1
            }
        );
        assert_eq!(
            Rows::image(ImageFormat::Bc7RgbaUnorm, [1, 1]).unwrap().span,
            16
        );
    }

    #[test]
    fn mixed_readback_rows_and_odd_byte_buffers_reserve_disjoint_map_ranges() {
        let kind = RuntimeCapacityKind::ReadbackBytes;
        let depth = Rows::image(ImageFormat::Depth32Float, [3, 2]).unwrap();
        let first = staging_range(0, 8, depth.span, 280, kind).unwrap();
        let next = staging_range(first.end, 8, Rows::bytes(5).unwrap().span, 280, kind).unwrap();
        assert_eq!(first, 0..268);
        assert_eq!(next, 272..280);
        assert!(staging_range(first.end, 8, 8, 279, kind).is_err());
    }
}

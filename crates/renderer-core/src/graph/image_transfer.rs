//! Transfer rectangles in texels of one single-sample color mip.
use super::{
    AccessPurpose, Graph, GraphError, ImageAspect, ImageDesc, ImageHandle, Operation, PassHandle,
    ResourceAccess, SampleCount,
    builder::{read, write},
};
use std::ops::Range;

/// A nonempty rectangle in the selected mip of a single-layer image, in texels.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageRegion {
    pub mip_level: u32,
    pub origin: [u32; 2],
    pub extent: [u32; 2],
}

impl ImageRegion {
    /// Select an entire mip, including a final mip smaller than one compression block.
    pub fn mip(desc: ImageDesc, mip_level: u32) -> Result<Self, GraphError> {
        let extent = desc
            .mip_extent(mip_level)
            .ok_or_else(|| GraphError::invalid_input("unknown image mip"))?;
        Ok(Self {
            mip_level,
            origin: [0; 2],
            extent,
        })
    }

    fn validate(self, desc: ImageDesc) -> Result<bool, GraphError> {
        let mip = Self::mip(desc, self.mip_level)?;
        if desc.sample_count != SampleCount::X1
            || desc.array_layers != 1
            || desc.format.aspect() != ImageAspect::Color
        {
            return Err(GraphError::invalid_input(
                "region transfers require a single-sample, single-layer color image",
            ));
        }
        let block = desc.format.block_extent();
        for (axis, block) in block.into_iter().enumerate() {
            let start = self.origin[axis];
            let len = self.extent[axis];
            let Some(end) = start.checked_add(len) else {
                return Err(GraphError::invalid_input("image region overflows"));
            };
            // An incomplete block is legal only at the mip edge.
            if len == 0
                || end > mip.extent[axis]
                || !start.is_multiple_of(block)
                || (end != mip.extent[axis] && !len.is_multiple_of(block))
            {
                return Err(GraphError::invalid_input(
                    "image region exceeds its mip or splits a texel block",
                ));
            }
        }
        Ok(self == mip)
    }

    fn range(self) -> Range<u64> {
        let mip = u64::from(self.mip_level);
        mip..mip + 1
    }

    fn destination(self, id: usize, complete: bool) -> ResourceAccess {
        // Reading the previous contents here expresses preservation in Graph dataflow.
        // Native recording issues only the copy write, with the managed image layout.
        ResourceAccess {
            read: !complete,
            ..write(id, self.range(), AccessPurpose::CopyDestination)
        }
    }
}

impl Graph {
    /// Upload tightly packed native blocks into a mip. A whole-mip upload initializes
    /// it. A subrectangle preserves its surroundings and requires initialized mip content.
    pub fn upload_image_region(
        &mut self,
        name: &str,
        dst: ImageHandle,
        region: ImageRegion,
        bytes: &[u8],
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let dst = graph.image_id(dst)?;
            let desc = graph.image_desc_at(dst)?;
            let complete = region
                .validate(desc)
                .map_err(|e| graph.resource_context(e, dst))?;
            let [w, h] = region.extent;
            if desc.format.packed_byte_len(w, h) != Some(bytes.len() as u64) {
                return Err(graph.resource_context(
                    GraphError::invalid_input("upload bytes differ from the packed image region"),
                    dst,
                ));
            }
            let data = graph.append_upload(bytes)?;
            Ok((
                Operation::UploadImage {
                    dst,
                    region: Some(region),
                    data,
                },
                graph.append_accesses([region.destination(dst, complete)])?,
                false,
            ))
        })
    }

    /// Copy a rectangle between two distinct images of identical format, with no resampling.
    /// The source mip and any preserved destination texels must have defined contents.
    pub fn copy_image_region(
        &mut self,
        name: &str,
        src: ImageHandle,
        source: ImageRegion,
        dst: ImageHandle,
        dst_mip: u32,
        dst_origin: [u32; 2],
    ) -> Result<PassHandle, GraphError> {
        self.build_pass(name, |graph| {
            let src = graph.image_id(src)?;
            let dst = graph.image_id(dst)?;
            let src_desc = graph.image_desc_at(src)?;
            let dst_desc = graph.image_desc_at(dst)?;
            if src == dst || src_desc.format != dst_desc.format {
                return Err(graph.resource_context(
                    GraphError::invalid_input(
                        "region copy requires distinct images with the same format",
                    ),
                    dst,
                ));
            }
            source
                .validate(src_desc)
                .map_err(|e| graph.resource_context(e, src))?;
            let destination = ImageRegion {
                mip_level: dst_mip,
                origin: dst_origin,
                extent: source.extent,
            };
            let complete = destination
                .validate(dst_desc)
                .map_err(|e| graph.resource_context(e, dst))?;
            Ok((
                Operation::CopyImage {
                    src,
                    dst,
                    source,
                    destination,
                },
                graph.append_accesses([
                    read(src, source.range(), AccessPurpose::CopySource),
                    destination.destination(dst, complete),
                ])?,
                false,
            ))
        })
    }
}

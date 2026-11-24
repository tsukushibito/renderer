//! Image formats shared by Graph resources and managed shader interfaces.

/// GPU-independent texel representation used by image descriptions and shader
/// contracts. Format identity does not imply support for every usage: resource,
/// sampled-view, attachment and storage capabilities are checked separately.
/// Sampling these formats returns ordinary FP32 components; RGBA16F storage
/// does not require shader FP16 arithmetic support. BC blocks follow standard GPU block order.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum ImageFormat {
    /// Four unsigned normalized eight-bit components in RGBA order.
    Rgba8Unorm,
    /// Eight-bit RGBA with sRGB RGB components and linear alpha. Sampling
    /// decodes RGB before filtering; storage-image use is unsupported.
    Rgba8Srgb,
    /// Four IEEE 754 binary16 components in RGBA order.
    Rgba16Float,
    /// One IEEE 754 binary32 depth component.
    Depth32Float,
    /// Four FP32 components for sampled data and transfer.
    Rgba32Float,
    /// BC1 RGBA, including one-bit alpha.
    Bc1RgbaUnorm,
    Bc1RgbaSrgb,
    Bc3RgbaUnorm,
    Bc3RgbaSrgb,
    /// Two independent unsigned normalized channels, commonly used for normals.
    Bc5RgUnorm,
    /// Unsigned linear HDR RGB, sixteen bytes per 4x4 block.
    Bc6hRgbUfloat,
    Bc7RgbaUnorm,
    Bc7RgbaSrgb,
}

impl ImageFormat {
    pub const fn is_block_compressed(self) -> bool {
        matches!(
            self,
            Self::Bc1RgbaUnorm
                | Self::Bc1RgbaSrgb
                | Self::Bc3RgbaUnorm
                | Self::Bc3RgbaSrgb
                | Self::Bc5RgUnorm
                | Self::Bc6hRgbUfloat
                | Self::Bc7RgbaUnorm
                | Self::Bc7RgbaSrgb
        )
    }

    /// Logical texels per independently encoded block.
    pub const fn block_extent(self) -> [u32; 2] {
        if self.is_block_compressed() {
            [4, 4]
        } else {
            [1, 1]
        }
    }

    pub const fn block_bytes(self) -> u32 {
        match self {
            Self::Rgba8Unorm | Self::Rgba8Srgb | Self::Depth32Float => 4,
            Self::Rgba16Float | Self::Bc1RgbaUnorm | Self::Bc1RgbaSrgb => 8,
            Self::Rgba32Float
            | Self::Bc3RgbaUnorm
            | Self::Bc3RgbaSrgb
            | Self::Bc5RgUnorm
            | Self::Bc6hRgbUfloat
            | Self::Bc7RgbaUnorm
            | Self::Bc7RgbaSrgb => 16,
        }
    }

    /// Tightly packed bytes in one row of blocks at the given logical width.
    pub const fn packed_row_bytes(self, width: u32) -> u64 {
        width.div_ceil(self.block_extent()[0]) as u64 * self.block_bytes() as u64
    }

    /// Number of encoded block rows at the given logical height.
    pub const fn packed_rows(self, height: u32) -> u32 {
        height.div_ceil(self.block_extent()[1])
    }

    /// Size of one tightly packed mip level. Partial edge blocks occupy a full block.
    pub const fn packed_byte_len(self, width: u32, height: u32) -> Option<u64> {
        self.packed_row_bytes(width)
            .checked_mul(self.packed_rows(height) as u64)
    }

    pub(crate) const fn supports_storage(self) -> bool {
        matches!(self, Self::Rgba8Unorm | Self::Rgba16Float)
    }

    pub(crate) const fn supports_attachment(self) -> bool {
        matches!(
            self,
            Self::Rgba8Unorm | Self::Rgba16Float | Self::Depth32Float
        )
    }
}

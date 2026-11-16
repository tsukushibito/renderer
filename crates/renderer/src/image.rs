//! Image formats shared by Graph resources and managed shader interfaces.

/// GPU-independent texel representation used by image descriptions and shader
/// contracts. Format identity does not imply support for every usage: resource,
/// sampled-view, attachment and storage capabilities are checked separately.
/// Sampling these formats returns ordinary FP32 components; RGBA16F storage
/// does not require shader FP16 arithmetic support.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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
}

impl ImageFormat {
    pub(crate) const fn texel_bytes(self) -> u64 {
        match self {
            Self::Rgba8Unorm | Self::Rgba8Srgb | Self::Depth32Float => 4,
            Self::Rgba16Float => 8,
        }
    }
}

//! Normalized-coordinate 2D sampling. Samplers can be shared across images.
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum AddressMode {
    Repeat,
    ClampToEdge,
    MirroredRepeat,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum FilterMode {
    Nearest,
    Linear,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub enum MipFilter {
    /// Sample only the selected view's base mip.
    None,
    Nearest,
    Linear,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct SamplerDescriptor {
    pub wrap_u: AddressMode,
    pub wrap_v: AddressMode,
    pub mag_filter: FilterMode,
    pub min_filter: FilterMode,
    pub mip_filter: MipFilter,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SamplerError {
    pub field: &'static str,
    pub value: u32,
}
impl fmt::Display for SamplerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unsupported glTF sampler {} value {}",
            self.field, self.value
        )
    }
}
impl std::error::Error for SamplerError {}

impl Default for SamplerDescriptor {
    fn default() -> Self {
        Self::LinearRepeat
    }
}

#[allow(non_upper_case_globals)]
impl SamplerDescriptor {
    pub const NearestClamp: Self = Self {
        wrap_u: AddressMode::ClampToEdge,
        wrap_v: AddressMode::ClampToEdge,
        min_filter: FilterMode::Nearest,
        mag_filter: FilterMode::Nearest,
        mip_filter: MipFilter::None,
    };
    pub const LinearRepeat: Self = Self {
        wrap_u: AddressMode::Repeat,
        wrap_v: AddressMode::Repeat,
        min_filter: FilterMode::Linear,
        mag_filter: FilterMode::Linear,
        mip_filter: MipFilter::Linear,
    };
    pub const LinearClamp: Self = Self {
        wrap_u: AddressMode::ClampToEdge,
        wrap_v: AddressMode::ClampToEdge,
        ..Self::LinearRepeat
    };
    pub const LinearRepeatUClampV: Self = Self {
        wrap_v: AddressMode::ClampToEdge,
        ..Self::LinearRepeat
    };

    /// Number of distinct normalized policies. Used to bound native cache storage.
    pub const COUNT: usize = 3 * 3 * 2 * 2 * 3;

    /// Dense local cache index. Serialized assets store the individual fields.
    pub const fn index(self) -> usize {
        ((((self.wrap_u as usize * 3 + self.wrap_v as usize) * 2 + self.mag_filter as usize) * 2
            + self.min_filter as usize)
            * 3)
            + self.mip_filter as usize
    }

    /// Import the glTF 2.0 sampler enum values. The renderer's default filter
    /// for an omitted glTF filter is linear, with interpolation between mips.
    pub fn from_gltf(
        wrap_s: Option<u32>,
        wrap_t: Option<u32>,
        mag_filter: Option<u32>,
        min_filter: Option<u32>,
    ) -> Result<Self, SamplerError> {
        fn wrap(value: Option<u32>, field: &'static str) -> Result<AddressMode, SamplerError> {
            match value {
                Some(10497) | None => Ok(AddressMode::Repeat),
                Some(33071) => Ok(AddressMode::ClampToEdge),
                Some(33648) => Ok(AddressMode::MirroredRepeat),
                Some(value) => Err(SamplerError { field, value }),
            }
        }
        let mag_filter = match mag_filter {
            Some(9728) => FilterMode::Nearest,
            Some(9729) | None => FilterMode::Linear,
            Some(value) => {
                return Err(SamplerError {
                    field: "magFilter",
                    value,
                });
            }
        };
        let (min_filter, mip_filter) = match min_filter {
            Some(9728) => (FilterMode::Nearest, MipFilter::None),
            Some(9729) => (FilterMode::Linear, MipFilter::None),
            Some(9984) => (FilterMode::Nearest, MipFilter::Nearest),
            Some(9985) => (FilterMode::Linear, MipFilter::Nearest),
            Some(9986) => (FilterMode::Nearest, MipFilter::Linear),
            Some(9987) | None => (FilterMode::Linear, MipFilter::Linear),
            Some(value) => {
                return Err(SamplerError {
                    field: "minFilter",
                    value,
                });
            }
        };
        Ok(Self {
            wrap_u: wrap(wrap_s, "wrapS")?,
            wrap_v: wrap(wrap_t, "wrapT")?,
            mag_filter,
            min_filter,
            mip_filter,
        })
    }

    #[cfg(any(feature = "vulkan", all(feature = "d3d12", windows)))]
    pub(crate) const fn is_linear(self) -> bool {
        matches!(self.mag_filter, FilterMode::Linear)
            || matches!(self.min_filter, FilterMode::Linear)
            || matches!(self.mip_filter, MipFilter::Linear)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn imports_all_gltf_combinations_without_cache_aliasing() {
        let mut seen = [false; SamplerDescriptor::COUNT];
        for s in [10497, 33071, 33648] {
            for t in [10497, 33071, 33648] {
                for mag in [9728, 9729] {
                    for min in [9728, 9729, 9984, 9985, 9986, 9987] {
                        let policy =
                            SamplerDescriptor::from_gltf(Some(s), Some(t), Some(mag), Some(min))
                                .unwrap();
                        assert!(!std::mem::replace(&mut seen[policy.index()], true));
                    }
                }
            }
        }
        assert!(seen.into_iter().all(|entry| entry));
        assert_eq!(
            SamplerDescriptor::from_gltf(None, None, None, None),
            Ok(SamplerDescriptor::LinearRepeat)
        );
        assert_eq!(
            SamplerDescriptor::from_gltf(None, None, None, Some(9988)).unwrap_err(),
            SamplerError {
                field: "minFilter",
                value: 9988
            }
        );
    }
}

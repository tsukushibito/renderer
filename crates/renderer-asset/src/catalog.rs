use crate::invalid;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest as _, Sha256};
use std::{collections::BTreeMap, fmt, fs, io, path::Path, str::FromStr};

fn parse_hex<const N: usize>(s: &str) -> io::Result<[u8; N]> {
    fn digit(b: u8) -> io::Result<u8> {
        match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(invalid("expected lowercase hexadecimal")),
        }
    }
    if s.len() != N * 2 {
        return Err(invalid("incorrect hexadecimal identifier length"));
    }
    let mut result = [0; N];
    for (dst, pair) in result.iter_mut().zip(s.as_bytes().as_chunks::<2>().0) {
        *dst = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Ok(result)
}

macro_rules! identifier {
    ($name:ident, $len:literal) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name([u8; $len]);
        impl FromStr for $name {
            type Err = io::Error;
            fn from_str(s: &str) -> io::Result<Self> {
                Ok(Self(parse_hex(s)?))
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in &self.0 {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
        }
        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.collect_str(self)
            }
        }
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct ReadId;
                impl serde::de::Visitor<'_> for ReadId {
                    type Value = $name;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("a lowercase hexadecimal identifier")
                    }
                    fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Self::Value, E> {
                        s.parse().map_err(E::custom)
                    }
                }
                d.deserialize_str(ReadId)
            }
        }
    };
}

identifier!(AssetId, 16);
identifier!(Digest, 32);

impl AssetId {
    pub const fn from_u128(value: u128) -> Self {
        Self(value.to_be_bytes())
    }
}

impl Digest {
    pub fn of(bytes: &[u8]) -> Self {
        Self(Sha256::digest(bytes).into())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BlobRef {
    pub hash: Digest,
    pub size: u64,
}

impl BlobRef {
    pub fn of(bytes: &[u8]) -> Self {
        Self {
            hash: Digest::of(bytes),
            size: bytes.len() as u64,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Primitive {
    pub mesh: BlobRef,
    pub material: BlobRef,
    /// Base color, normal, occlusion/roughness/metallic, and emission respectively.
    pub images: [Option<BlobRef>; 4],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Equipment {
    pub primitives: Vec<Primitive>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub version: u32,
    #[serde(deserialize_with = "read_assets")]
    pub assets: BTreeMap<AssetId, Equipment>,
}

// An ambiguous ID must never silently replace another definition while importing JSON.
fn read_assets<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<AssetId, Equipment>, D::Error> {
    struct Assets;
    impl<'de> serde::de::Visitor<'de> for Assets {
        type Value = BTreeMap<AssetId, Equipment>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("unique equipment IDs")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> Result<Self::Value, M::Error> {
            let mut assets = BTreeMap::new();
            while let Some((id, equipment)) = map.next_entry()? {
                if assets.insert(id, equipment).is_some() {
                    return Err(serde::de::Error::custom(format!(
                        "duplicate equipment {id}"
                    )));
                }
            }
            Ok(assets)
        }
    }
    deserializer.deserialize_map(Assets)
}

/// Pins one validated catalog. Resolution never reopens the mutable current pointer.
#[derive(Debug)]
pub struct Generation {
    id: Digest,
    catalog: Catalog,
    #[cfg(feature = "metrics")]
    lookups: std::sync::atomic::AtomicU64,
}

impl Generation {
    pub fn open(root: impl AsRef<Path>) -> io::Result<Self> {
        let root = root.as_ref();
        let current = fs::read_to_string(root.join("current"))?;
        Self::open_id(root, current.trim_end().parse()?)
    }

    pub fn open_id(root: impl AsRef<Path>, id: Digest) -> io::Result<Self> {
        let bytes = fs::read(root.as_ref().join("catalogs").join(id.to_string()))?;
        if Digest::of(&bytes) != id {
            return Err(invalid("catalog digest mismatch"));
        }
        let catalog: Catalog = serde_json::from_slice(&bytes).map_err(invalid)?;
        if catalog.version != 1 {
            return Err(invalid(format!(
                "unsupported catalog version {}",
                catalog.version
            )));
        }
        for (asset, equipment) in &catalog.assets {
            if equipment.primitives.is_empty() {
                return Err(invalid(format!("no primitives for equipment {asset}")));
            }
        }
        Ok(Self {
            id,
            catalog,
            #[cfg(feature = "metrics")]
            lookups: Default::default(),
        })
    }

    pub fn id(&self) -> &Digest {
        &self.id
    }
    pub fn equipment(&self, id: &AssetId) -> io::Result<&Equipment> {
        #[cfg(feature = "metrics")]
        self.lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.catalog.assets.get(id).ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, format!("unknown equipment {id}"))
        })
    }
    #[cfg(feature = "metrics")]
    pub fn lookups(&self) -> u64 {
        self.lookups.load(std::sync::atomic::Ordering::Relaxed)
    }
}

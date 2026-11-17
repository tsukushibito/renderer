//! Consumer-owned IBL publication boundary. No file access or payload scans occur
//! while drawing. Upload owns each immutable byte chain; CPU pixels may then be
//! released explicitly, while identity and dimensions remain available.

use std::{
    error::Error,
    ops::Range,
    path::{Component, Path},
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const SOURCE: &str = "5eb82f21bced3f7e901774083cfee1a05fd2bcf22f56ff699f3171c1eec65a50";
const MAPPING: &str = "u=fract(.5+atan2(z,x)/(2*pi));v=acos(y)/pi;top-origin;U-repeat/V-clamp";

pub(super) struct Environment {
    pub diffuse: Image,
    pub specular: Image,
    pub brdf: Image,
    pub strength: f32,
    pub yaw_degrees: f32,
    pub max_prefilter_lod: u32,
    pub identity: Value,
}

pub(super) struct Image {
    pub width: u32,
    pub height: u32,
    pub mips: Vec<Range<usize>>,
    pub bytes: Vec<u8>,
}

fn invalid(message: &str) -> Box<dyn Error> {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message).into()
}

fn require(condition: bool, message: &str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(invalid(message))
    }
}

fn tool_hash() -> String {
    // Match Python universal-newline source identity on every checkout OS.
    let source = include_str!("../../../../../tools/bistro/ibl.py").replace("\r\n", "\n");
    format!("{:x}", Sha256::digest(source.as_bytes()))
}

fn integer(value: &Value) -> Result<usize> {
    value
        .as_u64()
        .and_then(|v| usize::try_from(v).ok())
        .ok_or_else(|| invalid("invalid IBL integer"))
}

fn exact(value: &Value, expected: Value, message: &str) -> Result<()> {
    require(*value == expected, message)
}

fn load_image(
    root: &Path,
    record: &Value,
    width: u32,
    height: u32,
    levels: usize,
) -> Result<Image> {
    exact(
        &record["format"],
        json!("rgba16f-le"),
        "IBL image must be linear RGBA16F little-endian",
    )?;
    exact(
        &record["width"],
        json!(width),
        "IBL image width differs from fixed recipe",
    )?;
    exact(
        &record["height"],
        json!(height),
        "IBL image height differs from fixed recipe",
    )?;
    exact(
        &record["mip_levels"],
        json!(levels),
        "IBL mip count differs from fixed recipe",
    )?;
    let mut mips = Vec::with_capacity(levels);
    let mut end = 0usize;
    for mip in 0..levels {
        let size = usize::try_from((width >> mip).max(1))?
            .checked_mul(usize::try_from((height >> mip).max(1))?)
            .and_then(|v| v.checked_mul(8))
            .ok_or_else(|| invalid("IBL mip byte overflow"))?;
        let start = end;
        end = end
            .checked_add(size)
            .ok_or_else(|| invalid("IBL image byte overflow"))?;
        mips.push(start..end);
    }
    require(
        integer(&record["byte_length"])? == end,
        "IBL packed byte length disagrees with mip shapes",
    )?;
    let name = record["path"]
        .as_str()
        .ok_or_else(|| invalid("missing IBL relative image path"))?;
    let relative = Path::new(name);
    require(
        !relative.as_os_str().is_empty()
            && relative
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "IBL image path must contain only normal relative components",
    )?;
    let path = root.join(relative).canonicalize()?;
    require(
        path.starts_with(root),
        "IBL image path escapes manifest directory",
    )?;
    require(
        std::fs::metadata(&path)?.len() == u64::try_from(end)?,
        "IBL image file has wrong byte length",
    )?;
    let bytes = std::fs::read(path)?;
    require(bytes.len() == end, "IBL image changed length during load")?;
    exact(
        &record["sha256"],
        json!(format!("{:x}", Sha256::digest(&bytes))),
        "IBL image SHA-256 mismatch",
    )?;
    for texel in bytes.as_chunks::<8>().0 {
        for channel in texel.as_chunks::<2>().0.iter().take(3) {
            let word = u16::from_le_bytes(*channel);
            require(
                word & 0x7c00 != 0x7c00 && (word & 0x8000 == 0 || word & 0x7fff == 0),
                "IBL radiance/BRDF must be finite and nonnegative",
            )?;
        }
        require(
            u16::from_le_bytes([texel[6], texel[7]]) == 0x3c00,
            "IBL alpha must be exactly one",
        )?;
    }
    Ok(Image {
        width,
        height,
        mips,
        bytes,
    })
}

impl Environment {
    pub fn load(manifest: &Path) -> Result<Self> {
        let path = manifest.canonicalize()?;
        let root = path
            .parent()
            .ok_or_else(|| invalid("IBL manifest directory missing"))?;
        let bytes = std::fs::read(&path)?;
        let mut value: Value = serde_json::from_slice(&bytes)?;
        exact(
            &value["schema_version"],
            json!(1),
            "unsupported IBL manifest schema",
        )?;
        exact(
            &value["max_prefilter_lod"],
            json!(4),
            "IBL maximum LOD must retain directional resolution",
        )?;
        exact(
            &value["source"]["sha256"],
            json!(SOURCE),
            "IBL source is not the fixed Bistro HDR",
        )?;
        exact(
            &value["source"]["width"],
            json!(4096),
            "IBL source width mismatch",
        )?;
        exact(
            &value["source"]["height"],
            json!(2048),
            "IBL source height mismatch",
        )?;
        exact(
            &value["source"]["byte_length"],
            json!(23_695_717),
            "IBL source length mismatch",
        )?;
        exact(
            &value["lighting"]["strength"],
            json!(3),
            "IBL strength must be the fixed value 3",
        )?;
        exact(
            &value["lighting"]["yaw_degrees"],
            json!(0),
            "IBL yaw must be the fixed value zero",
        )?;
        let recipe = &value["recipe"];
        for (key, expected) in [
            ("version", json!(1)),
            ("mapping", json!(MAPPING)),
            ("cap", json!(4)),
            ("strength_applied", json!(false)),
            ("yaw_applied", json!(false)),
            (
                "diffuse_storage",
                json!("irradiance E; shader divides by pi"),
            ),
            ("brdf_storage", json!("A,B,0,1; f0*A+B")),
            (
                "distribution",
                json!("isotropic GGX; alpha=max(roughness^2,1e-3)"),
            ),
            ("visibility", json!("height-correlated Smith")),
            ("sequence", json!("Hammersley base2")),
            ("diffuse_size", json!([128, 64])),
            ("specular_size", json!([1024, 512])),
            ("brdf_size", json!([256, 256])),
            (
                "specular_roughness",
                json!("min(mip,max_prefilter_lod)/max_prefilter_lod"),
            ),
            ("max_prefilter_lod", json!(4)),
            ("source_cap_arithmetic", json!("float32 before resampling")),
            ("integration_arithmetic", json!("float64")),
            ("numpy", json!("1.26.4")),
            ("tool_sha256_utf8_lf", json!(tool_hash())),
        ] {
            exact(
                &recipe[key],
                expected,
                "IBL recipe differs from the reviewed cook",
            )?;
        }
        require(
            matches!(integer(&recipe["samples"])?, 1024 | 4096 | 16384),
            "unsupported IBL sample count",
        )?;
        exact(
            &recipe["decoder"]["version"],
            json!([4, 5, 16]),
            "IBL decoder version mismatch",
        )?;
        exact(
            &recipe["decoder"]["build"],
            json!("3f6797307da5"),
            "IBL decoder build mismatch",
        )?;
        exact(
            &recipe["decoder"]["numpy"],
            json!("1.26.4"),
            "IBL decoder NumPy mismatch",
        )?;
        let diffuse = load_image(root, &value["images"]["diffuse"], 128, 64, 1)?;
        let specular = load_image(root, &value["images"]["specular"], 1024, 512, 11)?;
        let brdf = load_image(root, &value["images"]["brdf"], 256, 256, 1)?;
        value["manifest_sha256"] = json!(format!("{:x}", Sha256::digest(bytes)));
        Ok(Self {
            diffuse,
            specular,
            brdf,
            strength: 3.,
            yaw_degrees: 0.,
            max_prefilter_lod: 4,
            identity: value,
        })
    }

    /// Small deterministic fixture: B2's 0.03 diffuse fill, no specular light.
    /// The BRDF LUT is cooked with the same 1024-sample correlated Smith recipe.
    pub fn fixture() -> Self {
        let diffuse = Image::constant(8, 4, 1, [0x2e08, 0x2e08, 0x2e08, 0x3c00]);
        let specular = Image::constant(8, 4, 4, [0, 0, 0, 0x3c00]);
        let brdf = Image {
            width: 16,
            height: 16,
            mips: std::iter::once(0..2048).collect(),
            bytes: include_bytes!("ibl-fixture-brdf.rgba16f").to_vec(),
        };
        Self {
            diffuse,
            specular,
            brdf,
            strength: 1.,
            yaw_degrees: 0.,
            max_prefilter_lod: 3,
            identity: json!({"kind":"fixture-constant-diffuse", "strength":1, "yaw_degrees":0,"max_prefilter_lod":3,
                "diffuse_irradiance_half":0x2e08, "specular":0,
                "brdf_samples":1024, "brdf_sha256":format!("{:x}", Sha256::digest(include_bytes!("ibl-fixture-brdf.rgba16f")))}),
        }
    }

    pub fn images(&self) -> [&Image; 3] {
        [&self.diffuse, &self.specular, &self.brdf]
    }

    /// Explicit preparation boundary; no CPU image bytes survive into the ROI.
    pub fn release_pixels(&mut self) {
        for image in [&mut self.diffuse, &mut self.specular, &mut self.brdf] {
            image.bytes = Vec::new();
        }
    }
}

impl Image {
    fn constant(width: u32, height: u32, levels: usize, rgba: [u16; 4]) -> Self {
        let mut bytes = Vec::new();
        let mut mips = Vec::with_capacity(levels);
        for mip in 0..levels {
            let start = bytes.len();
            for _ in 0..(width >> mip).max(1) * (height >> mip).max(1) {
                for word in rgba {
                    bytes.extend_from_slice(&word.to_le_bytes());
                }
            }
            mips.push(start..bytes.len());
        }
        Self {
            width,
            height,
            mips,
            bytes,
        }
    }

    pub fn byte_length(&self) -> usize {
        self.mips.last().map_or(0, |mip| mip.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn fixture_has_complete_chains_and_explicit_pixel_release() {
        let mut environment = Environment::fixture();
        let lengths = environment.images().map(Image::byte_length);
        assert_eq!(lengths, [256, 344, 2048]);
        assert_eq!(environment.specular.mips.len(), 4);
        environment.release_pixels();
        assert!(
            environment
                .images()
                .iter()
                .all(|image| image.bytes.is_empty())
        );
        assert_eq!(environment.images().map(Image::byte_length), lengths);
        assert_eq!(environment.strength, 1.);
        assert_eq!(environment.yaw_degrees, 0.);
        assert_eq!(environment.diffuse.width, 8);
        assert_eq!(environment.diffuse.height, 4);
        assert_eq!(environment.identity["kind"], "fixture-constant-diffuse");
    }

    #[test]
    fn image_boundary_checks_shape_hash_path_and_half_payload() {
        let temporary = std::env::temp_dir().join(format!(
            "renderer-ibl-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&temporary).unwrap();
        let root = temporary.canonicalize().unwrap();
        let image = Image::constant(2, 2, 2, [0x3c00; 4]);
        let path = root.join("pixels.bin");
        std::fs::write(&path, &image.bytes).unwrap();
        let mut record = json!({"path":"pixels.bin","format":"rgba16f-le","width":2,"height":2,
            "mip_levels":2,"byte_length":40,"sha256":format!("{:x}",Sha256::digest(&image.bytes))});
        assert_eq!(
            load_image(&root, &record, 2, 2, 2).unwrap().bytes,
            image.bytes
        );
        for (key, wrong) in [
            ("path", json!("../pixels.bin")),
            ("format", json!("rgba8")),
            ("width", json!(1)),
            ("byte_length", json!(32)),
            ("sha256", json!("wrong")),
        ] {
            let mut changed = record.clone();
            changed[key] = wrong;
            assert!(load_image(&root, &changed, 2, 2, 2).is_err());
        }
        for word in [0x7c00u16, 0x7e00, 0xbc00] {
            let mut changed = image.bytes.clone();
            changed[..2].copy_from_slice(&word.to_le_bytes());
            std::fs::write(&path, &changed).unwrap();
            record["sha256"] = json!(format!("{:x}", Sha256::digest(&changed)));
            assert!(load_image(&root, &record, 2, 2, 2).is_err());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn manifest_boundary_requires_reviewed_recipe_and_complete_assets() {
        let temporary = std::env::temp_dir().join(format!(
            "renderer-ibl-manifest-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&temporary).unwrap();
        let root = temporary.canonicalize().unwrap();
        let mut images = serde_json::Map::new();
        for (name, width, height, levels) in [
            ("diffuse", 128, 64, 1),
            ("specular", 1024, 512, 11),
            ("brdf", 256, 256, 1),
        ] {
            let image = Image::constant(width, height, levels, [0, 0, 0, 0x3c00]);
            let name_file = format!("{name}.bin");
            std::fs::write(root.join(&name_file), &image.bytes).unwrap();
            images.insert(name.into(),json!({"path":name_file,"format":"rgba16f-le","width":width,"height":height,
                "mip_levels":levels,"byte_length":image.bytes.len(),"sha256":format!("{:x}",Sha256::digest(&image.bytes))}));
        }
        let manifest = json!({"schema_version":1,"max_prefilter_lod":4,"source":{"sha256":SOURCE,"width":4096,"height":2048,"byte_length":23_695_717},
            "lighting":{"strength":3,"yaw_degrees":0},"images":images,
            "recipe":{"version":1,"mapping":MAPPING,"cap":4,"strength_applied":false,"yaw_applied":false,
                "diffuse_storage":"irradiance E; shader divides by pi","brdf_storage":"A,B,0,1; f0*A+B",
                "distribution":"isotropic GGX; alpha=max(roughness^2,1e-3)","visibility":"height-correlated Smith",
                "sequence":"Hammersley base2","diffuse_size":[128,64],"specular_size":[1024,512],"brdf_size":[256,256],
                "specular_roughness":"min(mip,max_prefilter_lod)/max_prefilter_lod","max_prefilter_lod":4,"source_cap_arithmetic":"float32 before resampling",
                "integration_arithmetic":"float64","numpy":"1.26.4","samples":1024,"tool_sha256_utf8_lf":tool_hash(),
                "decoder":{"version":[4,5,16],"build":"3f6797307da5","numpy":"1.26.4"}}});
        let path = root.join("manifest.json");
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let loaded = Environment::load(&path).unwrap();
        assert_eq!(loaded.specular.mips.len(), 11);
        assert_eq!(loaded.strength, 3.);
        assert_eq!(loaded.yaw_degrees, 0.);
        assert_eq!(loaded.brdf.bytes.len(), 524_288);
        for (pointer, wrong) in [
            ("/schema_version", json!(2)),
            ("/max_prefilter_lod", json!(10)),
            ("/source/sha256", json!("wrong")),
            ("/lighting/strength", json!(9)),
            ("/recipe/cap", json!(8)),
            ("/recipe/strength_applied", json!(true)),
            ("/recipe/tool_sha256_utf8_lf", json!("wrong")),
            ("/recipe/samples", json!(1000)),
            ("/recipe/decoder/build", json!("wrong")),
            ("/images/specular/mip_levels", json!(1)),
        ] {
            let mut changed = manifest.clone();
            *changed.pointer_mut(pointer).unwrap() = wrong;
            std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(
                Environment::load(&path).is_err(),
                "unexpectedly accepted {pointer}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }
}

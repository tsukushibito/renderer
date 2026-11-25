//! HTTP transport for the existing cooked scene, environment and lighting decoders.
//! The scene descriptor pins the manifests. Each nested payload is addressed by its digest.
use super::{asset, baked_assets, camera, ibl};
use crate::Error;
use js_sys::{Function, Uint8Array};
use sashiko_renderer::pbr::{LightmapAddress, ReflectionProbes, SurfaceLightmapIndex};
use sashiko_renderer_core::ImageFormat;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Response, Url};

pub(crate) struct Object {
    url: String,
    hash: String,
    size: usize,
}

fn js(error: JsValue) -> Error {
    js_sys::Error::from(error)
        .to_string()
        .as_string()
        .unwrap_or_else(|| "JavaScript failure".into())
        .into()
}

async fn get(url: &str) -> Result<Vec<u8>, Error> {
    let window = web_sys::window().ok_or("scene loading requires a browser window")?;
    let response: Response = JsFuture::from(window.fetch_with_str(url))
        .await
        .map_err(|e| format!("fetch {url}: {}", js(e)))?
        .dyn_into()
        .map_err(js)?;
    if !response.ok() {
        return Err(format!("HTTP {} fetching {url}", response.status()).into());
    }
    let buffer = JsFuture::from(response.array_buffer().map_err(js)?)
        .await
        .map_err(js)?;
    Ok(Uint8Array::new(&buffer).to_vec())
}

impl Object {
    fn new(base: &str, record: &Value) -> Result<Self, Error> {
        let hash = record["sha256"].as_str().ok_or("missing asset digest")?;
        let size = usize::try_from(
            record["byte_length"]
                .as_u64()
                .ok_or("missing asset byte length")?,
        )?;
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        {
            return Err("invalid scene object SHA-256".into());
        }
        let url = Url::new_with_base(&format!("objects/{hash}"), base)
            .map_err(js)?
            .href();
        Ok(Self {
            url,
            hash: hash.into(),
            size,
        })
    }

    pub(super) async fn fetch(&self) -> Result<Vec<u8>, Error> {
        let bytes = get(&self.url).await?;
        if bytes.len() != self.size || format!("{:x}", Sha256::digest(&bytes)) != self.hash {
            return Err(format!("invalid size or digest for {}", self.url).into());
        }
        Ok(bytes)
    }
}

pub(crate) struct WebAssets {
    pub scene: asset::Scene,
    pub environment: ibl::Environment,
    pub lighting: WebLightmaps,
    pub look: &'static str,
    pub view: String,
}

pub(crate) struct WebLightmaps {
    base: String,
    index: SurfaceLightmapIndex,
}

impl WebLightmaps {
    async fn object(&self, hash: &[u8; 32]) -> Result<Vec<u8>, Error> {
        lighting_object(&self.base, &baked_assets::object_name(hash)).await
    }

    pub async fn page(&self, id: usize) -> Result<Vec<u8>, Error> {
        let record = self.index.pages().get(id).ok_or("unknown lightmap page")?;
        let bytes = self.object(record.sha256()).await?;
        let mut payload = Some(bytes);
        let (pages, _, _) = self
            .index
            .load([id], None, |_| {
                payload
                    .take()
                    .ok_or_else(|| "lightmap page read twice".into())
            })?
            .into_parts();
        Ok(pages
            .into_iter()
            .next()
            .ok_or("missing lightmap page")?
            .into_rgba32f())
    }
}

async fn lighting_object(base: &str, hash: &str) -> Result<Vec<u8>, Error> {
    get(&Url::new_with_base(&format!("objects/{hash}"), base)
        .map_err(js)?
        .href())
    .await
}

async fn scene(base: &str, bytes: &[u8], alpha: &Value) -> Result<asset::Scene, Error> {
    let doc: Value = serde_json::from_slice(bytes)?;
    let mut geometry = HashMap::new();
    for key in ["vertices", "indices", "lightmap_uv"] {
        if !doc[key].is_null() {
            let object = Object::new(base, &doc[key])?;
            let contents = object.fetch().await?;
            geometry.insert(key, contents);
        }
    }
    asset::Scene::decode(
        bytes,
        &doc,
        false,
        |key| {
            geometry
                .remove(key)
                .ok_or_else(|| "missing geometry payload".into())
        },
        |r| {
            let object = Object::new(base, r)?;
            let maximum_alpha = alpha
                .get(&object.hash)
                .map(|v| {
                    u8::try_from(v.as_u64().ok_or("invalid alpha summary")?).map_err(Error::from)
                })
                .transpose()?;
            Ok(asset::Pixels::Http {
                object,
                maximum_alpha,
            })
        },
    )
}

async fn environment(base: &str, bytes: &[u8]) -> Result<ibl::Environment, Error> {
    let doc: Value = serde_json::from_slice(bytes)?;
    let mut images = HashMap::new();
    for key in ["brdf", "diffuse", "specular"] {
        let object = Object::new(base, &doc["images"][key])?;
        let data = get(&object.url).await?;
        // decode_image checks the byte length, digest, packed mips and half floats.
        images.insert(object.hash, data);
    }
    ibl::Environment::decode(bytes, |record, w, h, n| {
        let data = images
            .remove(record["sha256"].as_str().ok_or("missing IBL digest")?)
            .ok_or("missing IBL payload")?;
        ibl::decode_image(record, w, h, n, || Ok(data))
    })
}

fn image(
    format: ImageFormat,
    [width, height]: [u32; 2],
    bytes: Vec<u8>,
    length: usize,
) -> ibl::Image {
    ibl::Image {
        format,
        width,
        height,
        mips: std::iter::once(0..length).collect(),
        bytes,
    }
}

pub(crate) async fn load(url: &str, progress: &Function) -> Result<WebAssets, Error> {
    let notify = |label: &str| {
        progress
            .call1(&JsValue::NULL, &JsValue::from_str(label))
            .map_err(js)
    };
    notify("Loading scene metadata")?;
    let location = web_sys::window()
        .ok_or("missing browser window")?
        .location()
        .href()
        .map_err(js)?;
    let base = Url::new_with_base(url, &location).map_err(js)?.href();
    let descriptor: Value = serde_json::from_slice(&get(&base).await?)?;
    if descriptor["schema"] != "sashiko-web-scene-v1" {
        return Err("unsupported web scene descriptor".into());
    }
    let (look, strength) = match descriptor["look"].as_str() {
        Some("o3de-atrium") => ("o3de-atrium", 1.),
        Some("lumberyard-day") => ("lumberyard-day", camera::DAY_IBL_STRENGTH),
        _ => return Err("unsupported lighting preset".into()),
    };
    let view = descriptor["defaultView"]
        .as_str()
        .ok_or("missing default scene view")?
        .to_owned();
    notify("Loading geometry")?;
    let manifest = Object::new(&base, &descriptor["geometry"])?;
    let mut scene = scene(&base, &manifest.fetch().await?, &descriptor["maximumAlpha"]).await?;
    notify("Loading environment and baked lighting index")?;
    let bytes = Object::new(&base, &descriptor["environment"])?
        .fetch()
        .await?;
    let mut env = environment(&base, &bytes).await?;
    env.nits_per_unit = strength;
    let bake = camera::bake_identity(&manifest.hash, look.parse()?);
    let hdr = camera::environment_bake_identity(&bake, &env).ok_or("missing lighting identity")?;
    let matches = |id: &[u8]| id == hdr.as_bytes() || id == bake.as_bytes();

    let (identity, index) = baked_assets::LightingGeneration::decode_manifest(
        &Object::new(&base, &descriptor["lighting"])?.fetch().await?,
    )?;
    if !matches(identity.as_bytes()) {
        return Err("lighting generation belongs to a different scene or look".into());
    }
    let hash = baked_assets::object_name(&index);
    let data = lighting_object(&base, &hash).await?;
    if format!("{:x}", Sha256::digest(&data)) != hash {
        return Err("corrupt lighting index".into());
    }
    let index = SurfaceLightmapIndex::from_bytes(&data)?;
    let lighting = WebLightmaps {
        base: base.clone(),
        index,
    };
    for (page, record) in lighting.index.pages().iter().enumerate() {
        let [w, h] = record.extent();
        let size = (u64::from(w) * u64::from(h) * 16).try_into()?;
        env.lightmaps
            .push(image(ImageFormat::Rgba32Float, [w, h], Vec::new(), size));
        for (i, id) in record.placements().iter().enumerate() {
            let p = scene
                .primitives
                .get_mut(*id as usize)
                .ok_or("unknown baked primitive")?;
            if scene.materials[p.material].blend || p.lightmap.is_none() {
                return Err("invalid baked placement".into());
            }
            p.surface = Some(LightmapAddress {
                page,
                slot: u32::try_from(i + 1)?,
            });
        }
    }
    if scene.primitives.iter().any(|p| {
        let m = &scene.materials[p.material];
        !m.blend && !m.alpha_zero && p.surface.is_none()
    }) {
        return Err("incomplete baked surface coverage".into());
    }
    let probes = lighting.object(lighting.index.probe_sha256()).await?;
    let (_, _, volume) = lighting
        .index
        .load([], Some(&probes), |_| Err("unexpected page fetch".into()))?
        .into_parts();
    let volume = volume.ok_or("missing irradiance probes")?;
    env.identity["diffuse_scenarios"] = json!(volume.scenario_count());
    env.identity["baked_lighting_sha256"] = json!(hash);
    let extent = volume.extent();
    let pixels = volume.into_rgba32f();
    let size = pixels.len();
    env.diffuse = image(ImageFormat::Rgba32Float, extent, pixels, size);

    let reflection = Object::new(&base, &descriptor["reflections"])?
        .fetch()
        .await?;
    let (id, file) = reflection
        .split_at_checked(64)
        .ok_or("truncated reflection file")?;
    if !matches(id) {
        return Err("reflection probes belong to a different scene or look".into());
    }
    let atlas = ReflectionProbes::from_bytes(file)?;
    env.reflection_metadata = Some(Box::new(atlas.metadata()));
    let format = atlas.image_desc().format;
    let extent = atlas.extent();
    let pixels = atlas.into_pixels();
    let size = pixels.len();
    env.reflection_probes = Some(image(format, extent, pixels, size));
    env.identity["reflection_probes_sha256"] = json!(format!("{:x}", Sha256::digest(file)));
    Ok(WebAssets {
        scene,
        environment: env,
        look,
        view,
        lighting,
    })
}

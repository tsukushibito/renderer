//! Static PBRT v4 import. Resolve source files relative to the scene root,
//! preserve graphics state across includes, and keep object definitions local.
mod ply;
mod surface;
mod syntax;

use crate::{Error, scene::object};
use glam::{Mat4, Vec3};
use sashiko_asset::{
    Primitive,
    scene::{
        EnvironmentImage, FilmCamera, InfiniteSky, PerspectiveView, PhysicalScene, Placement,
        ScenePackage,
    },
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};
use surface::{Material, Materials};
use syntax::Command;

type Result<T> = std::result::Result<T, Error>;

// PBRT's +Z camera convention mirrors a Sashiko right-handed camera. Change
// world and prototype bases together so raster and light-space winding agree.
fn change_basis() -> Mat4 {
    Mat4::from_scale(Vec3::new(-1., 1., 1.))
}

#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Options {
    pub metres_per_unit: Option<f32>,
    pub origin: [f32; 3],
    pub region_xz_metres: Option<[f32; 4]>,
}

#[derive(Clone)]
struct State {
    matrix: Mat4,
    material: Arc<Material>,
    reverse: bool,
}

struct Shape {
    location: String,
    file: PathBuf,
    state: State,
    alpha: Option<PathBuf>,
}

#[derive(Default)]
struct Textures {
    scalar: HashMap<String, PathBuf>,
    spectrum: HashMap<String, PathBuf>,
}

struct Import {
    root: PathBuf,
    state: State,
    stack: Vec<(State, bool)>,
    active_files: HashSet<PathBuf>,
    definitions: HashMap<String, Vec<usize>>,
    definition: Option<String>,
    shapes: Vec<Shape>,
    direct: Vec<usize>,
    instances: Vec<(String, Mat4, String)>,
    textures: Textures,
    materials: HashMap<String, Arc<Material>>,
    notes: Vec<String>,
    camera: Option<(Mat4, f32)>,
    film: FilmCamera,
    sky: Option<(PathBuf, Mat4, f32)>,
    world: bool,
}

impl Command<'_> {
    fn arg(&self, i: usize) -> Result<&str> {
        self.args
            .get(i)
            .map(|s| s.as_ref())
            .ok_or_else(|| format!("missing argument {i}").into())
    }
    fn param(&self, key: &str) -> Result<Option<&str>> {
        match self.params.get(key).map(Vec::as_slice) {
            Some([v]) => Ok(Some(v)),
            None => Ok(None),
            _ => Err(format!("expected one value for {key}").into()),
        }
    }
    fn numbers<const N: usize>(&self) -> Result<[f32; N]> {
        let values = self
            .args
            .iter()
            .map(|v| v.parse::<f32>())
            .collect::<std::result::Result<Vec<_>, _>>()?;
        values
            .try_into()
            .map_err(|_| format!("expected {N} numbers").into())
    }
    fn supported(&self, keys: &[&str]) -> Result<()> {
        if let Some(key) = self.params.keys().find(|key| !keys.contains(&key.as_ref())) {
            return Err(format!("unsupported {} parameter {key}", self.name).into());
        }
        Ok(())
    }
}

impl Import {
    fn path(&self, name: &str) -> Result<PathBuf> {
        let path = self
            .root
            .join(name)
            .canonicalize()
            .map_err(|e| format!("{name}: {e}"))?;
        if !path.starts_with(&self.root) {
            return Err(format!("asset escapes source root: {name}").into());
        }
        Ok(path)
    }
    fn file(&mut self, file: &Path) -> Result<()> {
        if !self.active_files.insert(file.to_owned()) {
            return Err(format!("recursive Include: {}", file.display()).into());
        }
        let text = fs::read_to_string(file)?;
        syntax::visit(&text, file, |cmd| self.command(file, cmd))?;
        self.active_files.remove(file);
        Ok(())
    }
    fn command(&mut self, file: &Path, cmd: Command<'_>) -> Result<()> {
        let loc = format!("{}:{}", file.strip_prefix(&self.root)?.display(), cmd.line);
        match cmd.name {
            "Include" => self.file(&self.path(cmd.arg(0)?)?)?,
            "Identity" => self.state.matrix = Mat4::IDENTITY,
            "Transform" => self.state.matrix = Mat4::from_cols_array(&cmd.numbers()?),
            "ConcatTransform" => self.state.matrix *= Mat4::from_cols_array(&cmd.numbers()?),
            "Translate" => self.state.matrix *= Mat4::from_translation(cmd.numbers()?.into()),
            "Scale" => self.state.matrix *= Mat4::from_scale(cmd.numbers()?.into()),
            "Rotate" => {
                let [angle, x, y, z] = cmd.numbers()?;
                let axis = Vec3::new(x, y, z)
                    .try_normalize()
                    .ok_or("zero rotation axis")?;
                self.state.matrix *= Mat4::from_axis_angle(axis, angle.to_radians());
            }
            "LookAt" => {
                let [x, y, z, tx, ty, tz, ux, uy, uz] = cmd.numbers()?;
                self.state.matrix *= Mat4::look_at_lh(
                    Vec3::new(x, y, z),
                    Vec3::new(tx, ty, tz),
                    Vec3::new(ux, uy, uz),
                );
            }
            "Camera" => {
                if self.world || cmd.arg(0)? != "perspective" {
                    return Err("expected perspective camera before WorldBegin".into());
                }
                self.camera = Some((
                    self.state.matrix.inverse(),
                    cmd.param("float fov")?.unwrap_or("90").parse()?,
                ));
                cmd.supported(&["float fov", "float lensradius", "float focaldistance"])?;
                self.film.lens_radius_metres =
                    cmd.param("float lensradius")?.unwrap_or("0").parse()?;
                self.film.focus_metres = cmd
                    .param("float focaldistance")?
                    .unwrap_or("1000000")
                    .parse()?;
                if self.film.lens_radius_metres > 0. {
                    self.notes.push(format!(
                        "{loc}: interactive projection uses a pinhole in place of the finite lens"
                    ));
                }
            }
            "Film" => {
                if cmd.arg(0)? != "rgb" {
                    return Err("unsupported film type".into());
                }
                cmd.supported(&[
                    "string filename",
                    "integer xresolution",
                    "integer yresolution",
                    "float iso",
                    "float whitebalance",
                ])?;
                self.film.film_extent = [
                    cmd.param("integer xresolution")?
                        .unwrap_or("1280")
                        .parse()?,
                    cmd.param("integer yresolution")?.unwrap_or("720").parse()?,
                ];
                self.film.iso = cmd.param("float iso")?.unwrap_or("100").parse()?;
                self.film.white_balance_kelvin = cmd
                    .param("float whitebalance")?
                    .map(str::parse)
                    .transpose()?
                    .filter(|k| *k != 0.);
            }
            "Sampler" => {
                if !matches!(cmd.arg(0)?, "halton" | "sobol" | "independent") {
                    return Err("unsupported sampler type".into());
                }
                cmd.supported(&["integer pixelsamples"])?;
                self.film.samples_per_pixel =
                    cmd.param("integer pixelsamples")?.unwrap_or("16").parse()?;
                self.notes.push(format!(
                    "{loc}: source {} sampler, {} samples per pixel",
                    cmd.arg(0)?,
                    self.film.samples_per_pixel
                ));
            }
            "Integrator" | "PixelFilter" => {
                self.notes.push(format!(
                    "{loc} {} {:?} {:?} (source settings)",
                    cmd.name, cmd.args, cmd.params
                ));
            }
            "WorldBegin" => {
                if self.world {
                    return Err("repeated WorldBegin".into());
                }
                self.world = true;
                self.state.matrix = Mat4::IDENTITY;
            }
            "AttributeBegin" => self.stack.push((self.state.clone(), false)),
            "ObjectBegin" => {
                let name = cmd.arg(0)?.to_owned();
                if self.definition.is_some() || self.definitions.contains_key(&name) {
                    return Err("nested or duplicate object definition".into());
                }
                self.stack.push((self.state.clone(), true));
                self.definitions.insert(name.clone(), Vec::new());
                self.definition = Some(name);
            }
            "ObjectEnd" | "AttributeEnd" => {
                let (state, object) = self.stack.pop().ok_or("unbalanced graphics state")?;
                if object != (cmd.name == "ObjectEnd") {
                    return Err("mismatched graphics state end".into());
                }
                if object {
                    self.definition = None;
                }
                self.state = state;
            }
            "ReverseOrientation" => self.state.reverse = !self.state.reverse,
            "Texture" => {
                if cmd.arg(2)? != "imagemap" {
                    return Err("unsupported texture class".into());
                }
                let path = self.path(
                    cmd.param("string filename")?
                        .ok_or("missing imagemap filename")?,
                )?;
                let maps = match cmd.arg(1)? {
                    "float" => &mut self.textures.scalar,
                    "spectrum" => &mut self.textures.spectrum,
                    ty => return Err(format!("unsupported texture value type {ty}").into()),
                };
                maps.insert(cmd.arg(0)?.to_owned(), path);
                cmd.supported(&["string filename"])?;
            }
            "Material" | "MakeNamedMaterial" => {
                let named = cmd.name == "MakeNamedMaterial";
                let kind = if named {
                    cmd.param("string type")?
                        .ok_or("missing named material type")?
                } else {
                    cmd.arg(0)?
                };
                let material = Arc::new(Material::parse(&cmd, kind, &self.textures)?);
                if named {
                    self.materials.insert(cmd.arg(0)?.into(), material);
                } else {
                    self.state.material = material;
                }
            }
            "NamedMaterial" => {
                self.state.material = self
                    .materials
                    .get(cmd.arg(0)?)
                    .ok_or("undefined named material")?
                    .clone()
            }
            "Shape" => {
                if cmd.arg(0)? != "plymesh" {
                    return Err("unsupported shape (expected plymesh)".into());
                }
                let index = self.shapes.len();
                let file = self.path(
                    cmd.param("string filename")?
                        .ok_or("missing PLY filename")?,
                )?;
                let alpha = cmd
                    .param("texture alpha")?
                    .map(|name| {
                        self.textures
                            .scalar
                            .get(name)
                            .cloned()
                            .ok_or("undefined alpha texture")
                    })
                    .transpose()?;
                for key in cmd.params.keys() {
                    if !matches!(key.as_ref(), "string filename" | "texture alpha") {
                        return Err(format!("unsupported shape parameter {key}").into());
                    }
                }
                self.shapes.push(Shape {
                    location: loc,
                    file,
                    state: self.state.clone(),
                    alpha,
                });
                if let Some(name) = &self.definition {
                    self.definitions.get_mut(name).unwrap().push(index);
                } else {
                    self.direct.push(index);
                }
            }
            "ObjectInstance" => {
                if self.definition.is_some() {
                    return Err("nested object instances not supported by PBRT".into());
                }
                self.instances
                    .push((cmd.arg(0)?.into(), self.state.matrix, loc));
            }
            "LightSource" => {
                if cmd.arg(0)? != "infinite" {
                    return Err("unsupported light source".into());
                }
                let path = self.path(
                    cmd.param("string filename")?
                        .ok_or("expected infinite image light")?,
                )?;
                cmd.supported(&["string filename", "float scale"])?;
                if self.sky.is_some() {
                    return Err("multiple infinite lights are unsupported".into());
                }
                self.sky = Some((
                    path.clone(),
                    self.state.matrix,
                    cmd.param("float scale")?.unwrap_or("1").parse()?,
                ));
                self.notes.push(format!("{loc} infinite light {} {:?} matrix {:?}: interactive preview uses recipe environment",
                    path.strip_prefix(&self.root)?.display(),cmd.params,self.state.matrix.to_cols_array()));
            }
            _ => return Err(format!("unsupported PBRT directive {}", cmd.name).into()),
        }
        if !self.state.matrix.is_finite() {
            return Err("nonfinite transformation".into());
        }
        Ok(())
    }
}

fn infinite_sky(path: &Path, transform: Mat4, scale: f32, directory: &Path) -> Result<InfiniteSky> {
    use exr::prelude::{f16, read_first_rgba_layer_from_file};
    let layer = read_first_rgba_layer_from_file(
        path,
        |size, _| vec![vec![[f16::ZERO; 4]; size.width()]; size.height()],
        |rows, pos, (r, g, b, a): (f16, f16, f16, f16)| rows[pos.y()][pos.x()] = [r, g, b, a],
    )?
    .layer_data;
    if layer.size.width() != layer.size.height() {
        return Err("infinite equal-area light must be square".into());
    }
    let mut data = Vec::with_capacity(layer.size.area() * 8);
    for channel in layer.channel_data.pixels.into_iter().flatten().flatten() {
        if !channel.is_finite() {
            return Err("EXR radiance exceeds finite RGBA16F range".into());
        }
        data.extend(channel.to_bits().to_le_bytes());
    }
    Ok(InfiniteSky {
        equal_area_radiance: EnvironmentImage {
            width: layer.size.width().try_into()?,
            height: layer.size.height().try_into()?,
            mip_levels: 1,
            data: object(directory, &data)?,
        },
        world_from_light: transform.to_cols_array_2d(),
        scale,
    })
}

pub(super) fn cook(
    file: &Path,
    directory: &Path,
    options: Options,
    scene: &mut ScenePackage,
) -> Result<()> {
    let scale = options.metres_per_unit.unwrap_or(1.);
    if !scale.is_finite() || scale <= 0. {
        return Err("invalid PBRT scene scale".into());
    }
    let origin = Vec3::from(options.origin);
    if !origin.is_finite()
        || options
            .region_xz_metres
            .is_some_and(|r| r.iter().any(|v| !v.is_finite()) || r[0] >= r[2] || r[1] >= r[3])
    {
        return Err("invalid PBRT origin or region bounds".into());
    }
    let mut source = Import {
        root: file.parent().ok_or("missing scene directory")?.to_owned(),
        state: State {
            matrix: Mat4::IDENTITY,
            material: Arc::default(),
            reverse: false,
        },
        world: false,
        stack: Vec::new(),
        active_files: HashSet::new(),
        definitions: HashMap::new(),
        definition: None,
        shapes: Vec::new(),
        direct: Vec::new(),
        instances: Vec::new(),
        textures: Textures::default(),
        materials: HashMap::new(),
        notes: Vec::new(),
        camera: None,
        film: FilmCamera {
            lens_radius_metres: 0.,
            focus_metres: 1e6,
            iso: 100.,
            white_balance_kelvin: None,
            film_extent: [1280, 720],
            samples_per_pixel: 16,
        },
        sky: None,
    };
    source.file(file)?;
    if !source.world || !source.stack.is_empty() {
        return Err("incomplete PBRT world or graphics state".into());
    }
    let (pose, fov) = source.camera.ok_or("missing PBRT camera")?;
    let name = file
        .file_stem()
        .and_then(|v| v.to_str())
        .ok_or("invalid camera filename")?
        .to_owned();
    let [width, height] = source.film.film_extent;
    let vfov = if height > width {
        (2. * ((fov.to_radians() / 2.).tan() * height as f32 / width as f32).atan()).to_degrees()
    } else {
        fov
    };
    scene.settings.views.insert(
        name.clone(),
        PerspectiveView {
            position: change_basis()
                .transform_vector3((pose.w_axis.truncate() - origin) * scale)
                .into(),
            forward: change_basis()
                .transform_vector3(pose.z_axis.truncate())
                .normalize()
                .into(),
            up: change_basis()
                .transform_vector3(pose.y_axis.truncate())
                .normalize()
                .into(),
            vertical_fov_degrees: vfov,
            mirror_x: false,
        },
    );
    source.film.lens_radius_metres *= scale;
    source.film.focus_metres *= scale;
    let mut physical = PhysicalScene::default();
    physical.cameras.insert(name, source.film);
    physical.sky = source
        .sky
        .map(|(path, transform, s)| {
            infinite_sky(&path, change_basis() * transform, s, directory)
                .map_err(|e| format!("{}: {e}", path.display()))
        })
        .transpose()?;
    let mut selected = Vec::new();
    for &id in &source.direct {
        let shape = &source.shapes[id];
        if let Some(region) = options.region_xz_metres
            && !ply::overlaps(&shape.file, shape.state.matrix, origin, scale, region)
                .map_err(|e| format!("{} {}: {e}", shape.location, shape.file.display()))?
        {
            continue;
        }
        selected.push((id, Mat4::IDENTITY));
    }
    for (name, placement, loc) in &source.instances {
        let shapes = source
            .definitions
            .get(name)
            .ok_or_else(|| format!("{loc}: undefined instance {name}"))?;
        if let Some([x0, z0, x1, z1]) = options.region_xz_metres {
            let pos = (placement.w_axis.truncate() - origin) * scale;
            if pos.x < x0 || pos.x >= x1 || pos.z < z0 || pos.z >= z1 {
                continue;
            }
        }
        selected.extend(shapes.iter().map(|&id| (id, *placement)));
    }
    let mut geometries = HashMap::new();
    let mut indices = HashMap::new();
    let mut surfaces = Materials::default();
    let cache_directory = directory
        .parent()
        .ok_or("missing output parent")?
        .join(".sashiko-pbrt-cache");
    let mut cache = crate::cache::Cache::new(&cache_directory, directory)?;
    let mut placed_triangles = 0u64;
    for (id, placement) in selected {
        let shape = &source.shapes[id];
        let (primitive, triangles) = match indices.entry(id) {
            std::collections::hash_map::Entry::Occupied(e) => *e.get(),
            std::collections::hash_map::Entry::Vacant(e) => {
                let (mesh, n) = match geometries.entry((&shape.file, shape.state.reverse)) {
                    std::collections::hash_map::Entry::Occupied(e) => *e.get(),
                    std::collections::hash_map::Entry::Vacant(e) => {
                        let (mesh, n, zero_normals) = ply::cook(
                            &mut cache,
                            &shape.file,
                            shape.file.strip_prefix(&source.root)?,
                            shape.state.reverse,
                            scale,
                        )
                        .map_err(|err| {
                            format!("{} {}: {err}", shape.location, shape.file.display())
                        })?;
                        if zero_normals > 0 {
                            source.notes.push(format!("{} {}: replaced {zero_normals} zero normals with +Y after checking that every incident triangle has zero area",
                                shape.location, shape.file.strip_prefix(&source.root)?.display()));
                        }
                        *e.insert((mesh, n))
                    }
                };
                let (material, image, surface) = surfaces.cook(
                    directory,
                    &shape.state.material,
                    shape.alpha.as_deref(),
                    &shape.location,
                    &mut source.notes,
                )?;
                physical.surfaces.push(surface);
                let index = scene.primitives.len();
                scene.primitives.push(Primitive {
                    mesh,
                    material,
                    images: [image, None, None, None],
                });
                *e.insert((index, n))
            }
        };
        let mut transform = placement * shape.state.matrix;
        transform.w_axis = ((transform.w_axis.truncate() - origin) * scale).extend(1.);
        scene.placements.push(Placement {
            primitive,
            transform: (change_basis() * transform * change_basis()).to_cols_array_2d(),
        });
        placed_triangles += triangles as u64;
    }
    let unique_triangles: usize = geometries.values().map(|(_, n)| n).sum();
    let mut empty: Vec<_> = source
        .definitions
        .iter()
        .filter(|(_, v)| v.is_empty())
        .map(|(name, _)| name)
        .collect();
    empty.sort();
    scene.physical = Some(physical);
    println!(
        "{} source instances, {} placed primitives, {unique_triangles} unique / {placed_triangles} placed triangles; empty definitions: {empty:?}",
        source.instances.len(),
        scene.placements.len()
    );
    for note in source.notes {
        eprintln!("{note}");
    }
    Ok(())
}

//! Executable spelling/ownership comparison, deliberately without native objects.
//!
//! It checks local input/envelope contracts and compares normalized CPU intents.
//! No scheduling, GPU synchronization, contents validation, shader compilation,
//! physical allocation, completion, or performance model is implemented here.

use std::{
    collections::BTreeSet,
    error::Error,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_OWNER: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Hdr,
    Depth,
    Normal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Image {
    owner: u64,
    index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct View {
    image: Image,
    first_mip: u32,
    mip_count: u32,
}

impl Image {
    fn mip(self, mip: u32) -> View {
        View {
            image: self,
            first_mip: mip,
            mip_count: 1,
        }
    }
}

#[derive(Debug)]
struct ImageDesc {
    name: &'static str,
    format: Format,
    extent: [u32; 2],
    mips: u32,
    imported: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Usage {
    Sampled,
    StorageRead,
    StorageWrite,
    ColorWrite,
    ColorReadWrite,
    DepthWrite,
    DepthReadWrite,
    CopyRead,
    CopyWrite,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Access {
    view: View,
    usage: Usage,
}

impl Access {
    fn writes(self) -> bool {
        matches!(
            self.usage,
            Usage::StorageWrite
                | Usage::ColorWrite
                | Usage::ColorReadWrite
                | Usage::DepthWrite
                | Usage::DepthReadWrite
                | Usage::CopyWrite
        )
    }

    fn covers(self, other: Self) -> bool {
        self.usage == other.usage
            && self.view.image == other.view.image
            && self.view.first_mip <= other.view.first_mip
            && self.view.first_mip + self.view.mip_count
                >= other.view.first_mip + other.view.mip_count
    }
}

#[derive(Debug)]
pub(super) struct Failure {
    pass: &'static str,
    kind: &'static str,
    resource: Option<&'static str>,
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {} ({:?})", self.pass, self.kind, self.resource)
    }
}

impl Error for Failure {}
type Result<T> = std::result::Result<T, Failure>;

fn failure(pass: &'static str, kind: &'static str) -> Failure {
    Failure {
        pass,
        kind,
        resource: None,
    }
}

#[derive(Debug)]
struct Catalog {
    owner: u64,
    images: Vec<ImageDesc>,
}

impl Catalog {
    fn new() -> Self {
        Self {
            owner: NEXT_OWNER.fetch_add(1, Ordering::Relaxed),
            images: Vec::new(),
        }
    }

    fn image(&mut self, name: &'static str, format: Format, extent: [u32; 2], mips: u32) -> Image {
        assert!(extent.into_iter().all(|n| n != 0));
        assert!(mips > 0 && mips <= 32 - extent[0].max(extent[1]).leading_zeros());
        let image = Image {
            owner: self.owner,
            index: self.images.len(),
        };
        self.images.push(ImageDesc {
            name,
            format,
            extent,
            mips,
            imported: false,
        });
        image
    }

    fn import_history(&mut self, extent: [u32; 2]) -> Image {
        let image = self.image("previous-history", Format::Hdr, extent, 1);
        self.images[image.index].imported = true;
        image
    }

    fn view(&self, pass: &'static str, view: View) -> Result<&ImageDesc> {
        if view.image.owner != self.owner {
            return Err(failure(pass, "foreign graph resource"));
        }
        let desc = self
            .images
            .get(view.image.index)
            .ok_or_else(|| failure(pass, "unknown image"))?;
        if view.mip_count == 0
            || view
                .first_mip
                .checked_add(view.mip_count)
                .is_none_or(|end| end > desc.mips)
        {
            return Err(Failure {
                pass,
                kind: "mip range outside image",
                resource: Some(desc.name),
            });
        }
        Ok(desc)
    }

    fn extent(&self, pass: &'static str, view: View) -> Result<[u32; 2]> {
        let desc = self.view(pass, view)?;
        Ok(desc.extent.map(|n| (n >> view.first_mip).max(1)))
    }
}

// Pipeline values stand for immutable, creation-validated programs. A real API
// must own native programs, validate shader ABI/capabilities, and retain resources.
#[derive(Debug)]
struct RasterPipeline {
    name: &'static str,
    colors: &'static [Format],
    depth: bool,
}

#[derive(Debug)]
struct ComputePipeline {
    name: &'static str,
    slots: &'static [Usage],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Load {
    Clear,
    Load,
}

#[derive(Clone, Copy, Debug)]
struct Attachment {
    view: View,
    load: Load,
}

#[derive(Clone, Copy, Debug)]
struct Draw {
    // Symbolic managed mesh identity. Vertex/index ownership and provenance are
    // intentionally delegated to the future creation/import contract, not faked.
    mesh: u32,
    indices: u32,
    transform: [f32; 16],
}

struct RasterData<'frame> {
    pipeline: &'frame RasterPipeline,
    colors: Vec<Attachment>,
    depth: Option<Attachment>,
    draws: &'frame [Draw],
}

struct ComputeData<'frame> {
    pipeline: &'frame ComputePipeline,
    bindings: Vec<View>,
    groups: [u32; 3],
}

enum PassData<'frame> {
    Raster(RasterData<'frame>),
    Compute(ComputeData<'frame>),
    Copy { source: View, destination: View },
}

impl PassData<'_> {
    fn accesses(&self, pass: &'static str, catalog: &Catalog) -> Result<Vec<Access>> {
        let mut accesses = Vec::new();
        match self {
            Self::Raster(data) => {
                if data.pipeline.colors.len() != data.colors.len()
                    || data.pipeline.depth != data.depth.is_some()
                {
                    return Err(failure(pass, "pipeline attachment signature mismatch"));
                }
                let mut extent = None;
                for (attachment, expected) in data
                    .colors
                    .iter()
                    .zip(data.pipeline.colors.iter().copied())
                    .chain(data.depth.iter().map(|d| (d, Format::Depth)))
                {
                    if attachment.view.mip_count != 1
                        || catalog.view(pass, attachment.view)?.format != expected
                    {
                        return Err(failure(pass, "attachment format or view mismatch"));
                    }
                    let current = catalog.extent(pass, attachment.view)?;
                    if extent.replace(current).is_some_and(|old| old != current) {
                        return Err(failure(pass, "attachment extent mismatch"));
                    }
                    accesses.push(Access {
                        view: attachment.view,
                        usage: match (expected == Format::Depth, attachment.load) {
                            (true, Load::Clear) => Usage::DepthWrite,
                            (true, Load::Load) => Usage::DepthReadWrite,
                            (false, Load::Clear) => Usage::ColorWrite,
                            (false, Load::Load) => Usage::ColorReadWrite,
                        },
                    });
                }
            }
            Self::Compute(data) => {
                if data.pipeline.slots.len() != data.bindings.len() {
                    return Err(failure(pass, "pipeline binding count mismatch"));
                }
                if data.groups.contains(&0) {
                    return Err(failure(pass, "zero workgroup count"));
                }
                for (&view, &usage) in data.bindings.iter().zip(data.pipeline.slots) {
                    if !matches!(
                        usage,
                        Usage::Sampled | Usage::StorageRead | Usage::StorageWrite
                    ) || catalog.view(pass, view)?.format == Format::Depth
                    {
                        return Err(failure(pass, "binding usage or format mismatch"));
                    }
                    accesses.push(Access { view, usage });
                }
            }
            Self::Copy {
                source,
                destination,
            } => {
                if catalog.view(pass, *source)?.format != catalog.view(pass, *destination)?.format
                    || catalog.extent(pass, *source)? != catalog.extent(pass, *destination)?
                    || source.mip_count != 1
                    || destination.mip_count != 1
                {
                    return Err(failure(pass, "copy shape mismatch"));
                }
                accesses.extend([
                    Access {
                        view: *source,
                        usage: Usage::CopyRead,
                    },
                    Access {
                        view: *destination,
                        usage: Usage::CopyWrite,
                    },
                ]);
            }
        }
        accesses.sort_unstable();
        if accesses.iter().enumerate().any(|(i, a)| {
            accesses[i + 1..].iter().any(|b| {
                a.view.image == b.view.image
                    && a.view.first_mip + a.view.mip_count > b.view.first_mip
                    && b.view.first_mip + b.view.mip_count > a.view.first_mip
                    && (a.writes() || b.writes())
            })
        }) {
            return Err(failure(
                pass,
                "overlapping bindings require separate passes",
            ));
        }
        accesses.dedup();
        Ok(accesses)
    }

    fn commands(&self) -> Vec<Command> {
        match self {
            Self::Raster(data) => {
                let mut commands = vec![Command::Raster {
                    pipeline: data.pipeline.name,
                    colors: data.colors.iter().map(|a| (a.view, a.load)).collect(),
                    depth: data.depth.map(|a| (a.view, a.load)),
                }];
                commands.extend(data.draws.iter().map(|draw| Command::Draw {
                    mesh: draw.mesh,
                    indices: draw.indices,
                    transform: draw.transform.map(f32::to_bits),
                }));
                commands
            }
            Self::Compute(data) => vec![Command::Dispatch {
                pipeline: data.pipeline.name,
                bindings: data.bindings.clone(),
                groups: data.groups,
            }],
            Self::Copy {
                source,
                destination,
            } => vec![Command::Copy(*source, *destination)],
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Raster {
        pipeline: &'static str,
        colors: Vec<(View, Load)>,
        depth: Option<(View, Load)>,
    },
    Draw {
        mesh: u32,
        indices: u32,
        transform: [u32; 16],
    },
    Dispatch {
        pipeline: &'static str,
        bindings: Vec<View>,
        groups: [u32; 3],
    },
    Copy(View, View),
}

#[derive(Debug, PartialEq, Eq)]
struct PassIntent {
    name: &'static str,
    accesses: Vec<Access>,
    commands: Vec<Command>,
}

struct Recorder<'a> {
    catalog: &'a Catalog,
    name: &'static str,
    envelope: Option<&'a [Access]>,
    intent: PassIntent,
}

impl<'a> Recorder<'a> {
    fn new(catalog: &'a Catalog, name: &'static str, envelope: Option<&'a [Access]>) -> Self {
        Self {
            catalog,
            name,
            envelope,
            intent: PassIntent {
                name,
                accesses: Vec::new(),
                commands: Vec::new(),
            },
        }
    }

    fn emit(&mut self, data: &PassData<'_>) -> Result<()> {
        let accesses = data.accesses(self.name, self.catalog)?;
        if self.envelope.is_some_and(|allowed| {
            accesses
                .iter()
                .any(|access| !allowed.iter().any(|a| a.covers(*access)))
        }) {
            return Err(failure(self.name, "command outside declared envelope"));
        }
        if !self.intent.commands.is_empty() {
            return Err(failure(self.name, "dependent work needs a separate pass"));
        }
        self.intent.accesses = accesses;
        self.intent.commands = data.commands();
        Ok(())
    }
}

type Callback<'frame> = Box<dyn FnOnce(&mut Recorder<'_>) -> Result<()> + 'frame>;

enum Body<'frame> {
    Owned(PassIntent),
    Deferred(Callback<'frame>),
}

struct Pass<'frame> {
    name: &'static str,
    declared: Vec<Access>,
    body: Body<'frame>,
}

#[derive(Clone, Copy, Debug)]
enum Timing {
    Owned,
    Deferred,
}

#[derive(Default, Debug)]
struct Counts {
    build_callbacks: usize,
    record_callbacks: usize,
    declaration_accesses: usize,
    built_command_bytes: usize,
}

struct Study<'frame> {
    catalog: Catalog,
    passes: Vec<Pass<'frame>>,
    exports: Vec<Image>,
    counts: Counts,
}

impl<'frame> Study<'frame> {
    fn new(catalog: Catalog) -> Self {
        Self {
            catalog,
            passes: Vec::new(),
            exports: Vec::new(),
            counts: Counts::default(),
        }
    }

    // A: the closure may borrow transient CPU inputs, because it runs now.
    fn owned(
        &mut self,
        name: &'static str,
        record: impl FnOnce(&mut Recorder<'_>) -> Result<()>,
    ) -> Result<()> {
        let mut recorder = Recorder::new(&self.catalog, name, None);
        record(&mut recorder)?;
        self.counts.build_callbacks += 1;
        self.counts.declaration_accesses += recorder.intent.accesses.len();
        self.counts.built_command_bytes +=
            recorder.intent.commands.len() * std::mem::size_of::<Command>();
        self.passes.push(Pass {
            name,
            declared: recorder.intent.accesses.clone(),
            body: Body::Owned(recorder.intent),
        });
        Ok(())
    }

    // B: setup owns the bundle once; both declaration and callback use it.
    // There is no 'static, Send, Arc, or draw-list clone requirement here.
    fn deferred(
        &mut self,
        name: &'static str,
        data: PassData<'frame>,
        record: impl FnOnce(&mut Recorder<'_>, &PassData<'frame>) -> Result<()> + 'frame,
    ) -> Result<()> {
        let declared = data.accesses(name, &self.catalog)?;
        self.counts.declaration_accesses += declared.len();
        self.passes.push(Pass {
            name,
            declared,
            body: Body::Deferred(Box::new(move |recorder| record(recorder, &data))),
        });
        Ok(())
    }

    fn add(&mut self, timing: Timing, name: &'static str, data: PassData<'frame>) -> Result<()> {
        match timing {
            Timing::Owned => self.owned(name, |encoder| encoder.emit(&data)),
            Timing::Deferred => self.deferred(name, data, |encoder, data| encoder.emit(data)),
        }
    }

    fn export(&mut self, image: Image) -> Result<()> {
        self.catalog.view("export", image.mip(0))?;
        self.exports.push(image);
        Ok(())
    }

    // Only a small backward reachability probe, not Renderer's compiler. All
    // read/write ranges and native producer/hazard/initialization rules remain
    // work for B1; scenarios use one writer per image/mip except attachment Load.
    fn record(self) -> Result<(Vec<PassIntent>, Counts)> {
        let mut needed: BTreeSet<Image> = self.exports.into_iter().collect();
        let mut live = vec![false; self.passes.len()];
        for (index, pass) in self.passes.iter().enumerate().rev() {
            if pass
                .declared
                .iter()
                .any(|a| a.writes() && needed.contains(&a.view.image))
            {
                live[index] = true;
                needed.extend(pass.declared.iter().map(|a| a.view.image));
            }
        }
        let mut result = Vec::new();
        let mut counts = self.counts;
        for (pass, live) in self.passes.into_iter().zip(live) {
            if !live {
                continue;
            }
            match pass.body {
                Body::Owned(intent) => result.push(intent),
                Body::Deferred(callback) => {
                    let mut recorder =
                        Recorder::new(&self.catalog, pass.name, Some(&pass.declared));
                    callback(&mut recorder)?;
                    if recorder.intent.commands.is_empty() {
                        return Err(failure(pass.name, "callback emitted no commands"));
                    }
                    if pass.declared.iter().filter(|a| a.writes()).any(|required| {
                        !recorder
                            .intent
                            .accesses
                            .iter()
                            .any(|actual| actual.covers(*required))
                    }) {
                        return Err(failure(pass.name, "callback omitted declared output"));
                    }
                    counts.record_callbacks += 1;
                    result.push(recorder.intent);
                }
            }
        }
        Ok((result, counts))
    }
}

struct Programs {
    opaque: RasterPipeline,
    shadow: RasterPipeline,
    color: RasterPipeline,
    mrt: RasterPipeline,
    custom: ComputePipeline,
    extract: ComputePipeline,
    down: ComputePipeline,
    blur: ComputePipeline,
    combine: ComputePipeline,
    temporal: ComputePipeline,
}

impl Programs {
    fn new() -> Self {
        Self {
            opaque: RasterPipeline {
                name: "user/pbr",
                colors: &[Format::Hdr],
                depth: true,
            },
            shadow: RasterPipeline {
                name: "user/shadow",
                colors: &[],
                depth: true,
            },
            color: RasterPipeline {
                name: "user/color-only",
                colors: &[Format::Hdr],
                depth: false,
            },
            mrt: RasterPipeline {
                name: "user/gbuffer",
                colors: &[Format::Hdr, Format::Normal],
                depth: true,
            },
            custom: ComputePipeline {
                name: "user/custom-compute",
                slots: &[Usage::StorageRead, Usage::StorageWrite],
            },
            down: ComputePipeline {
                name: "external/bloom-downsample",
                slots: &[Usage::Sampled, Usage::StorageWrite],
            },
            extract: ComputePipeline {
                name: "external/bloom-extract",
                slots: &[Usage::Sampled, Usage::StorageWrite],
            },
            blur: ComputePipeline {
                name: "external/bloom-blur",
                slots: &[Usage::Sampled, Usage::StorageWrite],
            },
            combine: ComputePipeline {
                name: "external/bloom-combine",
                slots: &[Usage::Sampled, Usage::Sampled, Usage::StorageWrite],
            },
            temporal: ComputePipeline {
                name: "user/temporal",
                slots: &[Usage::Sampled, Usage::Sampled, Usage::StorageWrite],
            },
        }
    }
}

fn compute<'a>(
    pipeline: &'a ComputePipeline,
    bindings: Vec<View>,
    extent: [u32; 2],
) -> PassData<'a> {
    PassData::Compute(ComputeData {
        pipeline,
        bindings,
        groups: [extent[0].div_ceil(8), extent[1].div_ceil(8), 1],
    })
}

// An external effect is ordinary composition: adding a different shader/effect
// requires no new PassData, Command, Graph operation, or backend effect enum.
fn bloom<'a>(
    study: &mut Study<'a>,
    timing: Timing,
    programs: &'a Programs,
    input: Image,
    enabled: bool,
) -> Result<Image> {
    if !enabled {
        return Ok(input);
    }
    let extent = study.catalog.extent("bloom", input.mip(0))?;
    let extracted = study
        .catalog
        .image("bloom-extracted", Format::Hdr, extent, 1);
    study.add(
        timing,
        "bloom/extract",
        compute(
            &programs.extract,
            vec![input.mip(0), extracted.mip(0)],
            extent,
        ),
    )?;
    let pyramid = study.catalog.image("bloom-pyramid", Format::Hdr, extent, 3);
    for mip in 0..3 {
        let source = if mip == 0 {
            extracted.mip(0)
        } else {
            pyramid.mip(mip - 1)
        };
        let target = pyramid.mip(mip);
        study.add(
            timing,
            "bloom/downsample",
            compute(
                &programs.down,
                vec![source, target],
                study.catalog.extent("bloom", target)?,
            ),
        )?;
    }
    let small = study.catalog.extent("bloom", pyramid.mip(2))?;
    let blurred = study.catalog.image("bloom-blurred", Format::Hdr, small, 1);
    study.add(
        timing,
        "bloom/blur",
        compute(&programs.blur, vec![pyramid.mip(2), blurred.mip(0)], small),
    )?;
    let output = study.catalog.image("bloom-hdr", Format::Hdr, extent, 1);
    study.add(
        timing,
        "bloom/combine",
        compute(
            &programs.combine,
            vec![input.mip(0), blurred.mip(0), output.mip(0)],
            extent,
        ),
    )?;
    Ok(output)
}

fn scenario<'a>(
    timing: Timing,
    programs: &'a Programs,
    draws: &'a [Draw],
    previous_extent: Option<[u32; 2]>,
) -> Result<(Vec<PassIntent>, Counts)> {
    let extent = [1920, 1080];
    let mut catalog = Catalog::new();
    let hdr = catalog.image("hdr", Format::Hdr, extent, 1);
    let depth = catalog.image("depth", Format::Depth, extent, 1);
    let shadow = catalog.image("shadow", Format::Depth, [2048, 2048], 1);
    let normal = catalog.image("normal", Format::Normal, extent, 1);
    let mut study = Study::new(catalog);
    let depth_attachment = |image: Image| Attachment {
        view: image.mip(0),
        load: Load::Clear,
    };
    study.add(
        timing,
        "depth-only",
        PassData::Raster(RasterData {
            pipeline: &programs.shadow,
            colors: vec![],
            depth: Some(depth_attachment(shadow)),
            draws,
        }),
    )?;
    study.export(shadow)?;
    study.add(
        timing,
        "multiple-draws",
        PassData::Raster(RasterData {
            pipeline: &programs.opaque,
            colors: vec![Attachment {
                view: hdr.mip(0),
                load: Load::Clear,
            }],
            depth: Some(depth_attachment(depth)),
            draws,
        }),
    )?;
    study.add(
        timing,
        "color-only",
        PassData::Raster(RasterData {
            pipeline: &programs.color,
            colors: vec![Attachment {
                view: hdr.mip(0),
                load: Load::Load,
            }],
            depth: None,
            draws: &draws[..1],
        }),
    )?;
    let custom = study.catalog.image("custom-hdr", Format::Hdr, extent, 1);
    study.add(
        timing,
        "custom-compute",
        compute(&programs.custom, vec![hdr.mip(0), custom.mip(0)], extent),
    )?;
    let filtered = bloom(&mut study, timing, programs, custom, true)?;
    let history = study.catalog.image("new-history", Format::Hdr, extent, 1);
    if previous_extent == Some(extent) {
        let previous = study.catalog.import_history(extent);
        study.add(
            timing,
            "history/import-and-resolve",
            compute(
                &programs.temporal,
                vec![filtered.mip(0), previous.mip(0), history.mip(0)],
                extent,
            ),
        )?;
    } else {
        // First frame/reset/resize: initialize from this frame, never sample
        // uninitialized old history. Actual state/revision/submit is not modeled.
        study.add(
            timing,
            "history/first-or-reset",
            PassData::Copy {
                source: filtered.mip(0),
                destination: history.mip(0),
            },
        )?;
    }
    study.export(history)?;
    // Future MRT spelling/range probe, not a production feature assertion.
    let mrt = study.catalog.image("mrt-hdr", Format::Hdr, extent, 1);
    study.add(
        timing,
        "future-mrt",
        PassData::Raster(RasterData {
            pipeline: &programs.mrt,
            colors: vec![
                Attachment {
                    view: mrt.mip(0),
                    load: Load::Clear,
                },
                Attachment {
                    view: normal.mip(0),
                    load: Load::Clear,
                },
            ],
            depth: Some(depth_attachment(depth)),
            draws,
        }),
    )?;
    study.export(normal)?;
    let dead = study.catalog.image("unused", Format::Hdr, extent, 1);
    study.add(
        timing,
        "unused-effect",
        compute(&programs.down, vec![filtered.mip(0), dead.mip(0)], extent),
    )?;
    study.record()
}

fn draws() -> Vec<Draw> {
    (0..3)
        .map(|mesh| {
            let mut transform = [0.0; 16];
            for i in [0, 5, 10, 15] {
                transform[i] = 1.0;
            }
            transform[12] = mesh as f32;
            Draw {
                mesh,
                indices: 36,
                transform,
            }
        })
        .collect()
}

// Owner IDs differ per graph and are intentionally omitted only for normalized
// comparison. Invalid cross-graph handles are rejected before this conversion.
fn normalize(intents: &mut [PassIntent]) {
    fn view(v: &mut View) {
        v.image.owner = 0;
    }
    for intent in intents {
        for access in &mut intent.accesses {
            view(&mut access.view);
        }
        for command in &mut intent.commands {
            match command {
                Command::Raster { colors, depth, .. } => {
                    for (v, _) in colors {
                        view(v);
                    }
                    if let Some((v, _)) = depth {
                        view(v);
                    }
                }
                Command::Dispatch { bindings, .. } => {
                    for v in bindings {
                        view(v);
                    }
                }
                Command::Copy(a, b) => {
                    view(a);
                    view(b);
                }
                Command::Draw { .. } => {}
            }
        }
    }
}

pub(super) fn compare() -> Result<()> {
    let programs = Programs::new();
    let draws = draws();
    println!("CPU API study only; no GPU execution or performance claim");
    for previous in [None, Some([1920, 1080]), Some([1280, 720])] {
        let (mut owned, a) = scenario(Timing::Owned, &programs, &draws, previous)?;
        let (mut deferred, b) = scenario(Timing::Deferred, &programs, &draws, previous)?;
        normalize(&mut owned);
        normalize(&mut deferred);
        assert_eq!(owned, deferred);
        let commands: usize = owned.iter().map(|p| p.commands.len()).sum();
        let accesses: usize = owned.iter().map(|p| p.accesses.len()).sum();
        println!(
            "history={previous:?}: equal intents; probe_retained_passes={} commands={commands} accesses={accesses}; owned={a:?}; deferred={b:?}",
            owned.len()
        );
    }
    println!(
        "command byte count is enum storage only, excluding nested Vec capacity, catalog, callbacks and allocator overhead"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn representative_usage_has_equal_intents_for_both_timings() {
        let programs = Programs::new();
        let draws = draws();
        for previous in [None, Some([1920, 1080]), Some([1280, 720])] {
            let (mut a, ac) = scenario(Timing::Owned, &programs, &draws, previous).unwrap();
            let (mut b, bc) = scenario(Timing::Deferred, &programs, &draws, previous).unwrap();
            normalize(&mut a);
            normalize(&mut b);
            assert_eq!(a, b);
            assert_eq!(a.len(), 12);
            assert_eq!(ac.build_callbacks, 13);
            assert_eq!(bc.build_callbacks, 0);
            assert_eq!(bc.record_callbacks, 12);
            assert!(a.iter().any(|p| p.name == "future-mrt"));
            assert!(!a.iter().any(|p| p.name == "unused-effect"));
            assert!(a.iter().any(|p| p.name == "bloom/extract"));
            assert!(a.iter().any(|p| p.name == "bloom/blur"));
            let history = if previous == Some([1920, 1080]) {
                "history/import-and-resolve"
            } else {
                "history/first-or-reset"
            };
            assert!(a.iter().any(|p| p.name == history));
            assert!(
                a.iter()
                    .find(|p| p.name == "color-only")
                    .unwrap()
                    .accesses
                    .iter()
                    .any(|a| a.usage == Usage::ColorReadWrite)
            );
        }
    }

    #[test]
    fn graph_owner_and_mip_range_fail_with_context() {
        let mut a = Catalog::new();
        let image = a.image("image", Format::Hdr, [16, 16], 2);
        let b = Catalog::new();
        assert_eq!(
            b.view("foreign", image.mip(0)).unwrap_err().kind,
            "foreign graph resource"
        );
        let error = a.view("mips", image.mip(2)).unwrap_err();
        assert_eq!((error.pass, error.resource), ("mips", Some("image")));
    }

    #[test]
    fn mismatched_pipeline_or_overlapping_bindings_are_rejected() {
        let programs = Programs::new();
        let mut catalog = Catalog::new();
        let a = catalog.image("a", Format::Hdr, [16, 16], 1);
        let wrong = PassData::Raster(RasterData {
            pipeline: &programs.shadow,
            colors: vec![Attachment {
                view: a.mip(0),
                load: Load::Clear,
            }],
            depth: None,
            draws: &[],
        });
        assert_eq!(
            wrong.accesses("wrong-pipeline", &catalog).unwrap_err().kind,
            "pipeline attachment signature mismatch"
        );
        let overlap = compute(&programs.down, vec![a.mip(0), a.mip(0)], [16, 16]);
        assert_eq!(
            overlap.accesses("overlap", &catalog).unwrap_err().kind,
            "overlapping bindings require separate passes"
        );
    }

    #[test]
    fn deferred_scope_rejects_an_undeclared_destination_before_result() {
        let programs = Programs::new();
        let mut catalog = Catalog::new();
        let source = catalog.image("source", Format::Hdr, [16, 16], 1);
        let output = catalog.image("output", Format::Hdr, [16, 16], 1);
        let other = catalog.image("other", Format::Hdr, [16, 16], 1);
        let mut study = Study::new(catalog);
        let declared = compute(&programs.down, vec![source.mip(0), output.mip(0)], [16, 16]);
        let wrong = compute(&programs.down, vec![source.mip(0), other.mip(0)], [16, 16]);
        study
            .deferred("custom", declared, move |encoder, _| encoder.emit(&wrong))
            .unwrap();
        study.export(output).unwrap();
        assert_eq!(
            study.record().unwrap_err().kind,
            "command outside declared envelope"
        );
    }

    #[test]
    fn deferred_callback_borrows_frame_data_and_culled_callback_never_runs() {
        let programs = Programs::new();
        let draws = draws();
        let called = Cell::new(0);
        let mut catalog = Catalog::new();
        let output = catalog.image("output", Format::Depth, [16, 16], 1);
        let mut study = Study::new(catalog);
        let data = PassData::Raster(RasterData {
            pipeline: &programs.shadow,
            colors: vec![],
            depth: Some(Attachment {
                view: output.mip(0),
                load: Load::Clear,
            }),
            draws: &draws,
        });
        study
            .deferred("dead", data, |encoder, data| {
                called.set(called.get() + 1);
                encoder.emit(data)
            })
            .unwrap();
        assert_eq!(called.get(), 0);
        assert!(study.record().unwrap().0.is_empty());
        assert_eq!(called.get(), 0);
    }

    #[test]
    fn owned_commands_outlive_source_draw_list() {
        let programs = Programs::new();
        let mut catalog = Catalog::new();
        let output = catalog.image("output", Format::Depth, [16, 16], 1);
        let mut study = Study::new(catalog);
        {
            let draws = draws();
            let data = PassData::Raster(RasterData {
                pipeline: &programs.shadow,
                colors: vec![],
                depth: Some(Attachment {
                    view: output.mip(0),
                    load: Load::Clear,
                }),
                draws: &draws,
            });
            study.owned("owned", |encoder| encoder.emit(&data)).unwrap();
        }
        study.export(output).unwrap();
        let (intents, _) = study.record().unwrap();
        assert_eq!(intents[0].commands.len(), 4);
    }

    #[test]
    fn disabled_bloom_is_identity_without_passes_for_both_timings() {
        let programs = Programs::new();
        for timing in [Timing::Owned, Timing::Deferred] {
            let mut catalog = Catalog::new();
            let input = catalog.image("hdr", Format::Hdr, [16, 16], 1);
            let mut study = Study::new(catalog);
            assert_eq!(
                bloom(&mut study, timing, &programs, input, false).unwrap(),
                input
            );
            assert!(study.passes.is_empty());
        }
    }

    #[test]
    fn live_callback_uses_borrowed_draws_and_empty_callback_fails() {
        let programs = Programs::new();
        let draws = draws();
        let called = Cell::new(0);
        for emit in [true, false] {
            let mut catalog = Catalog::new();
            let output = catalog.image("depth", Format::Depth, [16, 16], 1);
            let mut study = Study::new(catalog);
            let data = PassData::Raster(RasterData {
                pipeline: &programs.shadow,
                colors: vec![],
                depth: Some(Attachment {
                    view: output.mip(0),
                    load: Load::Clear,
                }),
                draws: &draws,
            });
            study
                .deferred("borrowed", data, |encoder, data| {
                    called.set(called.get() + 1);
                    if emit { encoder.emit(data) } else { Ok(()) }
                })
                .unwrap();
            study.export(output).unwrap();
            let result = study.record();
            if emit {
                assert_eq!(result.unwrap().0[0].commands.len(), 4);
            } else {
                assert_eq!(result.unwrap_err().kind, "callback emitted no commands");
            }
        }
        assert_eq!(called.get(), 2);
    }

    #[test]
    fn repeated_reads_are_legal_but_callback_cannot_omit_a_declared_output() {
        let programs = Programs::new();
        let mut catalog = Catalog::new();
        let hdr = catalog.image("hdr", Format::Hdr, [16, 16], 1);
        let normal = catalog.image("normal", Format::Normal, [16, 16], 1);
        let depth = catalog.image("depth", Format::Depth, [16, 16], 1);
        let source = catalog.image("source", Format::Hdr, [16, 16], 1);
        let repeated = compute(
            &programs.combine,
            vec![source.mip(0), source.mip(0), hdr.mip(0)],
            [16, 16],
        );
        assert_eq!(
            repeated.accesses("repeated-read", &catalog).unwrap().len(),
            2
        );
        let duplicate_writer = ComputePipeline {
            name: "invalid/duplicate-output",
            slots: &[Usage::StorageWrite, Usage::StorageWrite],
        };
        let duplicate = compute(&duplicate_writer, vec![hdr.mip(0), hdr.mip(0)], [16, 16]);
        assert_eq!(
            duplicate
                .accesses("duplicate-write", &catalog)
                .unwrap_err()
                .kind,
            "overlapping bindings require separate passes"
        );
        let color = |image: Image| Attachment {
            view: image.mip(0),
            load: Load::Clear,
        };
        let declared = PassData::Raster(RasterData {
            pipeline: &programs.mrt,
            colors: vec![color(hdr), color(normal)],
            depth: Some(color(depth)),
            draws: &[],
        });
        let actual = PassData::Raster(RasterData {
            pipeline: &programs.color,
            colors: vec![color(hdr)],
            depth: None,
            draws: &[],
        });
        let mut study = Study::new(catalog);
        study
            .deferred("omitted-output", declared, move |encoder, _| {
                encoder.emit(&actual)
            })
            .unwrap();
        study.export(normal).unwrap();
        assert_eq!(
            study.record().unwrap_err().kind,
            "callback omitted declared output"
        );
    }
}

//! Recording and explicit configuration changes for an initialized scene renderer.
use super::*;

/// The replacement views are active. Retry release of their predecessors
/// with `SceneRenderer::collect_retired_views`.
#[derive(Debug)]
pub struct RetiredViewError(Box<dyn std::error::Error>);
impl std::fmt::Display for RetiredViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "replacement views active, previous view release pending: {}",
            self.0
        )
    }
}
impl std::error::Error for RetiredViewError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.0.as_ref())
    }
}

/// Prepared views, reusable graph storage and the runtime used for their submissions.
pub struct SceneRenderer<R: BorrowMut<Runtime> = Runtime> {
    pub(super) jobs: Option<sashiko_renderer_core::jobs::SharedJobs>,
    pub(crate) limits: SceneLimits,
    pub(crate) definitions: Vec<MaterialDefinition>,
    pub(super) driver: ViewRenderer,
    pub(super) pool: GraphPool,
    pub(super) runtime: Option<R>,
    pub(super) mesh_budget: u64,
    pub(super) configured: Preparation,
    pub(super) streaming: Option<(Preparation, u64)>,
    pub(super) retired: Option<ViewRenderer>,
    pub(super) transfers: super::candidate::Transfers,
}
impl<R: BorrowMut<Runtime>> SceneRenderer<R> {
    /// Opt into workload-dependent parallel execution at a preparation boundary.
    /// The host, standard scene, graph and native recorder share the same team.
    pub fn prepare_jobs(
        &mut self,
        jobs: impl Into<sashiko_renderer_core::jobs::SharedJobs>,
    ) -> PipelineResult<()> {
        self.driver.check_idle()?;
        let jobs = jobs.into();
        self.runtime()
            .prepare_automatic_raster_recording(jobs.clone())?;
        self.configure_cpu_jobs(&jobs);
        self.jobs = Some(jobs);
        Ok(())
    }

    fn configure_cpu_jobs(&mut self, jobs: &sashiko_renderer_core::jobs::SharedJobs) {
        // Candidate classification and generated visible draws have different costs.
        // Thresholds are in objects per view and draws per contiguous program run.
        #[cfg(feature = "vulkan")]
        let updating_shadows = self.runtime().as_vulkan().is_some();
        #[cfg(not(feature = "vulkan"))]
        let updating_shadows = false;
        self.driver
            .set_standard_scene_jobs(jobs, StandardJobPolicy { updating_shadows });
        self.pool
            .set_raster_jobs(jobs.clone(), NonZeroUsize::new(6_144).unwrap());
        self.pool
            .set_generated_batch_size(NonZeroUsize::new(3_125).unwrap());
    }

    pub(crate) fn mesh_upload_budget(&self) -> u64 {
        self.mesh_budget
    }
    /// Recreate all views on this runtime at a change boundary. The plan describes
    /// the complete replacement working set, including other engine passes using
    /// the device. Return outstanding result/export owners first. A rejected working
    /// set or failed initialization preserves the old views. If old-view release
    /// fails after publication, the error is a [`RetiredViewError`].
    pub fn replace_working_set(&mut self, plan: PreparedSetup<'_>) -> PipelineResult<()> {
        self.replace_plan(plan, |plan, runtime, overlap, _| {
            plan.check_runtime(runtime)?;
            Ok(runtime.prepare_working_set(overlap.working_set())?)
        })
    }
    pub(crate) fn replace_fitted_working_set(
        &mut self,
        plan: PreparedSetup<'_>,
    ) -> PipelineResult<()> {
        self.replace_plan(plan, |plan, runtime, overlap, streaming_transfer| {
            Ok(runtime.prepare_fitted_working_set(
                overlap,
                plan.upload_capacity()?.max(streaming_transfer),
                plan.readback,
            )?)
        })
    }

    fn replace_plan(
        &mut self,
        plan: PreparedSetup<'_>,
        prepare: impl FnOnce(&PreparedSetup<'_>, &mut Runtime, &Preparation, u64) -> PipelineResult<()>,
    ) -> PipelineResult<()> {
        self.collect_retired_views()?;
        self.driver.check_idle()?;
        // Keep every transient class and program the original cameras could
        // need if the new pipeline rejects an asset or fails to initialize.
        // Registered images and buffers retain their separate native leases.
        let mut old = Preparation::new(plan.preparation().frames_in_flight());
        old.add_working_set(self.configured.working_set())?;
        let mut overlap = Preparation::new(old.frames_in_flight());
        overlap.add_alternatives(&[&old, plan.preparation()])?;
        let streaming_transfer = self.include_streaming(&mut overlap)?;
        let runtime = self.runtime.as_mut().expect("live renderer").borrow_mut();
        prepare(&plan, runtime, &overlap, streaming_transfer)?;
        let mut next = plan.begin_with_runtime(runtime)?.wait()?;
        if let Some(jobs) = &self.jobs {
            next.configure_cpu_jobs(jobs);
        }
        self.retired = Some(std::mem::replace(
            &mut self.driver,
            std::mem::take(&mut next.driver),
        ));
        std::mem::swap(&mut self.pool, &mut next.pool);
        std::mem::swap(&mut self.configured, &mut next.configured);
        std::mem::swap(&mut self.definitions, &mut next.definitions);
        self.limits = next.limits;
        self.mesh_budget = next.mesh_budget;
        drop(next);
        self.collect_retired_views()
            .map_err(|e| Box::new(RetiredViewError(e)) as _)
    }

    pub fn collect_retired_views(&mut self) -> PipelineResult<()> {
        if let Some(retired) = &mut self.retired {
            retired.clear(self.runtime.as_mut().expect("live renderer").borrow_mut())?;
            self.retired = None;
        }
        Ok(())
    }

    fn include_streaming(&self, set: &mut Preparation) -> PipelineResult<u64> {
        if let Some((streaming, transfer)) = &self.streaming {
            set.add_requirements(streaming.graph_requirements(), streaming.working_set())?;
            Ok(*transfer)
        } else {
            Ok(0)
        }
    }

    /// Replace the host and scene resource reservations on the live device after
    /// returning outstanding frame results. The caller includes all ordinary passes.
    /// Stream reservations, registered assets and camera histories remain active.
    pub fn replace_resource_reservations(
        &mut self,
        set: Preparation,
        transfer: u64,
    ) -> PipelineResult<()> {
        if self.streaming.is_some() {
            let mut combined = Preparation::new(set.frames_in_flight());
            combined.add_requirements(set.graph_requirements(), set.working_set())?;
            let streaming_transfer = self.include_streaming(&mut combined)?;
            self.apply_native_set(&combined, transfer.max(streaming_transfer))?;
        } else {
            self.apply_native_set(&set, transfer)?;
        }
        self.configured = set;
        Ok(())
    }

    fn apply_native_set(&mut self, set: &Preparation, transfer: u64) -> PipelineResult<()> {
        let limits = self.runtime().limits();
        let graph = set.replacement_limits(limits)?.graph;
        let pool = (graph != limits.graph)
            .then(|| GraphPool::new(graph, set.frames_in_flight().get()))
            .transpose()?;
        self.runtime()
            .prepare_fitted_working_set(set, transfer, 0)?;
        if let Some(pool) = pool {
            self.pool = pool;
        }
        Ok(())
    }
    pub(super) fn install_asset_reservations(
        &mut self,
        texture_name: &str,
        buffers: &[sashiko_renderer_core::graph::BufferDesc],
        images: &[ImageDesc],
        samplers: &[sashiko_renderer_core::program::SamplerDescriptor],
    ) -> PipelineResult<()> {
        let frames = self.configured.frames_in_flight();
        let mut streaming = Preparation::new(frames);
        for sampler in samplers {
            streaming.reserve_sampler(*sampler);
        }
        for &desc in buffers {
            streaming.add_working_set(WorkingSet {
                resources: &[ResourceClass::Buffer { desc, count: 1 }],
                ..Default::default()
            })?;
        }
        let mut transfer = 0;
        let mut uploads = Vec::new();
        for &desc in images {
            streaming.add_working_set(WorkingSet {
                resources: &[ResourceClass::Image { desc, count: 1 }],
                ..Default::default()
            })?;
            if !uploads.iter().any(|(already, _)| *already == desc) {
                self.check_texture_support(desc, false)?;
                let mut upload = Preparation::new(frames);
                transfer =
                    transfer.max(reserve_texture_upload(&mut upload, texture_name, desc, 1)?);
                let mut declarations = Preparation::new(frames);
                declarations.add_graph_requirements(upload.graph_requirements())?;
                uploads.push((desc, declarations));
            }
        }
        streaming.add_alternatives(&uploads.iter().map(|(_, p)| p).collect::<Vec<_>>())?;
        let mut combined = Preparation::new(frames);
        combined.add_requirements(
            self.configured.graph_requirements(),
            self.configured.working_set(),
        )?;
        combined.add_requirements(streaming.graph_requirements(), streaming.working_set())?;
        self.apply_native_set(&combined, transfer)?;
        self.streaming = Some((streaming, transfer));
        Ok(())
    }

    /// Publish changed asset revisions within the prepared limits. Invoke after
    /// submitting or discarding graphs that refer to previous revisions. Accepted
    /// uploads survive errors so this maintenance operation can be retried.
    pub fn prepare_assets(
        &mut self,
        assets: SceneAssets<'_>,
    ) -> Result<(), crate::AssetPrepareError> {
        assets.meshes.collect_gpu(self.runtime())?;
        assets.materials.collect_gpu(self.runtime())?;
        self.prepare_assets_inner(assets).map_err(Into::into)
    }
    pub(crate) fn prepare_assets_inner(&mut self, assets: SceneAssets<'_>) -> PipelineResult<()> {
        let mut candidate = AssetCandidate::from_scene(&assets)?;
        let budget = UploadBudget {
            bytes: self
                .mesh_budget
                .max(crate::passes::pbr::MATERIAL_BYTES as u64),
            submissions: 1,
        };
        while self
            .advance_candidate(
                &mut candidate,
                assets.meshes,
                assets.materials,
                budget,
                Duration::from_secs(1),
            )?
            .is_pending()
        {}
        self.end_candidate(
            &mut candidate,
            assets.meshes,
            assets.materials,
            RetireAssets::default(),
            true,
        )
    }
    pub fn begin_frame(&mut self) -> PipelineResult<SceneFrame<'_>> {
        Ok(SceneFrame {
            frame: self.driver.begin_frame(&mut self.pool)?,
            runtime: self.runtime.as_mut().expect("live renderer").borrow_mut(),
        })
    }
    pub fn camera_cut(&mut self, id: ViewId) -> PipelineResult<()> {
        self.driver.camera_cut(id)
    }

    /// Record on a host-allocated Graph while the host keeps access to the Runtime
    /// for scheduling and observing its own submissions.
    pub fn begin_host_frame(
        &mut self,
        graph: sashiko_renderer_core::graph::Graph,
    ) -> PipelineResult<(ViewFrame<'_>, &mut Runtime)> {
        let frame = self.driver.begin_host_frame(graph)?;
        Ok((
            frame,
            self.runtime.as_mut().expect("live renderer").borrow_mut(),
        ))
    }
    pub fn finish_host_submission(&mut self, receipt: &FrameSubmission) -> PipelineResult<()> {
        self.driver.finish_host_submission(receipt)
    }
    pub fn runtime(&mut self) -> &mut Runtime {
        self.runtime.as_mut().expect("live renderer").borrow_mut()
    }
    pub fn finish_submission(&mut self) -> PipelineResult<FrameSubmission> {
        self.driver.finish_submission()
    }
    /// Upload immutable scene lighting at an explicit change boundary. The temporary
    /// upload arena is released before normal frame recording resumes.
    pub fn load_probe_lighting(
        &mut self,
        assets: ProbeLighting<'_>,
    ) -> PipelineResult<ResidentBakedLighting> {
        self.upload_baked_lighting(assets, None)
    }

    pub fn load_surface_lighting(
        &mut self,
        surfaces: &crate::pbr::SurfaceLightmaps,
        reflections: Option<&crate::pbr::ReflectionProbes>,
    ) -> PipelineResult<ResidentBakedLighting> {
        self.upload_baked_lighting(
            ProbeLighting {
                diffuse: surfaces.probes(),
                reflections,
            },
            Some(surfaces),
        )
    }

    fn upload_baked_lighting(
        &mut self,
        assets: ProbeLighting<'_>,
        surfaces: Option<&crate::pbr::SurfaceLightmaps>,
    ) -> PipelineResult<ResidentBakedLighting> {
        if assets.diffuse.is_none()
            && assets.reflections.is_none()
            && surfaces.is_none_or(|s| s.pages.is_empty())
        {
            return Ok(ResidentBakedLighting {
                complete: true,
                pages: Vec::new(),
                placements: Default::default(),
                diffuse: None,
                reflections: None,
            });
        }
        let diffuse = assets.diffuse.map(|a| (a.image_desc(), a.rgba32f()));
        let reflections = assets.reflections.map(|a| (a.image_desc(), a.pixels()));
        let mut startup = Preparation::new(self.configured.frames_in_flight());
        startup.add_working_set(self.configured.working_set())?;
        let mut bytes = 0;
        let mut footprint = 0;
        for (desc, data) in [diffuse, reflections].into_iter().flatten().chain(
            surfaces
                .into_iter()
                .flat_map(|s| s.pages.iter().map(|p| (p.image_desc(), p.rgba32f()))),
        ) {
            startup.add_working_set(WorkingSet {
                resources: &[ResourceClass::Image { desc, count: 1 }],
                ..Default::default()
            })?;
            bytes = bytes.max(data.len());
            footprint = footprint.max(super::transfer_footprint(desc)?);
        }
        let has_metadata = usize::from(reflections.is_some());
        if has_metadata != 0 {
            startup.add_working_set(WorkingSet {
                resources: &[ResourceClass::Buffer {
                    desc: crate::pbr::ReflectionProbes::metadata_desc(),
                    count: 1,
                }],
                ..Default::default()
            })?;
            bytes = bytes
                .checked_add(crate::pbr::ReflectionProbes::METADATA_BYTES)
                .ok_or("reflection upload size overflow")?;
            footprint = footprint
                .checked_add(
                    crate::pbr::ReflectionProbes::metadata_desc()
                        .size
                        .next_multiple_of(512),
                )
                .ok_or("reflection footprint overflow")?;
        }
        startup.add_graph_requirements(GraphRequirements {
            resources: 1 + has_metadata,
            passes: 1 + has_metadata,
            accesses: 1 + has_metadata,
            upload_bytes: bytes,
            name_bytes: 96,
            ..Default::default()
        })?;
        let mut pool = GraphPool::new(startup.graph_capacity(Default::default())?, 1)?;
        let streaming_transfer = self.include_streaming(&mut startup)?;
        let runtime = self.runtime();
        runtime.prepare_fitted_working_set(&startup, footprint.max(streaming_transfer), 0)?;
        let mut upload =
            |name: &str, (desc, data): (ImageDesc, &[u8])| -> PipelineResult<ManagedImage> {
                let (frame, image) = runtime.upload_texture(&mut pool, name, desc, data)?;
                while runtime
                    .wait_for_frame(frame, Duration::from_secs(1))?
                    .is_none()
                {}
                Ok(image)
            };
        Ok(ResidentBakedLighting {
            complete: surfaces.is_none_or(|s| s.complete),
            pages: surfaces
                .into_iter()
                .flat_map(|s| &s.pages)
                .map(|p| upload("static irradiance", (p.image_desc(), p.rgba32f())))
                .collect::<Result<_, _>>()?,
            placements: surfaces.map_or_else(Default::default, |s| s.placements.clone()),
            diffuse: diffuse
                .map(|image| upload("baked diffuse lighting", image))
                .transpose()?,
            reflections: assets
                .reflections
                .map(|asset| -> PipelineResult<_> {
                    let mut graph = pool.begin()?;
                    let image = graph.upload_texture_for_export(
                        "reflection probes",
                        asset.image_desc(),
                        asset.pixels(),
                    )?;
                    let buffer = crate::pbr::ReflectionProbes::upload_metadata(
                        &mut graph,
                        &asset.metadata(),
                    )?;
                    graph.export_buffer(buffer)?;
                    let (submission, image, metadata) =
                        runtime.submit_with_image_and_buffer(&graph.compile()?, image, buffer)?;
                    while runtime
                        .wait_for_frame(submission.id(), Duration::from_secs(1))?
                        .is_none()
                    {}
                    Ok((image, metadata))
                })
                .transpose()?,
        })
    }

    /// Reserve additional physical images and upload space at a capacity-change boundary.
    /// Include old and candidate versions that may overlap. Call once per added capacity.
    pub fn prepare_texture(
        &mut self,
        name: &str,
        desc: ImageDesc,
        simultaneous: usize,
    ) -> PipelineResult<()> {
        self.check_texture_support(desc, false)?;
        let mut next = Preparation::new(self.configured.frames_in_flight());
        next.add_requirements(
            self.configured.graph_requirements(),
            self.configured.working_set(),
        )?;
        let bytes = reserve_texture_upload(&mut next, name, desc, simultaneous)?;
        self.replace_resource_reservations(next, bytes)
    }

    /// Access the underlying components together at an explicit engine change boundary.
    pub fn parts_mut(&mut self) -> (&mut ViewRenderer, &mut GraphPool, &mut Runtime) {
        (
            &mut self.driver,
            &mut self.pool,
            self.runtime.as_mut().expect("live renderer").borrow_mut(),
        )
    }
    pub fn clear(&mut self) -> PipelineResult<()> {
        self.transfers
            .drain(self.runtime.as_mut().expect("live renderer").borrow_mut())?;
        self.collect_retired_views()?;
        self.driver
            .clear(self.runtime.as_mut().expect("live renderer").borrow_mut())
    }
    /// Release view-owned resources before transferring the runtime back to the host.
    pub fn into_runtime(mut self) -> PipelineResult<R> {
        self.clear()?;
        Ok(self.runtime.take().expect("live renderer"))
    }
}

impl<R: BorrowMut<Runtime>> Drop for SceneRenderer<R> {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.as_mut() {
            let _ = self.transfers.drain(runtime.borrow_mut());
            if let Some(retired) = &mut self.retired {
                let _ = retired.clear(runtime.borrow_mut());
            }
            let _ = self.driver.clear(runtime.borrow_mut());
        }
    }
}

/// Standard and custom views and engine passes share a graph and submission receipt.
pub struct SceneFrame<'a> {
    frame: ViewFrame<'a>,
    runtime: &'a mut Runtime,
}
impl<'a> Deref for SceneFrame<'a> {
    type Target = ViewFrame<'a>;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}
impl DerefMut for SceneFrame<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.frame
    }
}
impl<'a> SceneFrame<'a> {
    pub fn compile(self) -> Result<CompiledSceneFrame<'a>, GraphError> {
        Ok(CompiledSceneFrame {
            frame: self.frame.compile()?,
            runtime: self.runtime,
        })
    }
    pub fn submit(self) -> Result<FrameSubmission, ViewSubmitError> {
        self.frame.submit(self.runtime)
    }
}
pub struct CompiledSceneFrame<'a> {
    frame: CompiledViewFrame<'a>,
    runtime: &'a mut Runtime,
}
impl<'a> Deref for CompiledSceneFrame<'a> {
    type Target = CompiledViewFrame<'a>;
    fn deref(&self) -> &Self::Target {
        &self.frame
    }
}
impl CompiledSceneFrame<'_> {
    pub fn submit(self) -> Result<FrameSubmission, ViewSubmitError> {
        self.frame.submit(self.runtime)
    }
}

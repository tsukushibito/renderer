//! Fixed-storage accounting for the WSI acceptance fixture.

use super::super::{FrameCapacity, RuntimeConfig};
use super::PresentationConfig;
#[cfg(feature = "memory-profiling")]
use crate::measurement::{HostEvent, NativeEvent, RustEvent, Snapshot};

#[derive(Clone, Copy)]
pub(super) enum Phase {
    Setup,
    Normal,
    Shutdown,
}

pub(super) struct Measurements {
    #[cfg(feature = "memory-profiling")]
    start: Snapshot,
    #[cfg(feature = "memory-profiling")]
    caller: Snapshot,
    #[cfg(feature = "memory-profiling")]
    renderer: Snapshot,
    #[cfg(feature = "memory-profiling")]
    phases: [Snapshot; 3],
    #[cfg(feature = "memory-profiling")]
    normal_window: Option<(Snapshot, Snapshot)>,
    #[cfg(feature = "memory-profiling")]
    normal_process: Snapshot,
    #[cfg(feature = "memory-profiling")]
    normal_caller: Snapshot,
    #[cfg(feature = "memory-profiling")]
    normal_actions: Snapshot,
}

#[derive(Default)]
pub(super) struct Observations {
    pub(super) setup_sources: usize,
    pub(super) extra_profile_presents: usize,
    pub(super) normal_sources: usize,
    pub(super) normal_presents: usize,
    pub(super) normal_verified: usize,
    pub(super) pending_peak: usize,
    pub(super) held_peak: usize,
    pub(super) readback_peak: usize,
    pub(super) shutdown_presents: usize,
    pub(super) normal_extent: Option<(u32, u32)>,
    pub(super) normal_retired_swapchains: usize,
    pub(super) collected_retired_swapchains: usize,
}

impl Measurements {
    pub(super) fn new() -> Self {
        Self {
            #[cfg(feature = "memory-profiling")]
            start: Snapshot::now(),
            #[cfg(feature = "memory-profiling")]
            caller: Snapshot::default(),
            #[cfg(feature = "memory-profiling")]
            renderer: Snapshot::default(),
            #[cfg(feature = "memory-profiling")]
            phases: [Snapshot::default(); 3],
            #[cfg(feature = "memory-profiling")]
            normal_window: None,
            #[cfg(feature = "memory-profiling")]
            normal_process: Snapshot::default(),
            #[cfg(feature = "memory-profiling")]
            normal_caller: Snapshot::default(),
            #[cfg(feature = "memory-profiling")]
            normal_actions: Snapshot::default(),
        }
    }

    pub(super) fn begin_normal(&mut self) {
        #[cfg(feature = "memory-profiling")]
        {
            assert!(self.normal_window.is_none());
            self.normal_window = Some((Snapshot::now(), self.caller));
        }
    }

    pub(super) fn end_normal(&mut self) {
        #[cfg(feature = "memory-profiling")]
        {
            let (start, caller_start) = self.normal_window.take().expect("normal epoch started");
            self.normal_process = Snapshot::now().since(start);
            self.normal_caller = self.caller.since(caller_start);
            // Shipping Renderer Rust work runs on this isolated fixture thread;
            // the caller bucket contains only synchronous winit/Window work.
            // Native/VMA callbacks may run on another thread: never subtract
            // those events, including callbacks arriving during event pumping.
            let mut normal = self.normal_process.since(self.normal_caller);
            normal.native = self.normal_process.native;
            normal.vma_host = self.normal_process.vma_host;
            self.normal_actions = self.phases[Phase::Normal as usize];
            self.renderer = self.renderer.since(self.normal_actions);
            self.renderer.accumulate(normal);
            self.phases[Phase::Normal as usize] = normal;
        }
    }

    pub(super) fn caller<T>(&mut self, operation: impl FnOnce() -> T) -> T {
        #[cfg(feature = "memory-profiling")]
        let before = Snapshot::now();
        let result = operation();
        #[cfg(feature = "memory-profiling")]
        self.caller.accumulate(Snapshot::now().since(before));
        result
    }

    pub(super) fn renderer<T>(&mut self, phase: Phase, operation: impl FnOnce() -> T) -> T {
        #[cfg(feature = "memory-profiling")]
        let before = Snapshot::now();
        let result = operation();
        #[cfg(feature = "memory-profiling")]
        {
            let delta = Snapshot::now().since(before);
            self.renderer.accumulate(delta);
            self.phases[phase as usize].accumulate(delta);
        }
        #[cfg(not(feature = "memory-profiling"))]
        let _ = phase;
        result
    }

    pub(super) fn finish(
        self,
        observed: Observations,
        runtime: RuntimeConfig,
        presentation: PresentationConfig,
        frame: FrameCapacity,
        extents: &[(u32, u32)],
        graph_slots: usize,
    ) {
        assert_eq!(observed.normal_sources, 18);
        assert_eq!(observed.normal_presents, 36);
        assert_eq!(observed.normal_verified, 18);
        assert_eq!(observed.pending_peak, 3);
        assert_eq!(observed.held_peak, 3);
        assert!(observed.readback_peak >= 6);
        assert_eq!(observed.shutdown_presents, 1);
        assert!(observed.normal_extent.is_some());
        assert!(observed.normal_retired_swapchains > 0);
        assert_eq!(
            observed.collected_retired_swapchains,
            observed.normal_retired_swapchains
        );

        #[cfg(not(feature = "memory-profiling"))]
        let _ = (runtime, presentation, frame, extents, graph_slots);

        #[cfg(feature = "memory-profiling")]
        {
            assert!(self.normal_window.is_none());
            let process = Snapshot::now().since(self.start);
            let baseline = cfg!(any(
                feature = "development-validation",
                feature = "vulkan-diagnostics",
                feature = "graph-diagnostics"
            ));
            let normal = self.phases[Phase::Normal as usize];
            // Capture counters before formatting or serializing this report.
            let counts = |snapshot: Snapshot| {
                serde_json::json!({
                    "rust": snapshot.rust.map(|event| [event.calls, event.bytes]),
                    "vma_host": snapshot.vma_host.map(|event| [event.calls, event.bytes]),
                    "native": snapshot.native,
                    "overflowed": snapshot.overflowed,
                })
            };
            println!(
                "WSI_FRAME_MEMORY {}",
                serde_json::json!({
                    "mode": if baseline { "diagnostic_baseline" } else { "require_zero" },
                    "features": {
                        "optimized": !cfg!(debug_assertions),
                        "development_validation": cfg!(feature = "development-validation"),
                        "vulkan_diagnostics": cfg!(feature = "vulkan-diagnostics"),
                        "graph_diagnostics": cfg!(feature = "graph-diagnostics"),
                        "memory_profiling": true,
                        "gpu_profiling": false,
                    },
                    "tracing_filter": std::env::var("RUST_LOG").unwrap_or_else(|_| "renderer=info,renderer_core::vulkan::validation=warn".to_owned()),
                    "counter_order": {
                        "rust": ["allocate", "allocate_zeroed", "reallocate", "deallocate"],
                        "vma_host": ["allocate", "reallocate", "deallocate"],
                        "native": ["object_create", "object_destroy", "resource_create", "resource_destroy", "memory_allocate", "memory_free", "descriptor_allocate", "descriptor_free", "reset"],
                    },
                    "normal": {
                        "sources": observed.normal_sources,
                        "accepted_presents": observed.normal_presents,
                        "verified_readbacks": observed.normal_verified,
                        "max_pending_graphs": observed.pending_peak,
                        "max_held_cpu_results": observed.held_peak,
                        "max_live_readbacks": observed.readback_peak,
                        "extent": observed.normal_extent,
                        "retained_eligible_old_swapchains": observed.normal_retired_swapchains,
                        "changing_inputs": "transform, tint, clear and zero/one/two Invert passes; every source is built and compiled",
                        "scope": "all Renderer preparation checks, build/compile, submission, GPU/present polling, CPU readback, metadata/result/output return and final present drain",
                    },
                    "setup_change_sources": observed.setup_sources,
                    "setup_change_accepted_presents": observed.setup_sources * 2 + observed.extra_profile_presents,
                    "profiled_accepted_presents": 2 + observed.extra_profile_presents,
                    "explicitly_collected_old_swapchains_after_normal": observed.collected_retired_swapchains,
                    "extra_shutdown_accepted_presents": observed.shutdown_presents,
                    "prepared": {
                        "runtime": {
                            "execution_slots": runtime.execution_slots,
                            "resource_capacity": runtime.resource_capacity,
                            "staging_bytes_each": runtime.staging_bytes,
                            "staging_ranges": runtime.staging_ranges,
                            "max_shader_bindings": runtime.max_shader_bindings,
                        },
                        "presentation": {
                            "max_frames": presentation.max_frames,
                            "max_retired_swapchains": presentation.max_retired_swapchains,
                            "desired_images": presentation.desired_images,
                            "vsync_normal": presentation.vsync,
                        },
                        "graph_slots": graph_slots,
                        "mesh_graph_slots": 1,
                        "graph": {
                            "resources": frame.graph.resources, "passes": frame.graph.passes,
                            "accesses": frame.graph.accesses, "draws": frame.graph.draws,
                            "bindings": frame.graph.bindings, "attachments": frame.graph.attachments,
                            "dependencies": frame.graph.dependencies, "initial_ranges": frame.graph.initial_ranges,
                            "name_bytes": frame.graph.name_bytes, "upload_bytes": frame.graph.upload_bytes,
                            "edges": frame.graph.edges, "segments": frame.graph.segments, "readers": frame.graph.readers,
                        },
                        "frame_pools": {
                            "work_slots": frame.work_slots, "result_slots": frame.result_slots,
                            "metadata_slots": frame.metadata_slots, "range_snapshots": frame.range_snapshots,
                            "initialized_ranges_per_resource": frame.initialized_ranges_per_resource,
                            "provenance_segments_per_resource": frame.provenance_segments_per_resource, "index_prefixes_per_resource": frame.index_prefixes_per_resource,
                        },
                        "image_extents": extents,
                        "rgba8_instances_per_extent": 15,
                        "depth_instances_per_extent": 3,
                    },
                    "overall_process": counts(process),
                    "caller": counts(self.caller),
                    "renderer_total": counts(self.renderer),
                    "renderer_setup_change": counts(self.phases[Phase::Setup as usize]),
                    "renderer_normal": counts(normal),
                    "normal_process": counts(self.normal_process),
                    "normal_caller": counts(self.normal_caller),
                    "renderer_normal_actions": counts(self.normal_actions),
                    "row_semantics": "normal Rust excludes synchronous caller work; normal native/VMA cover the entire epoch, so caller and renderer rows are not disjoint native/VMA totals",
                    "renderer_shutdown": counts(self.phases[Phase::Shutdown as usize]),
                    "caller_scope": "synchronous winit/Window Rust work on the isolated fixture thread, with no Renderer Rust workers; native/VMA events are never excluded from the complete normal epoch",
                    "setup_change_scope": "explicit pools and native working set, residency, warm-up, resize/VSync/suspend/acquire-cancel and their completed owner retirement",
                    "shutdown_scope": "one additional ordinary-builder source retains pending presentation ownership for close; complete native owner teardown",
                    "host_callback_scope": "VMA and driver host requests routed through installed allocation callbacks; request origin is not distinguished",
                    "unobserved": "driver-internal allocation outside installed callbacks; Renderer native requests are counted separately",
                })
            );
            assert!(!process.overflowed && !self.renderer.overflowed && !self.caller.overflowed);
            assert!(self.phases.iter().all(|phase| !phase.overflowed));
            if !baseline {
                let startup = self.phases[Phase::Setup as usize];
                let shutdown = self.phases[Phase::Shutdown as usize];
                assert!(startup.rust[RustEvent::Allocate as usize].calls > 0);
                assert!(shutdown.rust[RustEvent::Deallocate as usize].calls > 0);
                for event in [
                    NativeEvent::ObjectCreate,
                    NativeEvent::ResourceCreate,
                    NativeEvent::DescriptorAllocate,
                ] {
                    assert!(
                        startup.native[event as usize] > 0,
                        "missing setup native counter: {event:?}"
                    );
                }
                for event in [
                    NativeEvent::ObjectDestroy,
                    NativeEvent::ResourceDestroy,
                    NativeEvent::DescriptorFree,
                ] {
                    assert!(
                        shutdown.native[event as usize] > 0,
                        "missing shutdown native counter: {event:?}"
                    );
                }
                assert!(startup.vma_host[HostEvent::Allocate as usize].calls > 0);
                assert!(shutdown.vma_host[HostEvent::Deallocate as usize].calls > 0);
                assert!(
                    normal.is_allocation_free(),
                    "normal WSI frame allocations: {normal:#?}"
                );
            }
        }
    }
}

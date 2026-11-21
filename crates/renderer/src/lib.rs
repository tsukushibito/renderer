//! Compose public Renderer Graphs with reusable drawing passes and recipes.
//!
//! Start with [`pipeline::RendererSetup::standard`] and [`pipeline::SceneAssets`].
//! Blocking or incremental setup derives the view, graph and GPU reservations from
//! the scene. [`pipeline::PreparedSetup`] lets an engine compose additional passes
//! or attach its own runtime. A [`pipeline::SceneFrame`] can record multiple cameras,
//! application passes and output in one submission.
//!
//! Choose the GPU backends on the host application's `renderer` dependency, or
//! forward them through this crate's matching features. A reusable pipeline
//! plugin can use the same implementation in CPU tools and native games.
//!
//! Use [`material::Materials`] to create shareable PBR, vertex-color Unlit or
//! custom materials. Submit their handles together through [`pipeline::StandardPipeline`],
//! which classifies draws and sorts transparent geometry across shader families.
//!
//! Keep your application scene above this layer. Register geometry with
//! [`mesh::Meshes`], prepare changed meshes and materials, then bind a
//! [`pipeline::SceneInput`] once per pipeline using [`pipeline::ViewFrame::bind_scene`].
//! Pass that binding to [`pipeline::ViewFrame::draw_view`] for each camera.
//! Use [`pipeline::ViewFrame::graph`] to insert post-processing before SDR output.
//! The executable examples are
//! `textured_view`, `two_views`, `toon_views`, and `mixed_post`. Implement
//! [`pipeline::ViewPipeline`] to use the same [`pipeline::ViewRenderer`] with a custom technique.
//!
//! # Choosing a PBR recipe
//!
//! Select the standard technique through [`pipeline::GeometryPipeline::new`] and reuse
//! the same frame API. Indexed draws are supported by each recipe. The table
//! shows the additional draw forms, MSAA and Program options.
//!
//! | Recipe | Samples | Opaque and masked | Shadow casters | Transparent | Programs |
//! | --- | --- | --- | --- | --- | --- |
//! | [`pbr_forward::prepare`] | 1×, 4× | Indexed, indirect, instanced | Indexed, instanced | Indexed, instanced | Built in |
//! | [`pbr_forward::prepare_with_programs`] | 1× | Indexed | Indexed | Indexed | Custom Forward ABI |
//! | [`pbr_forward_plus::prepare`] | 1×, 4× | Indexed, indirect, instanced | Indexed, instanced | Indexed, instanced | Built in |
//! | [`pbr_deferred::prepare`] | 1× | Indexed, indirect, instanced | Indexed, instanced | Indexed, instanced | Built in |
//!
//! Set [`pbr::SceneLimits::max_transforms`] to the required transform capacity.
//! Instanced draws may span multiple transform pages, and an opaque indirect group
//! can share one transform. Indexed draws
//! without transform operands keep their world-space vertex contract.
//! Transparent draws are supplied in back-to-front order. A 4× scene resolves
//! its color to a single-sample HDR image and has no scene depth output.
//! Set [`pbr::SceneLimits::max_point_lights`] for Forward, Forward+ or Deferred.
//! Forward+ culls lights by screen tile. Deferred shades its G-buffer before
//! compositing transparent draws.
//!
//! For HDR composition, add the scene and application passes to the same Graph
//! before adding the standard output stage. Drop or reset the frame Graph after
//! a recipe declaration fails, since a recipe can append more than one pass.

#![deny(unsafe_code)]

/// Shared graph, runtime and native interoperation APIs used by drawing passes.
pub use renderer_core as core;

pub mod material;
pub mod mesh;
pub mod passes;
pub mod pbr;
pub mod pbr_deferred;
pub mod pbr_forward;
pub mod pbr_forward_plus;
pub mod pipeline;
mod shader_binaries;
pub mod unlit_forward;

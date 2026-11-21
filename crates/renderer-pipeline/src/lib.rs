//! Compose public Renderer Graphs with reusable drawing passes and recipes.
//!
//! Register their resource and Program requirements in
//! [`renderer::capacity::Preparation`].
//! Prepare resource and Program reservations, then append standard and
//! application-specific passes to the same Graph. Renderer owns native
//! preparation, scheduling and execution.
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
//! [`mesh::Meshes`], prepare changed meshes and materials, then pass the same
//! [`pipeline::SceneInput`] to [`pipeline::ViewFrame::draw_view`] for
//! each camera. Use [`pipeline::ViewFrame::graph`] to insert your
//! own post-processing before SDR output. The executable examples are
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
//! A positive [`pbr::SceneLimits::max_transforms`] enables instanced draws
//! and one shared transform per opaque indirect group. Indexed draws
//! without transform operands keep their world-space vertex contract.
//! Transparent draws are supplied in back-to-front order. A 4× scene resolves
//! its color to a single-sample HDR image and has no scene depth output.
//! Forward+ accepts point lights for the view. Deferred shades its G-buffer
//! before compositing the caller's transparent draws.
//!
//! For HDR composition, add the scene and application passes to the same Graph
//! before adding the standard output stage. Drop or reset the frame Graph after
//! a recipe declaration fails, since a recipe can append more than one pass.

#![deny(unsafe_code)]

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

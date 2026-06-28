//! wgpu-backed render device for Noesis. Standalone module — does not pull
//! in Bevy types so it can be tested with a hand-built wgpu instance.
//!
//! See `../../../noesis_runtime/docs/PHASE_1_PLAN.md` for the FFI design and the
//! parent `CLAUDE.md` for the per-phase milestones.
//!
//! Submodules:
//!
//! - [`wgpu_device`] — the `RenderDevice` trait impl on top of wgpu.
//! - [`pipeline`] — pipeline cache keyed on `(shader, render_state, vertex_format)`.
//! - [`vertex_layout`] — build `wgpu::VertexBufferLayout` from a Noesis
//!   `VertexFormat` index.
//! - [`shader_defines`] — `Shader::Enum` → WGSL preprocessor define set.
//! - [`shader_preproc`] — minimal `#ifdef` stripper for `noesis.wgsl`.

pub mod pipeline;
pub mod shader_defines;
pub mod shader_preproc;
pub mod vertex_layout;
pub mod wgpu_device;

pub use wgpu_device::WgpuRenderDevice;

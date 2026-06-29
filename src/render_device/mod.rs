//! wgpu-backed render device for Noesis. This module pulls in no Bevy types, so
//! you can drive it with a hand-built wgpu instance in tests.
//!
//! Submodules:
//!
//! - [`wgpu_device`]: the `RenderDevice` trait impl on top of wgpu.
//! - [`pipeline`]: pipeline cache keyed on `(shader, render_state, vertex_format)`.
//! - [`vertex_layout`]: builds a `wgpu::VertexBufferLayout` from a Noesis
//!   `VertexFormat` index.
//! - [`shader_defines`]: `Shader::Enum` to WGSL preprocessor define set.
//! - [`shader_preproc`]: minimal `#ifdef` stripper for `noesis.wgsl`.

pub mod pipeline;
pub mod shader_defines;
pub mod shader_preproc;
pub mod vertex_layout;
pub mod wgpu_device;

pub use wgpu_device::WgpuRenderDevice;

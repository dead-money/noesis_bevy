//! The wgpu-backed Noesis render device.
//!
//! [`WgpuRenderDevice`] implements Noesis's `RenderDevice` on top of a
//! `wgpu::Device` / `wgpu::Queue` pair. [`NoesisRenderPlugin`] builds one
//! against Bevy's shared device and registers it with Noesis, so most apps
//! never touch this module. Reach for it directly to render Noesis without
//! the plugin, for example from a test with a hand-built wgpu instance: this
//! module uses no Bevy types beyond logging.
//!
//! The device is driven from inside Noesis's render calls, on whichever
//! thread owns the Noesis `View` and `Renderer` (the main world, under the
//! plugin). It renders into `Rgba8Unorm` targets only.
//!
//! Submodules:
//!
//! - [`wgpu_device`]: the device itself.
//! - [`pipeline`]: lazy pipeline cache keyed on shader, render state, vertex
//!   format, and stencil presence.
//! - [`vertex_layout`]: `wgpu::VertexBufferLayout` attributes for a Noesis
//!   vertex format.
//! - [`shader_defines`]: the WGSL define set for each Noesis shader.
//! - [`shader_preproc`]: the `#ifdef` stripper that turns `noesis.wgsl` into
//!   one shader variant.
//!
//! [`NoesisRenderPlugin`]: crate::render::NoesisRenderPlugin

pub mod pipeline;
pub mod shader_defines;
pub mod shader_preproc;
pub mod vertex_layout;
pub mod wgpu_device;

pub use wgpu_device::WgpuRenderDevice;

//! wgpu suite: tests that drive `WgpuRenderDevice` directly, and tests that run
//! Noesis views on it without a Bevy app.
//!
//! Each source file in this directory is one `#[test]` module, linked into one
//! binary. Run it under cargo-nextest so every test gets its own process; Noesis
//! state is process-global and thread-affine. See `tests/README.md`.

#[path = "../common/mod.rs"]
mod common;

mod headless_offscreen_brush;
mod headless_xaml;
mod headless_xaml_nested;
mod wgpu_effects;
mod wgpu_first_triangle;
mod wgpu_geometry_stream;
mod wgpu_multi_shader;
mod wgpu_offscreen_rt;
mod wgpu_pattern;
mod wgpu_pattern_wrap;
mod wgpu_ppaa_blit;
mod wgpu_radial;
mod wgpu_sdf_lcd;
mod wgpu_shadow_blur;
mod wgpu_stencil_clip;
mod wgpu_uniform_ring;

//! wgpu device suite (audit R2): direct-wgpu render tests + the Noesis-on-wgpu render-device tests.
//!
//! Each source file in this directory is one `#[test]` module. The suite
//! links Bevy + Noesis once for all of them instead of once per file, and
//! runs under cargo-nextest so every test still gets its own process (Noesis
//! state is process-global and thread-affine). See tests/README.md.

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

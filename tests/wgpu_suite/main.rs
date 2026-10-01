//! wgpu suite: tests that run Noesis views on `noesis_wgpu`'s
//! `WgpuRenderDevice` without a Bevy app, and the compositing blit. The
//! device's own GPU tests live in `noesis_wgpu`.
//!
//! Each source file in this directory is one `#[test]` module, linked into one
//! binary. Run it under cargo-nextest so every test gets its own process; Noesis
//! state is process-global and thread-affine. See `tests/README.md`.

#[path = "../common/mod.rs"]
mod common;

mod headless_offscreen_brush;
mod headless_xaml;
mod headless_xaml_nested;
mod wgpu_ppaa_blit;

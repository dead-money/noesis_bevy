//! Render-graph suite (audit R2): the few tests that need the real `DefaultPlugins` render graph.
//!
//! Each source file in this directory is one `#[test]` module. The suite
//! links Bevy + Noesis once for all of them instead of once per file, and
//! runs under cargo-nextest so every test still gets its own process (Noesis
//! state is process-global and thread-affine). See tests/README.md.

#[path = "../common/mod.rs"]
mod common;

mod headless_bake_label;
mod headless_compositing;
mod headless_intermediate_ghost;
mod headless_intermediate_ghost_removed;
mod headless_teardown;

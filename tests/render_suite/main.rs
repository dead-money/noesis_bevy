//! Render-graph suite: the tests that need the real `DefaultPlugins` render graph.
//!
//! Each source file in this directory is one `#[test]` module, linked into one
//! binary. Run it under cargo-nextest so every test gets its own process; Noesis
//! state is process-global and thread-affine. See `tests/README.md`.

#[path = "../common/mod.rs"]
mod common;

mod headless_bake_label;
mod headless_compositing;
mod headless_intermediate_ghost;
mod headless_intermediate_ghost_removed;
mod headless_teardown;

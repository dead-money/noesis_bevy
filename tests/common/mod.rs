//! Shared test harness for the Noesis integration tests.
//!
//! A `tests/common/` module (not `tests/common.rs`) is *not* compiled as its own
//! test binary; each test file pulls it in with `mod common;` and uses only the
//! helpers it needs (hence the crate-level `dead_code` allow).
//!
//! Two app shapes:
//!   * [`headless_app`] — `MinimalPlugins` + `AssetPlugin` + `InputPlugin` + the
//!     Noesis bridges + [`NoesisHeadlessPlugin`]. No `RenderPlugin`, no render
//!     graph, no pipeline compilation: bridge tests that assert messages (not
//!     pixels) run here, and never risk the teardown SIGSEGV that a
//!     mid-compile process exit causes (see `tests/render_suite/headless_bake_label.rs`).
//!   * [`render_app`] — the real `DefaultPlugins` engine, for the few tests that
//!     need the actual render graph. Drive it with [`run_until`] then [`settle`].
//!
//! Drive every app with [`run_until`] (stepping `app.update()`, no sleep), never
//! `app.run()`: Noesis is process-global and thread-affine, so it stays one
//! `#[test]` per process.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, Ordering};

use bevy::app::{PluginGroup, PluginsState};
use bevy::asset::AssetPlugin;
use bevy::input::InputPlugin;
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{NoesisHeadlessPlugin, NoesisLicense, NoesisPlugin};

/// One-Noesis-init-per-process interlock. Noesis' class/resource registration is
/// process-global and thread-affine, so two Noesis tests sharing a process is
/// undefined behavior (the teardown SIGSEGV). nextest runs each `#[test]` in its
/// own process, which resets this to `false`. Under a plain `cargo test`, every
/// `#[test]` in a suite binary shares one process, so the second Noesis init
/// trips this and fails loudly with instructions instead of crashing.
static NOESIS_CLAIMED: AtomicBool = AtomicBool::new(false);

/// Claim this process for a single Noesis-initializing test. Every entry point
/// that brings up the runtime calls it first; the second call in one process
/// panics. See [`NOESIS_CLAIMED`].
pub fn claim_noesis_process() {
    assert!(
        !NOESIS_CLAIMED.swap(true, Ordering::SeqCst),
        "second Noesis init in one process: these suites must run under \
         cargo-nextest (process-per-test). Use `cargo nextest run`, not \
         `cargo test`. See tests/README.md."
    );
}

/// The Noesis license from `NOESIS_LICENSE_NAME` / `NOESIS_LICENSE_KEY`, or
/// `None` (trial mode). Threaded into whichever plugin brings up the runtime.
#[must_use]
pub fn noesis_license_from_env() -> Option<NoesisLicense> {
    NoesisLicense::from_env()
}

/// Build a headless bridge-test app: every Noesis bridge driven against a
/// directly-requested wgpu device, with no `bevy_render` render graph.
///
/// `InputPlugin` registers the input message streams the input forwarders read;
/// the forwarders that also want a primary window are simply skipped (there is
/// none), exactly as under the old `WindowPlugin { primary_window: None }` setup.
/// Tests feed input through `NoesisInputQueue` directly instead.
pub fn headless_app() -> App {
    claim_noesis_process();
    let mut app = App::new();
    app.add_plugins((MinimalPlugins, AssetPlugin::default(), InputPlugin));
    NoesisPlugin::add_bridge_plugins(&mut app);
    app.add_plugins(NoesisHeadlessPlugin {
        license: noesis_license_from_env(),
    });
    app
}

/// Build a full-engine app for the few tests that need the real render graph
/// (`DefaultPlugins`, winit disabled, no primary window). Drive it with
/// [`run_until`] then [`settle`] so any in-flight pipeline compile drains before
/// the app drops.
pub fn render_app() -> App {
    claim_noesis_process();
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::winit::WinitPlugin>()
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            }),
    );
    app.add_plugins(NoesisPlugin {
        license: noesis_license_from_env(),
    });
    app
}

/// Finalize plugin setup the way `App::run` would, but for a manually-stepped
/// app: wait out any async plugin readiness (the render device on
/// `DefaultPlugins`), then `finish` + `cleanup` exactly once. `App::update` does
/// not do this itself, and without it `NoesisHeadlessPlugin::finish` never runs,
/// so `NoesisRenderState` is never inserted. Idempotent: once cleaned, a no-op.
fn finalize_plugins(app: &mut App) {
    if app.plugins_state() != PluginsState::Cleaned {
        while app.plugins_state() == PluginsState::Adding {
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();
    }
}

/// Step `app.update()` up to `max_frames` times with no sleep, stopping as soon
/// as `pred` returns `true` (checked after each update). Returns whether the
/// predicate ever passed, so callers can `assert!(run_until(...))` on a real
/// condition instead of padding a fixed frame count.
pub fn run_until(app: &mut App, max_frames: usize, mut pred: impl FnMut(&mut App) -> bool) -> bool {
    finalize_plugins(app);
    for _ in 0..max_frames {
        app.update();
        if pred(app) {
            return true;
        }
    }
    false
}

/// Pump `frames` extra `app.update()`s after a [`render_app`] test's assertion
/// has been satisfied. This is the pipeline-compile drain guard: a
/// `DefaultPlugins` app can have async render-pipeline compiles still running on
/// driver threads, and dropping the app mid-compile is the documented teardown
/// SIGSEGV. Headless tests compile no pipelines and do not need this.
pub fn settle(app: &mut App, frames: usize) {
    finalize_plugins(app);
    for _ in 0..frames {
        app.update();
    }
}

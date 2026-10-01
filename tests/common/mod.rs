//! Shared test harness for the Noesis integration tests.
//!
//! Each suite's `main.rs` includes this file with
//! `#[path = "../common/mod.rs"] mod common;`. Suites use only the helpers they
//! need, hence the `dead_code` allow.
//!
//! Two app shapes:
//!   * [`headless_app`]: `MinimalPlugins` + `AssetPlugin` + `InputPlugin` + the
//!     Noesis bridges + [`NoesisHeadlessPlugin`]. No render graph and no pipeline
//!     compilation, so bridge tests that assert messages (not pixels) can't hit
//!     the teardown SIGSEGV a mid-compile exit causes (see
//!     `tests/render_suite/headless_bake_label.rs`).
//!   * [`render_app`]: `DefaultPlugins`, for the few tests that need the real
//!     render graph. Drive it with [`run_until`] then [`settle`].
//!
//! Drive every app with [`run_until`] (steps `app.update()`, no sleep), never
//! `app.run()`. Noesis is process-global and thread-affine, so each `#[test]`
//! needs its own process; see [`claim_noesis_process`].

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

/// Claims this process for a single Noesis-initializing test. Every entry point
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

/// Builds a headless bridge-test app: every Noesis bridge driven against a
/// directly-requested wgpu device, with no `bevy_render` render graph.
///
/// `InputPlugin` registers the message streams the input forwarders read. There
/// is no primary window, so the window-bound forwarders do nothing; tests push
/// input onto `NoesisInputQueue` directly. Panics if this process already
/// initialized Noesis.
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

/// Builds a full-engine app for tests that need the real render graph
/// (`DefaultPlugins`, winit disabled, no primary window). Drive it with
/// [`run_until`] then [`settle`] so in-flight pipeline compiles drain before the
/// app drops. Panics if this process already initialized Noesis.
pub fn render_app() -> App {
    claim_noesis_process();
    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::winit::WinitPlugin>()
            // No test plays audio, and on a session-less CI runner the cpal/ALSA
            // backend can abort the process during teardown (free(): invalid
            // pointer after the test passes) when JACK/PulseAudio are absent.
            .disable::<bevy::audio::AudioPlugin>()
            // UI compositing never draws meshes; PBR's clustering and
            // mesh-preprocessing compute pipelines are the one shader family
            // whose in-driver JIT crashes (NVVM error 3, then DeviceLost) on
            // Ada GPUs under the wgpu 29 stack. Skip compiling them at all.
            .disable::<bevy::pbr::PbrPlugin>()
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

/// Finishes plugin setup the way `App::run` would for a manually stepped app:
/// waits for async plugin readiness (the render device on `DefaultPlugins`), then
/// calls `finish` + `cleanup` once. `App::update` skips this, and without it
/// `NoesisHeadlessPlugin::finish` never inserts `NoesisRenderState`. Idempotent.
fn finalize_plugins(app: &mut App) {
    if app.plugins_state() != PluginsState::Cleaned {
        while app.plugins_state() == PluginsState::Adding {
            bevy::tasks::tick_global_task_pools_on_main_thread();
        }
        app.finish();
        app.cleanup();
    }
}

/// Steps `app.update()` up to `max_frames` times with no sleep, stopping as soon
/// as `pred` returns `true` (checked after each update). Returns whether `pred`
/// ever passed.
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

/// Runs `frames` extra `app.update()`s after a [`render_app`] test's condition
/// holds. A `DefaultPlugins` app can still have pipeline compiles running on
/// driver threads, and dropping it mid-compile causes the teardown SIGSEGV.
/// Headless apps compile no pipelines and don't need this.
pub fn settle(app: &mut App, frames: usize) {
    finalize_plugins(app);
    for _ in 0..frames {
        app.update();
    }
}

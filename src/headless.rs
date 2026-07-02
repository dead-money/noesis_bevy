//! Headless harness entry point for tests and CI.
//!
//! [`NoesisHeadlessPlugin`] runs the full Noesis driving pipeline (the
//! [`NoesisSet`](crate::NoesisSet) Sync/Ensure/Apply/Drive phases every bridge
//! feeds) against a directly-requested wgpu device, **without** `bevy_render`'s
//! `RenderPlugin`/`RenderApp`. It is the cure for the nondeterministic teardown
//! SIGSEGV documented in `tests/render_suite/headless_bake_label.rs`: a `DefaultPlugins`
//! bridge test boots the real render graph and compiles pipelines it never
//! draws with, and can exit while a driver-thread pipeline compile is still in
//! flight. Bridge tests assert messages, not pixels, so they don't need any of
//! that.
//!
//! Compose it with `MinimalPlugins`, `AssetPlugin` (so XAML/font/image assets
//! still load through the registries), `InputPlugin` (so the input forwarders
//! have their message streams), and the bridge plugins:
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use bevy::input::InputPlugin;
//! use noesis_bevy::{NoesisHeadlessPlugin, NoesisPlugin};
//!
//! let mut app = App::new();
//! app.add_plugins((MinimalPlugins, AssetPlugin::default(), InputPlugin));
//! // Every bridge, minus the render pipeline plugin:
//! NoesisPlugin::add_bridge_plugins(&mut app);
//! app.add_plugins(NoesisHeadlessPlugin::default());
//! // Drive with `app.update()` in a loop; never `app.run()`.
//! ```
//!
//! The real [`NoesisPlugin`](crate::NoesisPlugin) is the app-facing entry point;
//! this one is the test/CI harness that swaps the render half for a raw device.

use bevy::prelude::*;
use bevy::window::{CursorLeft, CursorMoved, WindowFocused, WindowResized};

use crate::NoesisLicense;
use crate::render::{NoesisRenderState, build_main_world_pipeline};

/// Test/CI harness plugin: initializes the Noesis runtime, wires the main-world
/// driving pipeline, and inserts a [`NoesisRenderState`] backed by a
/// directly-requested wgpu device, with no `RenderApp`.
///
/// See the [module docs](self) for the composition it expects.
#[derive(Default)]
pub struct NoesisHeadlessPlugin {
    /// License to activate. Leave `None` to fall back to
    /// [`NoesisLicense::from_env`], matching [`NoesisPlugin`](crate::NoesisPlugin).
    pub license: Option<NoesisLicense>,
}

impl Plugin for NoesisHeadlessPlugin {
    fn build(&self, app: &mut App) {
        // Same runtime bring-up the real plugin does, so a `NoesisRenderState`
        // built in `finish` can register its device/providers with a live engine.
        crate::NoesisPlugin {
            license: self.license.clone(),
        }
        .init_runtime();

        // The Noesis input forwarders read these window message streams, which a
        // real `WindowPlugin` would register. Headless mode omits windowing, so
        // provide them here (empty, since no window is ever spawned) to keep the
        // forwarders from failing message-parameter validation. `add_message` is
        // idempotent, so this is harmless if a window plugin is present anyway.
        app.add_message::<CursorMoved>()
            .add_message::<CursorLeft>()
            .add_message::<WindowResized>()
            .add_message::<WindowFocused>();

        build_main_world_pipeline(app);
    }

    /// Request a wgpu device on the main thread (block-on'd) and insert the
    /// [`NoesisRenderState`] as a main-world non-send resource, pinning it and
    /// every Noesis handle it owns to the main thread. Runs in `finish` so it
    /// lands after `build` has initialized the runtime, mirroring how the real
    /// [`NoesisRenderPlugin`](crate::NoesisRenderPlugin) defers state creation.
    fn finish(&self, app: &mut App) {
        let (device, queue) = bevy::tasks::block_on(request_device());
        app.insert_non_send_resource(NoesisRenderState::new(device, queue));
    }
}

/// Request a wgpu instance/adapter/device the way the `wgpu_*` device tests do,
/// bypassing `bevy_render` entirely. The machine running these tests has a real
/// GPU; the adapter's own limits are requested so `request_device` can't fail on
/// a capability the adapter already advertises.
async fn request_device() -> (wgpu::Device, wgpu::Queue) {
    let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor::default());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
        })
        .await
        .expect("no wgpu adapter available for the headless Noesis test harness");
    adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("noesis headless test device"),
            required_features: wgpu::Features::empty(),
            required_limits: adapter.limits(),
            memory_hints: wgpu::MemoryHints::default(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            trace: wgpu::Trace::Off,
        })
        .await
        .expect("no wgpu device available for the headless Noesis test harness")
}

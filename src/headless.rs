//! Headless harness for tests and CI.
//!
//! [`NoesisHeadlessPlugin`] runs the full Noesis pipeline (the
//! [`NoesisSet`](crate::NoesisSet) phases every bridge feeds) against a wgpu
//! device it requests itself, without `bevy_render`'s `RenderPlugin` or
//! `RenderApp`. Bridge tests check messages, not pixels, so they skip the render
//! graph and its background pipeline compiles, which can still be in flight
//! when a `DefaultPlugins` test exits and crash teardown.
//!
//! Compose it with `MinimalPlugins`, `AssetPlugin` (so XAML, font, and image
//! assets still load), `InputPlugin` (so the input forwarders have their
//! message streams), and the bridge plugins:
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
//! Apps use [`NoesisPlugin`](crate::NoesisPlugin) instead. Noesis state is
//! process-global, so run one harness app per process.

use bevy::prelude::*;
use bevy::window::{CursorLeft, CursorMoved, WindowFocused, WindowResized};

use crate::NoesisLicense;
use crate::render::{NoesisRenderState, build_main_world_pipeline};

/// Initializes the Noesis runtime and drives every view on a wgpu device of its
/// own, with no `RenderApp`. See the [module docs](self) for the plugins it
/// expects alongside it.
///
/// # Panics
///
/// In `finish`, if no wgpu adapter or device is available.
#[derive(Default)]
pub struct NoesisHeadlessPlugin {
    /// License to activate. Leave `None` to fall back to
    /// [`NoesisLicense::from_env`], matching [`NoesisPlugin`](crate::NoesisPlugin).
    pub license: Option<NoesisLicense>,
}

impl Plugin for NoesisHeadlessPlugin {
    fn build(&self, app: &mut App) {
        crate::NoesisPlugin {
            license: self.license.clone(),
        }
        .init_runtime();

        // No `WindowPlugin` registers these, and the input forwarders fail
        // message-parameter validation without them. `add_message` is idempotent.
        app.add_message::<CursorMoved>()
            .add_message::<CursorLeft>()
            .add_message::<WindowResized>()
            .add_message::<WindowFocused>();

        build_main_world_pipeline(app);
    }

    /// Blocks on a wgpu device and inserts the Noesis state as a non-send
    /// resource, pinning every Noesis handle to the main thread. In `finish`
    /// so the runtime from `build` is already up.
    fn finish(&self, app: &mut App) {
        let (device, queue) = bevy::tasks::block_on(request_device());
        app.insert_non_send(NoesisRenderState::new(device, queue));
    }
}

/// Requests the adapter's own limits so `request_device` can't fail on a
/// capability the adapter advertises.
async fn request_device() -> (wgpu::Device, wgpu::Queue) {
    let instance =
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            ..Default::default()
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

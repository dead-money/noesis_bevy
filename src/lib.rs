//! Bevy plugin for Noesis GUI.
//!
//! Drives `libNoesis.so` (via the [`dm_noesis_runtime`] FFI crate) and — in later
//! phases — implements `Noesis::RenderDevice` on top of Bevy's wgpu device.
//! UIs render into an offscreen wgpu texture and composite into the Bevy frame.
//!
//! See `CLAUDE.md` for the phase plan. Currently at Phase 0: lifecycle only —
//! the plugin initializes Noesis at app startup and shuts it down at exit, but
//! does not yet render anything.

use bevy::prelude::*;

pub mod bake;
pub mod classes;
pub mod dp;
pub mod events;
pub mod focus;
pub mod font;
pub mod geometry;
pub mod image;
pub mod input;
pub mod items;
pub mod layout;
pub mod markup;
pub mod render;
pub mod render_device;
pub mod text;
pub mod theme;
pub mod viewmodel;
pub mod visibility;
pub mod xaml;

pub use bake::{NoesisLabelBaker, NoesisLabelBakerPlugin};
pub use classes::{NoesisClassPlugin, NoesisClassRegistry};
pub use dp::{
    DpKind, DpValue, DpWatch, NoesisDpChanged, NoesisDpPlugin, NoesisDpReadWatch, NoesisDpRequests,
    SharedDpChangedQueue,
};
pub use events::{
    Key, KeyDownWatchEntry, NoesisClickWatch, NoesisClicked, NoesisEventsPlugin, NoesisKeyDown,
    NoesisKeyDownWatch, SharedClickQueue, SharedKeyDownQueue,
};
pub use focus::{NoesisFocusPlugin, NoesisFocusRequests};
pub use font::{BevyFontProvider, FontAsset, FontAssetLoader, FontAssetPlugin, FontRegistry};
pub use geometry::{NoesisGeometryPlugin, NoesisGeometryRequests};
pub use image::{
    BevyTextureProvider, ImageAsset, ImageAssetLoader, ImageAssetPlugin, ImageRegistry,
};
pub use input::{NoesisInputEvent, NoesisInputPlugin, NoesisInputQueue};
pub use items::{ItemsBinding, NoesisItemsPlugin, NoesisItemsSources};
pub use layout::{NoesisLayoutPlugin, NoesisLayoutRequests};
pub use markup::{NoesisMarkupExtensionPlugin, NoesisMarkupExtensionRegistry};
pub use render::{NoesisCamera, NoesisRenderPlugin, NoesisScene};
pub use text::{
    NoesisTextChanged, NoesisTextPlugin, NoesisTextReadWatch, NoesisTextRequests,
    SharedTextChangedQueue,
};
pub use theme::NoesisDefaultThemePlugin;
pub use viewmodel::{
    NoesisViewModelChanged, NoesisViewModelPlugin, NoesisViewModels, SharedVmChangedQueue,
    ViewModelChangeForwarder, ViewModelDef, ViewModelId, VmValue,
};
pub use visibility::{NoesisVisibilityPlugin, NoesisVisibilityRequests};
pub use xaml::{BevyXamlProvider, XamlAsset, XamlAssetLoader, XamlAssetPlugin, XamlRegistry};

/// Per-developer Indie license credentials.
#[derive(Clone, Debug)]
pub struct NoesisLicense {
    pub name: String,
    pub key: String,
}

impl NoesisLicense {
    /// Read `NOESIS_LICENSE_NAME` and `NOESIS_LICENSE_KEY` from the environment.
    /// Returns `None` if either is unset.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let name = std::env::var("NOESIS_LICENSE_NAME").ok()?;
        let key = std::env::var("NOESIS_LICENSE_KEY").ok()?;
        Some(Self { name, key })
    }
}

/// Bevy plugin that initializes Noesis at app startup and shuts it down when
/// the [`App`] is dropped.
///
/// Falls back to `NoesisLicense::from_env()` when [`license`](Self::license)
/// is `None`. Without a license, Noesis runs in trial mode (visible watermark).
#[derive(Default)]
pub struct NoesisPlugin {
    pub license: Option<NoesisLicense>,
}

impl Plugin for NoesisPlugin {
    fn build(&self, app: &mut App) {
        if let Some(lic) = self.license.clone().or_else(NoesisLicense::from_env) {
            dm_noesis_runtime::set_license(&lic.name, &lic.key);
        }
        dm_noesis_runtime::init();

        info!("Noesis runtime version {}", dm_noesis_runtime::version());

        // Hold a !Send guard on the main app so Shutdown runs at App teardown
        // on the main thread. The render app's non-send resources (including
        // NoesisRenderState) drop before this guard thanks to Bevy 0.18's
        // App drop order, so the global shutdown() sees no live Noesis
        // objects.
        app.insert_non_send_resource(NoesisShutdownGuard);

        // Sub-plugins: XAML + font assets + the render-graph integration
        // + input forwarder. Safe to add unconditionally —
        // NoesisRenderPlugin no-ops if RenderApp isn't present (e.g. a
        // headless-test setup without a display).
        // Grouped into two nested tuples: asset/render/input infrastructure, and
        // the per-feature Bevy bridges. The nesting also keeps each tuple under
        // Bevy's 15-element `Plugins` impl limit.
        app.add_plugins((
            (
                xaml::XamlAssetPlugin,
                font::FontAssetPlugin,
                image::ImageAssetPlugin,
                render::NoesisRenderPlugin,
                input::NoesisInputPlugin,
            ),
            (
                events::NoesisEventsPlugin,
                classes::NoesisClassPlugin,
                markup::NoesisMarkupExtensionPlugin,
                visibility::NoesisVisibilityPlugin,
                layout::NoesisLayoutPlugin,
                text::NoesisTextPlugin,
                geometry::NoesisGeometryPlugin,
                focus::NoesisFocusPlugin,
                viewmodel::NoesisViewModelPlugin,
                items::NoesisItemsPlugin,
                dp::NoesisDpPlugin,
            ),
        ));
    }
}

struct NoesisShutdownGuard;

impl Drop for NoesisShutdownGuard {
    fn drop(&mut self) {
        dm_noesis_runtime::shutdown();
    }
}

//! Bevy plugin for Noesis GUI.
//!
//! Drives `libNoesis.so` (via the [`noesis_runtime`] FFI crate) and — in later
//! phases — implements `Noesis::RenderDevice` on top of Bevy's wgpu device.
//! UIs render into an offscreen wgpu texture and composite into the Bevy frame.
//!
//! See `CLAUDE.md` for the phase plan. Currently at Phase 0: lifecycle only —
//! the plugin initializes Noesis at app startup and shuts it down at exit, but
//! does not yet render anything.

use bevy::prelude::*;

pub mod bake;
pub mod brushes;
pub mod classes;
pub mod commands;
pub mod dp;
pub mod events;
pub mod focus;
pub mod focus_input;
pub mod font;
pub mod geometry;
pub mod image;
pub mod input;
pub mod items;
pub mod layout;
pub mod markup;
pub mod plain_vm;
pub mod render;
pub mod render_device;
pub mod routed_events;
pub mod text;
pub mod theme;
pub mod transforms;
pub mod viewmodel;
pub mod visibility;
pub mod visual_state;
pub mod xaml;

pub use bake::{NoesisLabelBaker, NoesisLabelBakerPlugin};
pub use brushes::{
    BrushReadback, BrushSpec, BrushTarget, GradientStop, NoesisBrushChanged, NoesisBrushes,
    NoesisBrushesPlugin,
};
pub use classes::{NoesisClassPlugin, NoesisClassRegistry};
pub use commands::{
    CommandForwarder, CommandsDef, NoesisCommandInvoked, NoesisCommands, NoesisCommandsPlugin,
    SharedCommandQueue,
};
/// Derive macro for [`NoesisViewModel`] — bind a plain struct's fields by name.
pub use dm_noesis_bevy_derive::NoesisViewModel;
pub use dp::{DpKind, DpValue, DpWatch, NoesisDp, NoesisDpChanged, NoesisDpPlugin};
pub use events::{
    Key, KeyDownWatchEntry, NoesisClickWatch, NoesisClicked, NoesisEventsPlugin, NoesisKeyDown,
    NoesisKeyDownWatch, SharedClickQueue, SharedKeyDownQueue,
};
pub use focus::{NoesisFocus, NoesisFocusPlugin};
pub use focus_input::{
    FocusMove, FocusNavigationDirection, FocusPredict, KeyBindingSpec, ModifierKeys,
    NoesisFocusBindingFired, NoesisFocusControl, NoesisFocusControlPlugin, NoesisFocusPredicted,
};
pub use font::{BevyFontProvider, FontAsset, FontAssetLoader, FontAssetPlugin, FontRegistry};
pub use geometry::{NoesisGeometry, NoesisGeometryPlugin};
pub use image::{
    BevyTextureProvider, ImageAsset, ImageAssetLoader, ImageAssetPlugin, ImageRegistry,
};
pub use input::{NoesisInputEvent, NoesisInputPlugin, NoesisInputQueue};
pub use items::{ItemsBinding, NoesisItems, NoesisItemsPlugin};
pub use layout::{Margin, NoesisLayout, NoesisLayoutPlugin};
pub use markup::{NoesisMarkupExtensionPlugin, NoesisMarkupExtensionRegistry};
pub use plain_vm::{NoesisViewModel, NoesisViewModelAppExt, PlainType, PlainValue, PlainValueRef};
pub use render::{NoesisCamera, NoesisIntermediate, NoesisRenderPlugin, NoesisSet, NoesisView};
pub use routed_events::{
    EventWatchEntry, MouseButton, NoesisEventWatch, NoesisRoutedEvent, NoesisRoutedEventsPlugin,
    RoutedEvent, RoutedEventSnapshot, SharedRoutedEventQueue,
};
pub use text::{NoesisText, NoesisTextChanged, NoesisTextPlugin};
pub use theme::NoesisDefaultThemePlugin;
pub use transforms::{
    NoesisTransform, NoesisTransformChanged, NoesisTransformPlugin, TransformSpec,
};
pub use viewmodel::{
    NoesisViewModelChanged, NoesisViewModelPlugin, NoesisVm, SharedVmChangedQueue,
    ViewModelChangeForwarder, ViewModelDef, VmValue,
};
pub use visibility::{NoesisVisibility, NoesisVisibilityPlugin};
pub use visual_state::{NoesisVisualState, NoesisVisualStatePlugin, StateRequest};
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
            noesis_runtime::set_license(&lic.name, &lic.key);
        }
        noesis_runtime::init();

        info!("Noesis runtime version {}", noesis_runtime::version());

        // Global `shutdown()` is owned by `NoesisRenderState::drop` — it releases
        // every live Noesis handle and then shuts the engine down, as its final
        // step, on the main thread. A separate guard can't guarantee it runs
        // *after* the state (Bevy gives no drop order between two main-world
        // resources), which is why the old `NoesisShutdownGuard` was removed.

        // Sub-plugins: XAML + font assets + the render-graph integration
        // + input forwarder. Safe to add unconditionally —
        // NoesisRenderPlugin no-ops if RenderApp isn't present (e.g. a
        // headless-test setup without a display).
        // Grouped into tuples kept under Bevy's 15-element `Plugins` impl limit:
        // asset/render/input infrastructure, then the per-feature Bevy bridges
        // split into two groups so new bridges have headroom (each new bridge
        // appends to `bridge_group_b`).
        app.add_plugins((
            xaml::XamlAssetPlugin,
            font::FontAssetPlugin,
            image::ImageAssetPlugin,
            render::NoesisRenderPlugin,
            input::NoesisInputPlugin,
        ));
        // Bridge group A — the foundational per-element bridges.
        app.add_plugins((
            events::NoesisEventsPlugin,
            routed_events::NoesisRoutedEventsPlugin,
            classes::NoesisClassPlugin,
            markup::NoesisMarkupExtensionPlugin,
            visibility::NoesisVisibilityPlugin,
            layout::NoesisLayoutPlugin,
            text::NoesisTextPlugin,
            geometry::NoesisGeometryPlugin,
        ));
        // Bridge group B — interaction + data bridges. New bridges append here.
        app.add_plugins((
            focus::NoesisFocusPlugin,
            visual_state::NoesisVisualStatePlugin,
            focus_input::NoesisFocusControlPlugin,
            viewmodel::NoesisViewModelPlugin,
            commands::NoesisCommandsPlugin,
            items::NoesisItemsPlugin,
            dp::NoesisDpPlugin,
            transforms::NoesisTransformPlugin,
            brushes::NoesisBrushesPlugin,
        ));
    }
}

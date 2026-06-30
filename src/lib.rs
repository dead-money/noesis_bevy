//! Bevy plugin for the Noesis GUI SDK.
//!
//! Drives `libNoesis.so` through the [`noesis_runtime`] FFI crate and implements
//! `Noesis::RenderDevice` on top of Bevy's wgpu device. UIs render into an
//! offscreen wgpu texture and composite into the Bevy frame.
//!
//! Add [`NoesisPlugin`] to initialize the runtime, then host XAML through a
//! [`NoesisView`] camera.
#![warn(missing_docs)]

use bevy::prelude::*;

pub mod animation;
pub mod bake;
pub mod binding;
pub mod brushes;
pub mod classes;
pub mod commands;
pub mod diagnostics;
pub mod dp;
pub mod events;
pub mod focus;
pub mod focus_input;
pub mod font;
pub mod geometry;
pub mod image;
pub mod imaging;
pub mod inlines;
pub mod input;
pub mod integration;
pub mod items;
pub mod layout;
pub mod list;
pub mod markup;
pub mod panel;
pub mod plain_vm;
pub mod reconcile;
pub mod render;
pub mod render_device;
pub mod resources;
pub mod routed_events;
pub mod shapes;
pub mod styles;
pub mod svg;
pub mod text;
pub mod theme;
pub mod transforms;
pub mod transforms3d;
pub mod typography;
pub mod ui;
pub mod viewmodel;
pub mod visibility;
pub mod visual_state;
pub mod window_compat;
pub mod xaml;

pub use animation::{AnimationSpec, NoesisAnimation, NoesisAnimationPlugin};
pub use bake::{NoesisLabelBaker, NoesisLabelBakerPlugin};
pub use binding::{
    BindingMode, ConvertArg, Converted, MultiValueConverter, NoesisBinding, NoesisBindingPlugin,
    SourceSpec, ValueConverter,
};
pub use brushes::{
    BrushReadback, BrushSpec, BrushTarget, GradientStop, NoesisBrushChanged, NoesisBrushes,
    NoesisBrushesPlugin,
};
pub use classes::{NoesisClassPlugin, NoesisClassRegistry};
pub use commands::{
    CommandForwarder, CommandsDef, NoesisCommandInvoked, NoesisCommands, NoesisCommandsPlugin,
    SharedCommandQueue,
};
pub use diagnostics::{NoesisDiagnostics, NoesisDiagnosticsPlugin};
pub use dp::{DpKind, DpValue, DpWatch, NoesisDp, NoesisDpChanged, NoesisDpPlugin};
pub use events::{
    ClickWatchEntry, Key, KeyDownWatchEntry, NoesisClickWatch, NoesisClicked, NoesisEventsPlugin,
    NoesisKeyDown, NoesisKeyDownWatch, SharedClickQueue, SharedKeyDownQueue, UiClicked, UiKeyDown,
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
pub use imaging::{
    ImageBitmap, ImageReadback, NoesisImageChanged, NoesisImaging, NoesisImagingPlugin,
};
pub use inlines::{
    InlineSpec, InlinesReadback, NoesisInlines, NoesisInlinesChanged, NoesisInlinesPlugin,
    TextDecorations,
};
pub use input::{NoesisInputEvent, NoesisInputPlugin, NoesisInputQueue};
pub use integration::{
    CursorType, NoesisCursorRequested, NoesisIntegrationPlugin, NoesisOpenUrl, NoesisPlayAudio,
    get_culture, open_url, play_audio, set_culture,
};
pub use items::{
    CollectionViewOp, ItemValue, ItemsBinding, NoesisItems, NoesisItemsCurrent, NoesisItemsPlugin,
    ObjectRow, ObjectSource,
};
pub use layout::{Margin, NoesisLayout, NoesisLayoutPlugin};
pub use list::{
    ListSort, ListedIn, NoesisListAppExt, NoesisListOps, NoesisListPlugin, NoesisListSelection,
    NoesisListSet, Selected, UiList,
};
pub use markup::{NoesisMarkupExtensionPlugin, NoesisMarkupExtensionRegistry};
/// Derive macro for [`NoesisViewModel`]: binds a plain struct's fields by name.
pub use noesis_bevy_derive::NoesisViewModel;
pub use panel::{
    NoesisPanelAppExt, NoesisPanelPlugin, NoesisPanelSet, NoesisPanelText, NoesisPanelTextChanged,
    SealPanel, UiPanel,
};
pub use plain_vm::{NoesisViewModel, NoesisViewModelAppExt, PlainType, PlainValue, PlainValueRef};
pub use render::{NoesisCamera, NoesisIntermediate, NoesisRenderPlugin, NoesisSet, NoesisView};
pub use resources::{
    NoesisResources, NoesisResourcesInstalled, NoesisResourcesPlugin, ResourceEntry,
};
pub use routed_events::{
    EventWatchEntry, MouseButton, NoesisEventWatch, NoesisRoutedEvent, NoesisRoutedEventsPlugin,
    RoutedEvent, RoutedEventSnapshot, SharedRoutedEventQueue, UiRoutedEvent,
};
pub use shapes::{NoesisShapes, NoesisShapesPlugin, ShapeKind, ShapeSpec};
pub use styles::{
    DataTriggerSpec, MultiTriggerSpec, NoesisStyles, NoesisStylesPlugin, PropertyTrigger, StyleSpec,
};
pub use svg::{NoesisSvg, NoesisSvgChanged, NoesisSvgPlugin};
pub use text::{NoesisText, NoesisTextChanged, NoesisTextPlugin};
pub use theme::NoesisDefaultThemePlugin;
pub use transforms::{
    NoesisTransform, NoesisTransformChanged, NoesisTransformPlugin, TransformSpec,
};
pub use transforms3d::{
    Matrix3DSpec, NoesisMatrixTransform3DChanged, NoesisTransform3D, NoesisTransform3DChanged,
    NoesisTransform3DPlugin, Transform3DSpec,
};
pub use typography::{
    FontStretch, FontStyle, FontStyling, FontWeight, NoesisTypography, NoesisTypographyChanged,
    NoesisTypographyPlugin, TypographyField, TypographyValue, TypographyWatch,
};
pub use ui::NoesisUi;
pub use viewmodel::{
    NoesisViewModelChanged, NoesisViewModelPlugin, NoesisVm, SharedVmChangedQueue,
    ViewModelChangeForwarder, ViewModelDef, VmValue,
};
pub use visibility::{NoesisVisibility, NoesisVisibilityPlugin};
pub use visual_state::{NoesisVisualState, NoesisVisualStatePlugin, StateRequest};
pub use window_compat::{NoesisWindowCompatPlugin, WINDOW_CLASS};
pub use xaml::{BevyXamlProvider, XamlAsset, XamlAssetLoader, XamlAssetPlugin, XamlRegistry};

/// Per-developer Indie license credentials.
#[derive(Clone, Debug)]
pub struct NoesisLicense {
    /// Licensee name, as issued with the Indie license.
    pub name: String,
    /// License key paired with [`name`](Self::name).
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
    /// License to activate. Leave `None` to fall back to
    /// [`NoesisLicense::from_env`], or to run in trial mode if the environment
    /// has no credentials either.
    pub license: Option<NoesisLicense>,
}

impl Plugin for NoesisPlugin {
    fn build(&self, app: &mut App) {
        if let Some(lic) = self.license.clone().or_else(NoesisLicense::from_env) {
            noesis_runtime::set_license(&lic.name, &lic.key);
        }
        noesis_runtime::init();

        info!("Noesis runtime version {}", noesis_runtime::version());

        // Global `shutdown()` is owned by `NoesisRenderState::drop`: it releases every
        // live Noesis handle then shuts the engine down, last, on the main thread.
        // A separate guard can't guarantee it runs after the state (Bevy gives no
        // drop order between two main-world resources).

        // NoesisRenderPlugin no-ops if RenderApp isn't present (headless tests).
        // Tuples are split to stay under Bevy's 15-element `Plugins` impl limit.
        app.add_plugins((
            xaml::XamlAssetPlugin,
            font::FontAssetPlugin,
            image::ImageAssetPlugin,
            render::NoesisRenderPlugin,
            input::NoesisInputPlugin,
            integration::NoesisIntegrationPlugin,
        ));
        // Group A: foundational per-element bridges.
        app.add_plugins((
            events::NoesisEventsPlugin,
            routed_events::NoesisRoutedEventsPlugin,
            classes::NoesisClassPlugin,
            markup::NoesisMarkupExtensionPlugin,
            visibility::NoesisVisibilityPlugin,
            layout::NoesisLayoutPlugin,
            text::NoesisTextPlugin,
            inlines::NoesisInlinesPlugin,
            geometry::NoesisGeometryPlugin,
        ));
        // Group B: interaction + data bridges. New bridges append here.
        app.add_plugins((
            focus::NoesisFocusPlugin,
            visual_state::NoesisVisualStatePlugin,
            focus_input::NoesisFocusControlPlugin,
            viewmodel::NoesisViewModelPlugin,
            commands::NoesisCommandsPlugin,
            items::NoesisItemsPlugin,
            dp::NoesisDpPlugin,
            (
                transforms::NoesisTransformPlugin,
                transforms3d::NoesisTransform3DPlugin,
            ),
            brushes::NoesisBrushesPlugin,
            animation::NoesisAnimationPlugin,
            typography::NoesisTypographyPlugin,
            binding::NoesisBindingPlugin,
            imaging::NoesisImagingPlugin,
            svg::NoesisSvgPlugin,
            diagnostics::NoesisDiagnosticsPlugin::default(),
        ));
        // Group C: past group B's 15-element `Plugins` limit.
        app.add_plugins((
            styles::NoesisStylesPlugin,
            shapes::NoesisShapesPlugin,
            resources::NoesisResourcesPlugin,
            panel::NoesisPanelPlugin,
            list::NoesisListPlugin,
        ));
    }
}

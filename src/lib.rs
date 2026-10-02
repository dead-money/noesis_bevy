//! Bevy plugin for the [Noesis GUI](https://www.noesisengine.com/) SDK.
//!
//! Noesis parses your XAML and owns the live controls, layout, styles and
//! animations. This crate drives it through the [`noesis_runtime`] FFI crate,
//! renders it on Bevy's wgpu device, and composites each UI onto a camera.
//! Building requires a licensed Noesis Native SDK at `NOESIS_SDK_DIR`.
//!
//! # Getting started
//!
//! Add [`NoesisPlugin`], register XAML in the [`XamlRegistry`] (or load `.xaml`
//! files through the asset server), and put a [`NoesisView`] on a camera:
//!
//! ```no_run
//! use std::sync::Arc;
//! use bevy::prelude::*;
//! use noesis_bevy::{NoesisPlugin, NoesisView, XamlRegistry};
//!
//! const MENU: &str = r#"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation">
//!   <TextBlock Text="Hello, Noesis!" Foreground="White"
//!              HorizontalAlignment="Center" VerticalAlignment="Center"/>
//! </Grid>"#;
//!
//! fn setup(mut commands: Commands, mut xaml: ResMut<XamlRegistry>) {
//!     xaml.insert("menu.xaml", Arc::new(MENU.as_bytes().to_vec()));
//!     commands.spawn((
//!         Camera2d,
//!         NoesisView {
//!             xaml_uri: "menu.xaml".to_string(),
//!             size: UVec2::new(1920, 1080),
//!             ..default()
//!         },
//!     ));
//! }
//!
//! App::new()
//!     .add_plugins((DefaultPlugins, NoesisPlugin::default()))
//!     .add_systems(Startup, setup)
//!     .run();
//! ```
//!
//! # Bridges
//!
//! Controls are not entities. You reach them through bridge components on the
//! [`NoesisView`] entity that name elements by `x:Name`: [`NoesisText`],
//! [`NoesisDp`] (any dependency property), [`NoesisVm`] and
//! [`NoesisViewModel`] (view models), [`NoesisCommands`], [`NoesisItems`],
//! [`NoesisVisibility`], [`NoesisLayout`] and more. Their systems run in
//! [`NoesisSet::Apply`] in `PostUpdate`, and read-backs arrive as messages
//! carrying the `view` entity.
//!
//! Most bridge maps are write-through: removing an entry stops further writes
//! but leaves the element at its last value. The order of bridges within
//! [`NoesisSet::Apply`] is unspecified.
//!
//! For entity-shaped UI, [`UiPanel`] mounts a XAML fragment bound to an
//! entity's components, and [`UiList`] turns entities into list rows.
//!
//! # Input
//!
//! Mouse, keyboard, touch and window focus are forwarded to the primary view
//! automatically (see [`input`]). Read [`NoesisPointerOverUi`] to keep UI
//! clicks out of the game world.
//!
//! # Threading
//!
//! Noesis is thread-affine. All Noesis calls run on the main thread in the
//! main world, in [`NoesisSet`] in `PostUpdate`; the render world only receives
//! each view's painted texture to composite.
#![warn(missing_docs)]

use bevy::prelude::*;

pub mod animation;
pub mod bake;
pub mod binding;
pub mod brushes;
pub mod classes;
pub mod clip;
pub mod commands;
pub mod diagnostics;
pub mod dp;
pub mod events;
pub mod focus;
pub mod focus_input;
pub mod font;
pub mod geometry;
pub mod headless;
#[cfg(feature = "hot_reload")]
pub mod hot_reload;
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
pub use clip::{NoesisClip, NoesisClipPlugin};
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
pub use headless::NoesisHeadlessPlugin;
#[cfg(feature = "hot_reload")]
pub use hot_reload::{NoesisHotReload, NoesisHotReloadPlugin};
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
pub use input::{
    NoesisInputEvent, NoesisInputPlugin, NoesisInputQueue, NoesisPointerOverUi, TargetedInput,
};
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
    ListRows, ListSort, ListedIn, NoesisListAppExt, NoesisListOps, NoesisListPlugin,
    NoesisListSelection, NoesisListSet, NoesisRowSelected, Selected, UiList,
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

/// Noesis license credentials. See [`NoesisPlugin::license`].
#[derive(Clone, Debug)]
pub struct NoesisLicense {
    /// Licensee name, as issued with the license.
    pub name: String,
    /// License key paired with [`name`](Self::name).
    pub key: String,
}

impl NoesisLicense {
    /// Reads `NOESIS_LICENSE_NAME` and `NOESIS_LICENSE_KEY` from the
    /// environment. Returns `None` if either is unset or not valid Unicode.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let name = std::env::var("NOESIS_LICENSE_NAME").ok()?;
        let key = std::env::var("NOESIS_LICENSE_KEY").ok()?;
        Some(Self { name, key })
    }
}

/// Initializes Noesis and adds the standard plugin set: asset loaders, input,
/// host integration, all bridges, and the render pipeline. Noesis shuts down
/// when the [`App`] is dropped.
///
/// Add it once. The individual `Noesis*Plugin`s it includes must not be added
/// again; Bevy panics on a duplicate plugin. Opt-in plugins such as
/// [`NoesisWindowCompatPlugin`], [`NoesisDefaultThemePlugin`] and
/// [`NoesisLabelBakerPlugin`] are not included.
#[derive(Default)]
pub struct NoesisPlugin {
    /// License to activate. `None` falls back to [`NoesisLicense::from_env`].
    /// Without a license, Noesis runs in trial mode and eventually blanks the
    /// view with a "Trial expired" message.
    pub license: Option<NoesisLicense>,
}

impl NoesisPlugin {
    /// Activates the license and starts the process-global runtime. Shared with
    /// [`NoesisHeadlessPlugin`]; must run before any device or provider is
    /// registered.
    pub(crate) fn init_runtime(&self) {
        if let Some(lic) = self.license.clone().or_else(NoesisLicense::from_env) {
            noesis_runtime::set_license(&lic.name, &lic.key);
        }
        noesis_runtime::init();

        info!("Noesis runtime version {}", noesis_runtime::version());
    }

    /// Adds the asset loader, input, integration and bridge plugins, without
    /// runtime init, hot reload or [`NoesisRenderPlugin`].
    ///
    /// For a custom harness with its own render wiring, as
    /// [`NoesisHeadlessPlugin`] does. Apps add [`NoesisPlugin`] instead.
    pub fn add_bridge_plugins(app: &mut App) {
        // Split into tuples: Bevy's `Plugins` impl stops at 15 elements.
        app.add_plugins((
            xaml::XamlAssetPlugin,
            font::FontAssetPlugin,
            image::ImageAssetPlugin,
            input::NoesisInputPlugin,
            integration::NoesisIntegrationPlugin,
        ));
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
            clip::NoesisClipPlugin,
        ));
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
        app.add_plugins((
            styles::NoesisStylesPlugin,
            shapes::NoesisShapesPlugin,
            resources::NoesisResourcesPlugin,
            panel::NoesisPanelPlugin,
            list::NoesisListPlugin,
        ));
    }
}

impl Plugin for NoesisPlugin {
    fn build(&self, app: &mut App) {
        self.init_runtime();

        // `NoesisRenderState::drop` calls `shutdown()` after releasing every handle;
        // a separate guard resource couldn't be ordered after it.

        Self::add_bridge_plugins(app);

        // Not in `add_bridge_plugins`, so headless tests don't start a file watcher.
        #[cfg(feature = "hot_reload")]
        app.add_plugins(hot_reload::NoesisHotReloadPlugin);

        app.add_plugins(render::NoesisRenderPlugin);
    }
}

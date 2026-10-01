//! Loads the control theme that ships with the Noesis SDK.
//!
//! Without a `ControlTemplate`, Noesis paints controls magenta. The SDK ships a
//! full theme (templates, brushes, fonts) under
//! `$NOESIS_SDK_DIR/Src/Packages/App/Theme/Data/Theme/` in color variants
//! (`DarkBlue`, `DarkEmerald`, `LightOrange`, ...). [`NoesisDefaultThemePlugin`]
//! reads one variant from there at startup, so `Button`, `TextBox`, and
//! `ScrollViewer` are styled without hand-written templates.
//!
//! The theme is SDK content that can't be embedded in this crate, so the plugin
//! is opt-in and separate from [`crate::NoesisPlugin`], which it requires. When
//! `NOESIS_SDK_DIR` is unset or the theme directory is missing, it warns and
//! controls render unstyled.
//!
//! ```ignore
//! app.add_plugins((NoesisPlugin::default(), NoesisDefaultThemePlugin::default()));
//! // or pick a variant:
//! app.add_plugins(NoesisDefaultThemePlugin { theme: "DarkEmerald".into() });
//! ```
//!
//! Each [`NoesisView`] gets the theme when it is spawned: the plugin puts the
//! theme dictionary first in
//! [`application_resources`](NoesisView::application_resources), adds the
//! theme fonts to the view's font waits, and appends the theme's font as a
//! fallback. [`NoesisResources`](crate::resources::NoesisResources) entries
//! override theme keys.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;

use crate::font::FontRegistry;
use crate::render::{NoesisSet, NoesisView};
use crate::xaml::XamlRegistry;

/// `Font.Family.Default` in `NoesisTheme.Fonts.xaml`, shared by every variant.
const THEME_FONT_FALLBACK: &str = "Fonts/#PT Root UI";

/// Loads a Noesis SDK control theme. Requires [`crate::NoesisPlugin`] and
/// `NOESIS_SDK_DIR`; see the [module docs](self).
pub struct NoesisDefaultThemePlugin {
    /// Variant name, such as `"DarkBlue"` (the default). Loads
    /// `NoesisTheme.{theme}.xaml` and its sibling dictionaries.
    pub theme: String,
}

impl Default for NoesisDefaultThemePlugin {
    fn default() -> Self {
        // The variant Noesis's own samples use.
        Self {
            theme: "DarkBlue".into(),
        }
    }
}

impl Plugin for NoesisDefaultThemePlugin {
    fn build(&self, app: &mut App) {
        let staged = stage_theme(&self.theme);
        if staged.xamls.is_empty() {
            warn!(
                "NoesisDefaultThemePlugin: no theme files staged for {:?} — \
                 controls will render unstyled (magenta). Check NOESIS_SDK_DIR.",
                self.theme
            );
        }
        app.insert_resource(staged)
            .add_systems(Startup, inject_theme_registries)
            // Before Sync: the resources bridge reads the patched chain there,
            // and scene build (Ensure) reads the font waits.
            .add_systems(PostUpdate, apply_theme_to_scene.before(NoesisSet::Sync));
    }
}

/// Theme files discovered on disk, ready to read into the registries.
#[derive(Resource, Default)]
struct StagedTheme {
    /// The requested variant name (without the `NoesisTheme.`/`.xaml` affixes).
    name: String,
    /// `(registry-uri, absolute-path)` for every theme XAML.
    xamls: Vec<(String, PathBuf)>,
    /// `(folder, filename, absolute-path)` for every theme font.
    fonts: Vec<(String, String, PathBuf)>,
}

/// Empty (with a warning) when the SDK or theme directory is missing.
fn stage_theme(theme: &str) -> StagedTheme {
    let mut staged = StagedTheme {
        name: theme.to_string(),
        ..default()
    };
    let Some(sdk) = std::env::var_os("NOESIS_SDK_DIR") else {
        warn!("NOESIS_SDK_DIR unset — cannot load Noesis default theme");
        return staged;
    };
    let root = PathBuf::from(sdk).join("Src/Packages/App/Theme/Data/Theme");
    if !root.is_dir() {
        warn!("Noesis theme dir {} not found", root.display());
        return staged;
    }

    // Bare filename: the theme's nested `<ResourceDictionary Source="..."/>`
    // references resolve against the same form.
    for entry in std::fs::read_dir(&root).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "xaml")
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            staged.xamls.push((name.to_string(), path.clone()));
        }
    }

    // Theme fonts go into the `Fonts/` folder so `FontFamily="Fonts/#PT Root UI"`
    // resolves alongside any scene fonts the consumer already loaded there.
    let fonts_dir = root.join("Fonts");
    for entry in std::fs::read_dir(&fonts_dir)
        .into_iter()
        .flatten()
        .flatten()
    {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "otf" || e == "ttf")
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            staged
                .fonts
                .push(("Fonts".to_string(), name.to_string(), path.clone()));
        }
    }

    let want = format!("NoesisTheme.{theme}.xaml");
    if !staged.xamls.iter().any(|(n, _)| n == &want) {
        warn!(
            "NoesisDefaultThemePlugin: {want} not found under {} — available \
             variants are the NoesisTheme.<Variant>.xaml files there",
            root.display()
        );
    }
    info!(
        "NoesisDefaultThemePlugin: staged {} theme XAML(s) + {} font(s) for {theme}",
        staged.xamls.len(),
        staged.fonts.len()
    );
    staged
}

/// Theme fonts are read straight from disk, not through the asset server, so
/// they are in `FontRegistry` before a view's `scan_folder("Fonts")`.
#[allow(clippy::needless_pass_by_value)]
fn inject_theme_registries(
    staged: Res<StagedTheme>,
    xaml: Option<ResMut<XamlRegistry>>,
    fonts: Option<ResMut<FontRegistry>>,
) {
    let (Some(mut xaml), Some(mut fonts)) = (xaml, fonts) else {
        warn!("NoesisDefaultThemePlugin requires NoesisPlugin (registries missing)");
        return;
    };
    for (name, path) in &staged.xamls {
        match std::fs::read(path) {
            Ok(bytes) => xaml.insert(name.clone(), Arc::new(bytes)),
            Err(err) => warn!("theme xaml read failed {}: {err}", path.display()),
        }
    }
    for (folder, filename, path) in &staged.fonts {
        match std::fs::read(path) {
            Ok(bytes) => fonts.insert(folder.clone(), filename.clone(), Arc::new(bytes)),
            Err(err) => warn!("theme font read failed {}: {err}", path.display()),
        }
    }
}

/// `Added<NoesisView>` rather than a one-shot `Local`: a view spawned later,
/// even earlier the same frame, is patched before its scene parses.
#[allow(clippy::needless_pass_by_value)]
fn apply_theme_to_scene(
    staged: Res<StagedTheme>,
    mut views: Query<&mut NoesisView, Added<NoesisView>>,
) {
    if staged.xamls.is_empty() {
        return;
    }

    let theme_uri = format!("NoesisTheme.{}.xaml", staged.name);
    for mut scene in &mut views {
        if !scene.application_resources.contains(&theme_uri) {
            // Theme first, so later chain entries can build on its styles.
            scene.application_resources.insert(0, theme_uri.clone());
        }
        if !scene.wait_for_fonts.iter().any(|f| f == "Fonts") {
            scene.wait_for_fonts.push("Fonts".to_string());
        }
        for (folder, filename, _) in &staged.fonts {
            let pair = (folder.clone(), filename.clone());
            if !scene.wait_for_font_files.contains(&pair) {
                scene.wait_for_font_files.push(pair);
            }
        }
        let fallback = THEME_FONT_FALLBACK.to_string();
        if !scene.font_fallbacks.contains(&fallback) {
            scene.font_fallbacks.push(fallback);
        }
    }
}

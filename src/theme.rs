//! Opt-in loader for Noesis's shipped control theme.
//!
//! Without a `ControlTemplate`, Noesis paints controls magenta ("no style").
//! The Native SDK ships a full theme — control templates, brushes, fonts — under
//! `$NOESIS_SDK_DIR/Src/Packages/App/Theme/Data/Theme/`, in color variants
//! (`DarkBlue`, `DarkEmerald`, `LightOrange`, …). This plugin stages one variant
//! into the XAML + font registries and installs it as the scene's application
//! resources, so a consumer gets styled `Button` / `TextBox` / `ScrollViewer`
//! without hand-authoring templates.
//!
//! It's deliberately **opt-in** (a separate plugin from [`crate::NoesisPlugin`]):
//! the theme is SDK content that can't be embedded, so it's only loaded when a
//! consumer asks and `NOESIS_SDK_DIR` is set.
//!
//! ```ignore
//! app.add_plugins((NoesisPlugin::default(), NoesisDefaultThemePlugin::default()));
//! // or pick a variant:
//! app.add_plugins(NoesisDefaultThemePlugin { theme: "DarkEmerald".into() });
//! ```
//!
//! Add it **after** [`crate::NoesisPlugin`] (it uses the registries that plugin
//! installs) and insert your [`NoesisScene`] as usual — the theme patches it in.

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;

use crate::font::FontRegistry;
use crate::render::NoesisScene;
use crate::xaml::XamlRegistry;

/// The theme's default font family. Every Noesis color variant shares it
/// (`NoesisTheme.Fonts.xaml`'s `Font.Family.Default`), so it's a safe fallback
/// for unstyled text once the theme fonts are staged.
const THEME_FONT_FALLBACK: &str = "Fonts/#PT Root UI";

/// Loads a shipped Noesis control theme from the SDK. See the module docs.
pub struct NoesisDefaultThemePlugin {
    /// Variant name, e.g. `"DarkBlue"`. Loads `NoesisTheme.{theme}.xaml` and
    /// its sibling dictionaries from the SDK theme directory.
    pub theme: String,
}

impl Default for NoesisDefaultThemePlugin {
    fn default() -> Self {
        // DarkBlue is the variant Noesis's own samples default to.
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
            .add_systems(Update, apply_theme_to_scene);
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

/// Discover the theme's XAML + font files under the SDK. Returns an empty set
/// (and logs) when the SDK or theme directory is missing, so the plugin
/// degrades to "unstyled" rather than panicking.
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

    // All *.xaml under their bare filename — the theme's nested
    // `<ResourceDictionary Source="..."/>` references use the same bare form.
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

/// Read the staged theme files into the XAML + font registries. Theme fonts are
/// read straight from the SDK (not through the asset server), so they land in
/// `FontRegistry` before the view's one-shot `scan_folder("Fonts")` fires.
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

/// Patch the consumer's [`NoesisScene`] to load the theme: install it as an
/// application resource and gate view creation on the theme fonts. Runs each
/// frame until the scene exists and is patched once.
#[allow(clippy::needless_pass_by_value)]
fn apply_theme_to_scene(
    staged: Res<StagedTheme>,
    scene: Option<ResMut<NoesisScene>>,
    mut applied: Local<bool>,
) {
    if *applied {
        return;
    }
    // Nothing staged → nothing to apply; stop retrying.
    if staged.xamls.is_empty() {
        *applied = true;
        return;
    }
    // Wait for the consumer to insert their scene.
    let Some(mut scene) = scene else {
        return;
    };

    let theme_uri = format!("NoesisTheme.{}.xaml", staged.name);
    if !scene.application_resources.contains(&theme_uri) {
        // Theme first, so any app-level resources can build on its styles.
        scene.application_resources.insert(0, theme_uri);
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

    *applied = true;
}

//! Font-asset plumbing for the Bevy plugin (Phase 4.F.2).
//!
//! Parallels [`crate::xaml`] for font files:
//!
//! - [`FontAsset`] / [`FontAssetLoader`] ingest `.ttf` / `.otf` / `.ttc`
//!   files into Bevy's asset system.
//! - [`FontRegistry`] indexes loaded fonts by `(folder_uri, filename)`.
//!   Noesis's `FontFamily="Fonts/#Bitter"` attribute decomposes into a
//!   folder URI (`"Fonts/"`) and a family name (`"Bitter"`) — the folder
//!   URI is what our scan-folder callback sees, and we need to report
//!   every filename we've loaded for that folder.
//! - `ExtractResource` mirrors the registry into the render world each
//!   frame.
//! - [`BevyFontProvider`] implements
//!   [`noesis_runtime::font_provider::FontProvider`] against a
//!   [`SharedFontMap`] that the plugin syncs from the registry.
//!
//! # How Noesis resolves `FontFamily="Fonts/#Bitter"`
//!
//! Noesis splits on `#`: the prefix (`"Fonts/"`) is the folder URI, the
//! suffix (`"Bitter"`) is the family name. Noesis calls our provider's
//! `ScanFolder("Fonts/")` once to learn which fonts exist; for each
//! filename we hand back, Noesis opens the file (via our `OpenFont`) and
//! scans its face metadata (family name, weight, stretch, style). After
//! that, Noesis's `MatchFont` picks the closest face for the requested
//! properties.
//!
//! We convert an asset path like `"Fonts/Bitter-Regular.ttf"` into
//! `(folder="Fonts/", filename="Bitter-Regular.ttf")` by splitting on the
//! last `/`. Paths without a folder component go into folder `""`
//! (matches Noesis's root-relative URI handling).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::asset::{AssetApp, AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;
use bevy_render::extract_resource::{ExtractResource, ExtractResourcePlugin};

use noesis_runtime::font_provider::FontProvider;

// ─────────────────────────────────────────────────────────────────────────────
// FontAsset + loader
// ─────────────────────────────────────────────────────────────────────────────

/// Raw font-file bytes. Noesis parses the font face metadata itself via
/// `FreeType`; we never inspect the bytes on the Rust side.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct FontAsset {
    pub bytes: Arc<Vec<u8>>,
}

/// Loads `.ttf` / `.otf` / `.ttc` font files into [`FontAsset`]. Reads the
/// whole file into memory — typical UI fonts are under a megabyte.
#[derive(Default, TypePath)]
pub struct FontAssetLoader;

impl AssetLoader for FontAssetLoader {
    type Asset = FontAsset;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<FontAsset, std::io::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(FontAsset {
            bytes: Arc::new(bytes),
        })
    }

    fn extensions(&self) -> &[&str] {
        &["ttf", "otf", "ttc"]
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FontRegistry — folder → filename → bytes
// ─────────────────────────────────────────────────────────────────────────────

/// Flat (`folder_uri`, `filename`) → bytes map, populated by
/// [`update_font_registry`] on `AssetEvent<FontAsset>`. Cloned into the
/// render world via [`ExtractResource`]; the `Arc<Vec<u8>>` values make
/// the clone cheap.
#[derive(Resource, ExtractResource, Default, Clone)]
pub struct FontRegistry {
    /// `(folder_uri, filename)` → bytes. Folder URIs are stored *with* a
    /// trailing slash (`"Fonts/"`), matching how Noesis hands them to us
    /// in `ScanFolder`.
    pub(crate) entries: HashMap<(String, String), Arc<Vec<u8>>>,
}

impl FontRegistry {
    /// Look up the bytes for a `(folder, filename)` pair.
    #[must_use]
    pub fn get(&self, folder_uri: &str, filename: &str) -> Option<&Arc<Vec<u8>>> {
        self.entries
            .get(&(folder_uri.to_string(), filename.to_string()))
    }

    /// Number of registered fonts.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no fonts have been registered yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over the `(folder, filename)` keys. Order is undefined.
    pub fn keys(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .keys()
            .map(|(folder, filename)| (folder.as_str(), filename.as_str()))
    }

    /// Register font bytes under a `(folder, filename)` key — bypasses the
    /// `AssetServer` flow. Callers are responsible for the folder /
    /// filename matching whatever `FontFamily="Folder/#Family"` the XAML
    /// references.
    pub fn insert(
        &mut self,
        folder_uri: impl Into<String>,
        filename: impl Into<String>,
        bytes: Arc<Vec<u8>>,
    ) {
        self.entries
            .insert((folder_uri.into(), filename.into()), bytes);
    }
}

/// Split an asset path like `"Fonts/Bitter-Regular.ttf"` into
/// `("Fonts", "Bitter-Regular.ttf")`. The folder is returned *without*
/// a trailing slash — that's the format Noesis's `CachedFontProvider`
/// hands to `ScanFolder` (it strips the slash when resolving URIs like
/// `FontFamily="Fonts/#Bitter"`). Paths with no folder return
/// `("", filename)`.
fn split_folder_filename(asset_path: &str) -> (String, String) {
    match asset_path.rsplit_once('/') {
        Some((folder, filename)) => (folder.to_string(), filename.to_string()),
        None => (String::new(), asset_path.to_string()),
    }
}

/// The final path segment of a folder URI (no trailing slash).
///
/// Noesis resolves a `FontFamily="Fonts/#Family"` folder *relative to the
/// referring XAML's base URI*: an explicit reference from `ui/root.xaml`
/// arrives here as `"ui/Fonts"`, while the registry is keyed by the bare
/// `"Fonts"` the fallback chain registers under. Matching on the final segment
/// lets a rooted family reference resolve regardless of where the document
/// lives — without it, every explicit `FontFamily` silently misses and falls
/// through to the fallback chain (the long-standing "explicit fonts don't
/// work, only fallbacks do" gotcha).
fn folder_basename(uri: &str) -> &str {
    uri.trim_end_matches('/').rsplit('/').next().unwrap_or(uri)
}

/// Main-app system that keeps [`FontRegistry`] in sync with the asset
/// system.
#[allow(clippy::needless_pass_by_value)]
pub fn update_font_registry(
    mut events: MessageReader<AssetEvent<FontAsset>>,
    assets: Res<Assets<FontAsset>>,
    asset_server: Res<AssetServer>,
    mut registry: ResMut<FontRegistry>,
) {
    for event in events.read() {
        match *event {
            AssetEvent::Added { id } | AssetEvent::Modified { id } => {
                let Some(path) = asset_server.get_path(id) else {
                    continue;
                };
                let Some(asset) = assets.get(id) else {
                    continue;
                };
                let (folder, filename) = split_folder_filename(&path.to_string());
                registry
                    .entries
                    .insert((folder, filename), Arc::clone(&asset.bytes));
            }
            AssetEvent::Removed { id } | AssetEvent::Unused { id } => {
                let Some(path) = asset_server.get_path(id) else {
                    continue;
                };
                let (folder, filename) = split_folder_filename(&path.to_string());
                registry.entries.remove(&(folder, filename));
            }
            AssetEvent::LoadedWithDependencies { .. } => {}
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// BevyFontProvider — the render-world FontProvider impl
// ─────────────────────────────────────────────────────────────────────────────

/// Shared `(folder, filename)` → bytes map. Render-world-only; the
/// provider's boxed impl holds one Arc handle, a [`NoesisRenderState`]
/// sibling holds another so a sync system can refresh the map from
/// [`FontRegistry`] each frame.
///
/// [`NoesisRenderState`]: crate::render::NoesisRenderState
type FontMapEntries = HashMap<(String, String), Arc<Vec<u8>>>;

#[derive(Clone, Default)]
pub struct SharedFontMap(pub(crate) Arc<Mutex<FontMapEntries>>);

impl SharedFontMap {
    /// Replace the map contents from an extracted [`FontRegistry`].
    ///
    /// # Panics
    ///
    /// Panics on mutex poisoning, which can only happen if another holder
    /// panicked mid-modification — a bug, not a runtime condition.
    pub fn sync_from(&self, registry: &FontRegistry) {
        let mut guard = self.0.lock().expect("SharedFontMap mutex poisoned");
        guard.clone_from(&registry.entries);
    }
}

/// Implements [`FontProvider`] against a [`SharedFontMap`].
///
/// `open_font` returns a borrow into `self.current`, rotated on each call
/// — same pattern as [`crate::xaml::BevyXamlProvider`].
pub struct BevyFontProvider {
    shared: SharedFontMap,
    current: Option<Arc<Vec<u8>>>,
}

impl BevyFontProvider {
    #[must_use]
    pub fn from_shared(map: SharedFontMap) -> Self {
        Self {
            shared: map,
            current: None,
        }
    }
}

impl FontProvider for BevyFontProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn scan_folder(&mut self, folder_uri: &str, register: &mut dyn FnMut(&str)) {
        let want = folder_basename(folder_uri);
        let guard = self.shared.0.lock().expect("SharedFontMap mutex poisoned");
        let matches: Vec<String> = guard
            .keys()
            .filter(|(folder, _)| folder_basename(folder) == want)
            .map(|(_, filename)| filename.clone())
            .collect();
        drop(guard);
        for filename in &matches {
            register(filename);
        }
    }

    fn open_font(&mut self, folder_uri: &str, filename: &str) -> Option<&[u8]> {
        let want = folder_basename(folder_uri);
        let arc = {
            let guard = self.shared.0.lock().ok()?;
            guard
                .iter()
                .find(|((folder, name), _)| folder_basename(folder) == want && name == filename)
                .map(|(_, bytes)| Arc::clone(bytes))?
        };
        self.current = Some(arc);
        self.current.as_deref().map(Vec::as_slice)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// FontAssetPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Registers [`FontAsset`] + [`FontAssetLoader`], initializes
/// [`FontRegistry`], and mirrors the registry into the render world.
/// Noesis-side font provider registration happens in `NoesisRenderPlugin`.
pub struct FontAssetPlugin;

impl Plugin for FontAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<FontAsset>()
            .init_asset_loader::<FontAssetLoader>()
            .init_resource::<FontRegistry>()
            .add_systems(Update, update_font_registry)
            .add_plugins(ExtractResourcePlugin::<FontRegistry>::default());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_folder_filename_with_subdir() {
        let (folder, filename) = split_folder_filename("Fonts/Bitter-Regular.ttf");
        assert_eq!(folder, "Fonts");
        assert_eq!(filename, "Bitter-Regular.ttf");
    }

    #[test]
    fn split_folder_filename_root() {
        let (folder, filename) = split_folder_filename("Roboto.ttf");
        assert_eq!(folder, "");
        assert_eq!(filename, "Roboto.ttf");
    }

    #[test]
    fn split_folder_filename_nested() {
        let (folder, filename) = split_folder_filename("Assets/Fonts/Deep/Regular.otf");
        assert_eq!(folder, "Assets/Fonts/Deep");
        assert_eq!(filename, "Regular.otf");
    }

    #[test]
    fn provider_scan_folder_lists_fonts() {
        let shared = SharedFontMap::default();
        {
            let mut guard = shared.0.lock().unwrap();
            guard.insert(
                ("Fonts".into(), "Bitter-Regular.ttf".into()),
                Arc::new(b"bitter".to_vec()),
            );
            guard.insert(
                ("Fonts".into(), "Roboto-Bold.ttf".into()),
                Arc::new(b"roboto".to_vec()),
            );
            guard.insert(
                ("Other".into(), "LCDMono.ttf".into()),
                Arc::new(b"lcd".to_vec()),
            );
        }
        let mut provider = BevyFontProvider::from_shared(shared);
        let mut registered = Vec::<String>::new();
        provider.scan_folder("Fonts", &mut |name| registered.push(name.to_string()));
        registered.sort();
        assert_eq!(registered, vec!["Bitter-Regular.ttf", "Roboto-Bold.ttf"]);
    }

    #[test]
    fn folder_basename_takes_last_segment() {
        assert_eq!(folder_basename("Fonts"), "Fonts");
        assert_eq!(folder_basename("ui/Fonts"), "Fonts");
        assert_eq!(folder_basename("a/b/Fonts/"), "Fonts");
        assert_eq!(folder_basename(""), "");
    }

    #[test]
    fn provider_resolves_relative_folder_to_registered_bare_folder() {
        // Noesis hands an explicit `FontFamily="Fonts/#Fam"` from `ui/x.xaml` to
        // the provider as folder `"ui/Fonts"`, but the registry is keyed `"Fonts"`.
        // Both scan_folder and open_font must match on the final path segment, or
        // every explicit reference silently misses and falls back.
        let shared = SharedFontMap::default();
        shared.0.lock().unwrap().insert(
            ("Fonts".into(), "DSEG7Classic-Bold.ttf".into()),
            Arc::new(b"DSEG".to_vec()),
        );
        let mut provider = BevyFontProvider::from_shared(shared);

        let mut registered = Vec::<String>::new();
        provider.scan_folder("ui/Fonts", &mut |name| registered.push(name.to_string()));
        assert_eq!(registered, vec!["DSEG7Classic-Bold.ttf"]);

        assert_eq!(
            provider.open_font("ui/Fonts", "DSEG7Classic-Bold.ttf"),
            Some(&b"DSEG"[..])
        );
    }

    #[test]
    fn provider_open_font_returns_bytes() {
        let shared = SharedFontMap::default();
        {
            let mut guard = shared.0.lock().unwrap();
            guard.insert(
                ("Fonts".into(), "Bitter-Regular.ttf".into()),
                Arc::new(b"BITTER_FONT_BYTES".to_vec()),
            );
        }
        let mut provider = BevyFontProvider::from_shared(shared);
        let bytes = provider.open_font("Fonts", "Bitter-Regular.ttf").unwrap();
        assert_eq!(bytes, b"BITTER_FONT_BYTES");
        assert!(provider.open_font("Fonts", "Missing.ttf").is_none());
    }
}

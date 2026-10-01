//! Font assets and the Noesis font provider.
//!
//! - [`FontAsset`] / [`FontAssetLoader`] load `.ttf` / `.otf` / `.ttc` files
//!   through Bevy's asset server.
//! - [`FontRegistry`] indexes loaded fonts by `(folder, filename)`.
//! - [`BevyFontProvider`] answers Noesis's font requests from a
//!   [`SharedFontMap`] that the plugin refreshes from the registry each frame.
//!
//! [`FontAssetPlugin`] (added by [`NoesisPlugin`](crate::NoesisPlugin)) keeps
//! the registry current. Load fonts with `asset_server.load("Fonts/Bitter-Regular.ttf")`
//! and keep the handle alive, or stage bytes directly with
//! [`FontRegistry::insert`].
//!
//! # How `FontFamily="Fonts/#Bitter"` resolves
//!
//! Noesis splits on `#`: `Fonts` is the folder, `Bitter` the family name. It
//! asks the provider which files the folder holds, opens each one, reads the
//! face metadata (family, weight, stretch, style), and picks the closest face.
//! An asset path such as `Fonts/Bitter-Regular.ttf` is registered as folder
//! `Fonts` (no trailing slash) and filename `Bitter-Regular.ttf`; a path with
//! no `/` goes in folder `""`.
//!
//! Noesis resolves the folder relative to the referring XAML, so
//! `FontFamily="Fonts/#Bitter"` in `ui/root.xaml` asks for `ui/Fonts`. When no
//! registered folder matches exactly, the provider falls back to matching the
//! last path segment, so `ui/Fonts` finds fonts registered under `Fonts`.
//!
//! Noesis scans each folder only once and caches the result, so the plugin
//! also registers every font with Noesis as it arrives. Fonts that finish
//! loading after the first scan still resolve.
//!
//! All of this runs on the main thread.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::asset::{AssetApp, AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;

use noesis_runtime::font_provider::FontProvider;

/// Raw font-file bytes. Noesis parses them; the Rust side never inspects them.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct FontAsset {
    /// The whole font file.
    pub bytes: Arc<Vec<u8>>,
}

/// Loads `.ttf` / `.otf` / `.ttc` files into [`FontAsset`] by reading the
/// whole file into memory.
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

/// Every font Noesis can see, keyed by `(folder, filename)`.
///
/// [`update_font_registry`] fills it from loaded [`FontAsset`]s and removes
/// entries when the asset is dropped. Folders are stored without a trailing
/// slash (`"Fonts"`, not `"Fonts/"`).
#[derive(Resource, Default, Clone)]
pub struct FontRegistry {
    // Folder keys never end in '/': `split_folder_filename` and `insert` both
    // strip it.
    pub(crate) entries: HashMap<(String, String), Arc<Vec<u8>>>,
}

impl FontRegistry {
    /// Looks up the bytes for a `(folder, filename)` pair. `folder` must not end
    /// in `/`.
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

    /// Registers font bytes without going through the asset server. A trailing
    /// `/` on `folder_uri` is stripped. `folder_uri` should match the folder
    /// part of the XAML's `FontFamily="Folder/#Family"`; the family name comes
    /// from the font file itself, not `filename`.
    pub fn insert(
        &mut self,
        folder_uri: impl Into<String>,
        filename: impl Into<String>,
        bytes: Arc<Vec<u8>>,
    ) {
        let mut folder = folder_uri.into();
        while folder.ends_with('/') {
            folder.pop();
        }
        self.entries.insert((folder, filename.into()), bytes);
    }
}

/// `"Fonts/Bitter-Regular.ttf"` to `("Fonts", "Bitter-Regular.ttf")`; `("", path)`
/// when there is no `/`. No trailing slash on the folder: Noesis's
/// `CachedFontProvider` strips it before calling `ScanFolder`.
fn split_folder_filename(asset_path: &str) -> (String, String) {
    match asset_path.rsplit_once('/') {
        Some((folder, filename)) => (folder.to_string(), filename.to_string()),
        None => (String::new(), asset_path.to_string()),
    }
}

/// Final path segment of a folder URI, ignoring a trailing slash.
///
/// Noesis resolves a `FontFamily` folder relative to the referring XAML, so
/// `Fonts/#Family` from `ui/root.xaml` arrives as `"ui/Fonts"` while the
/// registry holds `"Fonts"`. Without this fallback every explicit `FontFamily`
/// misses and only the fallback chain renders.
fn folder_basename(uri: &str) -> &str {
    uri.trim_end_matches('/').rsplit('/').next().unwrap_or(uri)
}

/// Keeps [`FontRegistry`] in sync with `AssetEvent<FontAsset>`. Runs in
/// `Update`.
#[allow(clippy::needless_pass_by_value)]
pub fn update_font_registry(
    mut events: MessageReader<AssetEvent<FontAsset>>,
    assets: Res<Assets<FontAsset>>,
    asset_server: Res<AssetServer>,
    mut registry: ResMut<FontRegistry>,
    // Removal events arrive after the path is gone (`get_path` is `None`), so
    // remember each id's key.
    mut keys: Local<HashMap<AssetId<FontAsset>, (String, String)>>,
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
                let key = split_folder_filename(&path.to_string());
                keys.insert(id, key.clone());
                registry.entries.insert(key, Arc::clone(&asset.bytes));
            }
            AssetEvent::Removed { id } | AssetEvent::Unused { id } => {
                let Some(key) = keys.remove(&id) else {
                    continue;
                };
                registry.entries.remove(&key);
            }
            AssetEvent::LoadedWithDependencies { .. } => {}
        }
    }
}

type FontMapEntries = HashMap<(String, String), Arc<Vec<u8>>>;

/// The font map [`BevyFontProvider`] reads. The plugin copies
/// [`FontRegistry`] into it every frame. Cloning shares the same map.
#[derive(Clone, Default)]
pub struct SharedFontMap(pub(crate) Arc<Mutex<FontMapEntries>>);

impl SharedFontMap {
    /// Replaces the map contents with the registry's.
    ///
    /// # Panics
    ///
    /// Panics on mutex poisoning, which can only happen if another holder
    /// panicked mid-modification: a bug, not a runtime condition.
    pub fn sync_from(&self, registry: &FontRegistry) {
        let mut guard = self.0.lock().expect("SharedFontMap mutex poisoned");
        guard.clone_from(&registry.entries);
    }
}

/// The [`FontProvider`] the plugin installs. Serves fonts from a
/// [`SharedFontMap`], matching folders exactly first and by last path segment
/// otherwise (see the [module docs](self)).
///
/// The slice `open_font` returns borrows the provider and stays valid until
/// the next `open_font` call.
pub struct BevyFontProvider {
    shared: SharedFontMap,
    current: Option<Arc<Vec<u8>>>,
}

impl BevyFontProvider {
    /// Creates a provider that serves fonts from `map`.
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
        let guard = self.shared.0.lock().expect("SharedFontMap mutex poisoned");
        let mut matches: Vec<String> = guard
            .keys()
            .filter(|(folder, _)| folder == folder_uri)
            .map(|(_, filename)| filename.clone())
            .collect();
        if matches.is_empty() {
            let want = folder_basename(folder_uri);
            let mut folders = std::collections::BTreeSet::new();
            matches = guard
                .keys()
                .filter(|(folder, _)| folder_basename(folder) == want)
                .map(|(folder, filename)| {
                    folders.insert(folder.as_str());
                    filename.clone()
                })
                .collect();
            if folders.len() > 1 {
                warn!(
                    "FontRegistry: folder \"{folder_uri}\" matches distinct registered folders \
                     {folders:?} by final path segment; scan results may be unstable",
                );
            }
        }
        drop(guard);
        for filename in &matches {
            register(filename);
        }
    }

    fn open_font(&mut self, folder_uri: &str, filename: &str) -> Option<&[u8]> {
        let arc = {
            let guard = self.shared.0.lock().expect("SharedFontMap mutex poisoned");
            if let Some(bytes) = guard.get(&(folder_uri.to_string(), filename.to_string())) {
                Arc::clone(bytes)
            } else {
                let want = folder_basename(folder_uri);
                let hits: Vec<&Arc<Vec<u8>>> = guard
                    .iter()
                    .filter(|((folder, name), _)| {
                        folder_basename(folder) == want && name == filename
                    })
                    .map(|(_, bytes)| bytes)
                    .collect();
                if hits.len() > 1 {
                    warn!(
                        "FontRegistry: font \"{filename}\" in folder \"{folder_uri}\" matches {} \
                         registered folders by final path segment; resolving arbitrarily",
                        hits.len(),
                    );
                }
                Arc::clone(hits.into_iter().next()?)
            }
        };
        self.current = Some(arc);
        self.current.as_deref().map(Vec::as_slice)
    }
}

/// Registers [`FontAsset`], its loader, and [`FontRegistry`], and keeps the
/// registry current. Added by [`NoesisPlugin`](crate::NoesisPlugin). The
/// provider itself is installed by
/// [`NoesisRenderPlugin`](crate::NoesisRenderPlugin).
pub struct FontAssetPlugin;

impl Plugin for FontAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<FontAsset>()
            .init_asset_loader::<FontAssetLoader>()
            .init_resource::<FontRegistry>()
            .add_systems(Update, update_font_registry);
    }
}

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
    fn insert_normalizes_trailing_slash() {
        let mut registry = FontRegistry::default();
        registry.insert("Fonts/", "Bitter-Regular.ttf", Arc::new(b"bitter".to_vec()));
        assert!(registry.get("Fonts", "Bitter-Regular.ttf").is_some());
        assert!(registry.get("Fonts/", "Bitter-Regular.ttf").is_none());
    }

    #[test]
    fn provider_prefers_exact_folder_over_basename_collision() {
        // "ui/Fonts" and "hud/Fonts" share the basename "Fonts"; an exact
        // request must resolve to its own folder, not whichever the HashMap
        // happens to iterate first.
        let shared = SharedFontMap::default();
        {
            let mut guard = shared.0.lock().unwrap();
            guard.insert(
                ("ui/Fonts".into(), "Panel.ttf".into()),
                Arc::new(b"ui".to_vec()),
            );
            guard.insert(
                ("hud/Fonts".into(), "Panel.ttf".into()),
                Arc::new(b"hud".to_vec()),
            );
        }
        let mut provider = BevyFontProvider::from_shared(shared);
        assert_eq!(
            provider.open_font("ui/Fonts", "Panel.ttf"),
            Some(&b"ui"[..])
        );
        assert_eq!(
            provider.open_font("hud/Fonts", "Panel.ttf"),
            Some(&b"hud"[..])
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

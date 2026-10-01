//! XAML assets and the provider Noesis loads them through.
//!
//! `.xaml` files load as [`XamlAsset`]s through Bevy's asset server.
//! [`update_xaml_registry`] mirrors them into [`XamlRegistry`], keyed by asset
//! path. Each frame the plugin copies the registry into the [`SharedXamlMap`]
//! behind [`BevyXamlProvider`], which answers Noesis's requests for XAML by URI
//! while a scene builds. Everything runs on the main thread, where Noesis lives.
//!
//! ```text
//!   AssetEvent<XamlAsset> ─▶ update_xaml_registry ─▶ XamlRegistry
//!                                                        │ sync_xaml_provider_map
//!                                                        ▼
//!                                  SharedXamlMap ─▶ BevyXamlProvider::load_xaml
//! ```
//!
//! To serve XAML from outside the asset system (a file outside `assets/`, or
//! generated markup), call [`XamlRegistry::insert`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::asset::{AssetApp, AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;

use noesis_runtime::xaml_provider::XamlProvider;

/// Raw XAML bytes from the asset server. Noesis parses them; Rust never does.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct XamlAsset {
    /// UTF-8 XAML markup. `Arc` so the per-frame registry sync copies handles,
    /// not bytes.
    pub bytes: Arc<Vec<u8>>,
}

/// Asset loader for `.xaml` files. Reads the whole file into one buffer.
#[derive(Default, TypePath)]
pub struct XamlAssetLoader;

impl AssetLoader for XamlAssetLoader {
    type Asset = XamlAsset;
    type Settings = ();
    type Error = std::io::Error;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<XamlAsset, std::io::Error> {
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok(XamlAsset {
            bytes: Arc::new(bytes),
        })
    }

    fn extensions(&self) -> &[&str] {
        &["xaml"]
    }
}

/// URI → XAML bytes for every XAML file Noesis can load. Keys are the asset
/// paths passed to `AssetServer::load` (the same URIs a view and `Source="…"`
/// references use). Filled by [`update_xaml_registry`] and by
/// [`insert`](Self::insert); synced to the provider each frame.
#[derive(Resource, Default, Clone)]
pub struct XamlRegistry {
    pub(crate) entries: HashMap<String, Arc<Vec<u8>>>,
}

impl XamlRegistry {
    /// Bytes registered for `uri`, if any.
    #[must_use]
    pub fn get(&self, uri: &str) -> Option<&Arc<Vec<u8>>> {
        self.entries.get(uri)
    }

    /// Number of registered XAML files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no XAML has been registered yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over registered URIs. Order is undefined.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Registers XAML bytes for `uri` without the asset server, for scenes
    /// outside `assets/` (the `xaml_viewer` example uses it). Replaces any
    /// existing entry. An asset event for the same path overwrites it later.
    pub fn insert(&mut self, uri: impl Into<String>, bytes: Arc<Vec<u8>>) {
        self.entries.insert(uri.into(), bytes);
    }
}

/// Keeps [`XamlRegistry`] in step with loaded, modified and unloaded
/// [`XamlAsset`]s. Runs in `Update`. Assets without a path (added directly to
/// `Assets<XamlAsset>`) are skipped, since Noesis looks XAML up by URI.
#[allow(clippy::needless_pass_by_value)] // Bevy systems take Res<T> by value
pub fn update_xaml_registry(
    mut events: MessageReader<AssetEvent<XamlAsset>>,
    assets: Res<Assets<XamlAsset>>,
    asset_server: Res<AssetServer>,
    mut registry: ResMut<XamlRegistry>,
    // `get_path` is `None` once the asset is dropped, so removals look the key up here.
    mut keys: Local<HashMap<AssetId<XamlAsset>, String>>,
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
                info!(
                    "update_xaml_registry: inserting {} ({} bytes)",
                    path,
                    asset.bytes.len(),
                );
                let key = path.to_string();
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

/// URI → bytes map shared between a [`BevyXamlProvider`] and the code that
/// updates it. Once the provider is handed to
/// [`noesis_runtime::xaml_provider::set_xaml_provider`], the returned
/// [`Registered`] guard owns it opaquely, so updates go through a clone of this
/// handle instead. The lock is only taken on the main thread and never
/// contended.
///
/// [`Registered`]: noesis_runtime::xaml_provider::Registered
#[derive(Clone, Default)]
pub struct SharedXamlMap(pub(crate) Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>);

impl SharedXamlMap {
    /// Replaces the map's contents with the registry's.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned (another holder panicked).
    pub fn sync_from(&self, registry: &XamlRegistry) {
        let mut guard = self.0.lock().expect("SharedXamlMap mutex poisoned");
        guard.clone_from(&registry.entries);
    }
}

/// Log of every URI (with the exact bytes served) that [`BevyXamlProvider`]
/// returned during a scene build: the root plus its transitive `Source="…"`
/// dependencies such as merged `ResourceDictionary`s.
///
/// The plugin calls [`begin`](Self::begin) before a build and
/// [`take`](Self::take) after it, then rebuilds the view when any logged
/// dependency's bytes change (compared by `Arc` identity). That is how an edit
/// to a shared dictionary hot-reloads every view that uses it. Main thread only.
#[derive(Clone, Default)]
pub struct SharedFetchLog(pub(crate) Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>);

impl SharedFetchLog {
    /// Clears the log so [`take`](Self::take) returns only what the next build
    /// fetches.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
    pub fn begin(&self) {
        self.0
            .lock()
            .expect("SharedFetchLog mutex poisoned")
            .clear();
    }

    /// Drains the URIs fetched since [`begin`](Self::begin), as `uri → served
    /// bytes`.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
    #[must_use]
    pub fn take(&self) -> HashMap<String, Arc<Vec<u8>>> {
        std::mem::take(&mut *self.0.lock().expect("SharedFetchLog mutex poisoned"))
    }
}

/// [`XamlProvider`] that serves XAML from a [`SharedXamlMap`], and logs each
/// fetch to a [`SharedFetchLog`]. [`NoesisPlugin`](crate::NoesisPlugin)
/// installs one; you only build your own to drive Noesis without the plugin
/// (tests, custom setups).
///
/// A missing URI returns `None`, which Noesis reports as a load failure. The
/// returned slice stays valid until the next `load_xaml` call, which covers
/// Noesis's synchronous parse; the map lock is not held during the parse.
pub struct BevyXamlProvider {
    shared: SharedXamlMap,
    log: SharedFetchLog,
    current: Option<Arc<Vec<u8>>>,
}

impl BevyXamlProvider {
    /// Creates a provider and a handle to its map. Give the provider to
    /// [`noesis_runtime::xaml_provider::set_xaml_provider`] and update the map
    /// through the handle, e.g. with [`SharedXamlMap::sync_from`].
    #[must_use]
    pub fn new_shared() -> (Self, SharedXamlMap) {
        let shared = SharedXamlMap::default();
        (Self::from_shared(shared.clone()), shared)
    }

    /// Creates a provider over an existing map, with a fetch log nobody reads.
    /// Use [`from_parts`](Self::from_parts) to track dependencies.
    #[must_use]
    pub fn from_shared(map: SharedXamlMap) -> Self {
        Self::from_parts(map, SharedFetchLog::default())
    }

    /// Creates a provider over an existing map and fetch log, keeping clones of
    /// both so you can update the map and read back each build's dependencies.
    #[must_use]
    pub fn from_parts(map: SharedXamlMap, log: SharedFetchLog) -> Self {
        Self {
            shared: map,
            log,
            current: None,
        }
    }
}

impl XamlProvider for BevyXamlProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn load_xaml(&mut self, uri: &str) -> Option<&[u8]> {
        let arc = {
            let guard = self.shared.0.lock().expect("SharedXamlMap mutex poisoned");
            guard.get(uri).cloned()?
        };
        self.log
            .0
            .lock()
            .expect("SharedFetchLog mutex poisoned")
            .insert(uri.to_string(), Arc::clone(&arc));
        self.current = Some(arc);
        self.current.as_deref().map(Vec::as_slice)
    }
}

/// Registers [`XamlAsset`] and its loader, and keeps [`XamlRegistry`] current.
/// Added by [`NoesisPlugin`](crate::NoesisPlugin). It doesn't touch Noesis; the
/// provider is installed by [`NoesisRenderPlugin`](crate::NoesisRenderPlugin).
pub struct XamlAssetPlugin;

impl Plugin for XamlAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<XamlAsset>()
            .init_asset_loader::<XamlAssetLoader>()
            .init_resource::<XamlRegistry>()
            .add_systems(Update, update_xaml_registry);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_returns_bytes_then_forgets_on_next_load() {
        let (mut provider, shared) = BevyXamlProvider::new_shared();
        let bytes_a = Arc::new(b"<Grid Background=\"Red\"/>".to_vec());
        let bytes_b = Arc::new(b"<Grid Background=\"Blue\"/>".to_vec());
        let mut registry = XamlRegistry::default();
        registry
            .entries
            .insert("a.xaml".into(), Arc::clone(&bytes_a));
        registry
            .entries
            .insert("b.xaml".into(), Arc::clone(&bytes_b));
        shared.sync_from(&registry);

        let slice_a = provider.load_xaml("a.xaml").expect("a.xaml missing");
        assert_eq!(slice_a, bytes_a.as_slice());

        // Noesis contract: the slice must live until the parse returns, i.e.
        // until the next load_xaml call, which rotates `current` to the new Arc.
        let slice_b = provider.load_xaml("b.xaml").expect("b.xaml missing");
        assert_eq!(slice_b, bytes_b.as_slice());

        assert!(provider.load_xaml("missing.xaml").is_none());
    }

    #[test]
    fn provider_sees_registry_changes_after_sync() {
        let (mut provider, shared) = BevyXamlProvider::new_shared();
        let mut registry = XamlRegistry::default();
        registry
            .entries
            .insert("a.xaml".into(), Arc::new(b"v1".to_vec()));
        shared.sync_from(&registry);
        assert_eq!(provider.load_xaml("a.xaml"), Some(b"v1".as_slice()));

        registry
            .entries
            .insert("a.xaml".into(), Arc::new(b"v2".to_vec()));
        shared.sync_from(&registry);
        assert_eq!(provider.load_xaml("a.xaml"), Some(b"v2".as_slice()));

        registry.entries.remove("a.xaml");
        shared.sync_from(&registry);
        assert_eq!(provider.load_xaml("a.xaml"), None);
    }
}

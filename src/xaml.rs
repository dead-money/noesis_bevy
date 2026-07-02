//! XAML asset plumbing: feeds Bevy-loaded XAML bytes to Noesis's parser.
//!
//! Noesis fetches XAML bytes by URI through a
//! [`noesis_runtime::xaml_provider::XamlProvider`]; Bevy's asset system is the
//! natural source. [`update_xaml_registry`] mirrors loaded [`XamlAsset`] bytes
//! into the [`XamlRegistry`] resource; the [`crate::render`] driving pipeline
//! syncs those into the [`SharedXamlMap`] backing a [`BevyXamlProvider`], which
//! answers Noesis's `load_xaml` callback while a scene builds. Registry, sync,
//! and callback all run in the main world, on the one thread Noesis is pinned
//! to, so no lock ever crosses a world boundary.
//!
//! Data flow:
//!
//! ```text
//!   AssetEvent<XamlAsset> ─▶ update_xaml_registry ─▶ XamlRegistry
//!                                                        │ sync_xaml_provider_map
//!                                                        ▼
//!                                  SharedXamlMap ─▶ BevyXamlProvider::load_xaml
//! ```

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::asset::{AssetApp, AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;

use noesis_runtime::xaml_provider::XamlProvider;

// ─────────────────────────────────────────────────────────────────────────────
// XamlAsset + loader
// ─────────────────────────────────────────────────────────────────────────────

/// Raw XAML bytes loaded from the asset system. Noesis parses the bytes
/// directly; we never interpret them on the Rust side.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct XamlAsset {
    /// UTF-8 XAML markup. Wrapped in an `Arc` so [`XamlRegistry`] can share
    /// handles across the world boundary without bulk-copying every frame.
    pub bytes: Arc<Vec<u8>>,
}

/// Loader for the `.xaml` extension. Reads the whole file into memory:
/// XAML files are small (kilobytes) and Noesis wants a contiguous slice.
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

// ─────────────────────────────────────────────────────────────────────────────
// XamlRegistry: URI → bytes map
// ─────────────────────────────────────────────────────────────────────────────

/// Maps the URIs Noesis asks for (typically the asset path used with
/// `AssetServer::load`) to the currently-loaded XAML bytes. Populated from
/// [`AssetEvent<XamlAsset>`] by [`update_xaml_registry`] and synced into the
/// provider's [`SharedXamlMap`] each frame. Values are `Arc<Vec<u8>>` so the
/// sync is a cheap handle copy regardless of XAML size.
#[derive(Resource, Default, Clone)]
pub struct XamlRegistry {
    pub(crate) entries: HashMap<String, Arc<Vec<u8>>>,
}

impl XamlRegistry {
    /// Look up bytes for `uri`. Shared between main-side tests and the
    /// render-side provider.
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

    /// Register XAML bytes for a URI directly, bypassing the
    /// `AssetServer`-driven path. Useful when loading scenes from
    /// arbitrary filesystem locations (the `xaml_viewer` example uses
    /// this to point at `$NOESIS_SDK_DIR/Data/` or a standalone file).
    pub fn insert(&mut self, uri: impl Into<String>, bytes: Arc<Vec<u8>>) {
        self.entries.insert(uri.into(), bytes);
    }
}

/// Main-app system that keeps [`XamlRegistry`] in sync with the asset
/// system. Reads `AssetEvent<XamlAsset>` and updates the map whenever a
/// XAML asset loads, changes, or unloads.
///
/// Uses `AssetServer::get_path` to recover the canonical URI. Assets loaded
/// without a path (e.g. `add_asset` directly) are skipped: Noesis needs a
/// URI to look them up.
#[allow(clippy::needless_pass_by_value)] // Bevy systems take Res<T> by value
pub fn update_xaml_registry(
    mut events: MessageReader<AssetEvent<XamlAsset>>,
    assets: Res<Assets<XamlAsset>>,
    asset_server: Res<AssetServer>,
    mut registry: ResMut<XamlRegistry>,
    // `AssetId` → registry key, so removal arms can find the entry after the
    // asset (and its path) are already gone: `get_path` returns `None` for a
    // dropped asset, so keying off the live path here would leave stale
    // entries and leaked byte buffers behind.
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

// ─────────────────────────────────────────────────────────────────────────────
// BevyXamlProvider: the XamlProvider impl
// ─────────────────────────────────────────────────────────────────────────────

/// Shared URI → bytes map. Once a provider is handed to Noesis via
/// [`noesis_runtime::xaml_provider::set_xaml_provider`], the [`Registered`] guard
/// owns the boxed provider opaquely; we can't mutate its state through the
/// guard. The provider holds a clone of this `Arc`; a separate clone lives in
/// `NoesisRenderState` so the sync system can update the map each frame from
/// the [`XamlRegistry`].
///
/// The `Mutex` is **only** touched from the main thread (the sync system and
/// the provider callback fired while a scene builds), which run sequentially,
/// so contention is always zero and no lock crosses a world boundary.
///
/// [`Registered`]: noesis_runtime::xaml_provider::Registered
#[derive(Clone, Default)]
pub struct SharedXamlMap(pub(crate) Arc<Mutex<HashMap<String, Arc<Vec<u8>>>>>);

impl SharedXamlMap {
    /// Replace the map contents from the [`XamlRegistry`].
    ///
    /// # Panics
    ///
    /// Panics if the internal mutex is poisoned, only possible if another
    /// holder panicked mid-modification, which in our architecture means
    /// programmer error (schedule violation) rather than recoverable
    /// runtime state.
    pub fn sync_from(&self, registry: &XamlRegistry) {
        let mut guard = self.0.lock().expect("SharedXamlMap mutex poisoned");
        guard.clone_from(&registry.entries);
    }
}

/// Implements [`noesis_runtime::xaml_provider::XamlProvider`] against a
/// [`SharedXamlMap`] that the plugin updates each frame from the
/// [`XamlRegistry`].
///
/// `load_xaml` clones the `Arc<Vec<u8>>` for the requested URI out of the
/// shared map into `self.current`, releases the lock, and returns a borrow
/// into `self.current`. The borrow stays valid until the *next* call
/// rotates `self.current`, which covers Noesis's synchronous-parse
/// contract and keeps the lock untouched during the parse itself.
pub struct BevyXamlProvider {
    shared: SharedXamlMap,
    current: Option<Arc<Vec<u8>>>,
}

impl BevyXamlProvider {
    /// Build a provider + a cloneable handle to its shared map. Give the
    /// provider to [`noesis_runtime::xaml_provider::set_xaml_provider`]; keep
    /// the `SharedXamlMap` handle so the plugin can sync it from
    /// [`XamlRegistry`].
    #[must_use]
    pub fn new_shared() -> (Self, SharedXamlMap) {
        let shared = SharedXamlMap::default();
        (Self::from_shared(shared.clone()), shared)
    }

    /// Build a provider that shares `map` with an existing handle. Used by
    /// the Bevy plugin so one `SharedXamlMap` lives both in `NoesisRenderState`
    /// (for the sync system) and inside the boxed provider (for the
    /// [`XamlProvider::load_xaml`] callback).
    #[must_use]
    pub fn from_shared(map: SharedXamlMap) -> Self {
        Self {
            shared: map,
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
        self.current = Some(arc);
        self.current.as_deref().map(Vec::as_slice)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// XamlAssetPlugin: wires the asset loader + registry
// ─────────────────────────────────────────────────────────────────────────────

/// Plugin that registers [`XamlAsset`] + [`XamlAssetLoader`], initializes
/// [`XamlRegistry`], and keeps it current from asset events.
///
/// Does *not* touch Noesis; the provider registration happens in
/// `NoesisRenderPlugin`.
pub struct XamlAssetPlugin;

impl Plugin for XamlAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<XamlAsset>()
            .init_asset_loader::<XamlAssetLoader>()
            .init_resource::<XamlRegistry>()
            .add_systems(Update, update_xaml_registry);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

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

        // Registry updates in place (hot-reload on the main side); a fresh
        // sync propagates.
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

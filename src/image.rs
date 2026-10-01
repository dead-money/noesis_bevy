//! Image-asset plumbing for the `ImageBrush` / `Image` loader.
//!
//! Parallels [`crate::font`]:
//!
//! - [`ImageAsset`] / [`ImageAssetLoader`] ingest `.png` / `.jpg` / `.jpeg`
//!   files, decoding to tightly-packed RGBA8 on load.
//! - [`ImageRegistry`] indexes loaded images by their asset-path URI so
//!   `<Image Source="Images/BgTile.png"/>` and
//!   `<ImageBrush ImageSource="Images/BgTile.png"/>` both resolve by the
//!   same key Noesis hands us.
//! - [`BevyTextureProvider`] implements
//!   [`noesis_runtime::texture_provider::TextureProvider`] against a
//!   [`SharedImageMap`] kept fresh by a sync system in
//!   [`crate::render::NoesisRenderPlugin`]'s main-world driving pipeline.
//!
//! Noesis decides whether to ask us via `GetTextureInfo` (layout-size
//! only) or `LoadTexture` (decoded pixels); we answer both from the same
//! map.
//!
//! # Premultiplied alpha
//!
//! Noesis configures `DeviceCaps::linearRendering = false` and its blend
//! state is `(BlendFactor::One, BlendFactor::OneMinusSrcAlpha)` for
//! `BlendMode::SrcOver`, the canonical premultiplied-alpha blend.
//! Feeding straight-alpha bytes to that blend state produces noticeable
//! edge fringing where opacity is partial.
//!
//! Premultiplication therefore happens at decode time inside
//! [`ImageAssetLoader::load`]. Every byte stored in [`ImageAsset::bytes`]
//! and (transitively) [`ImageRegistry`] / [`SharedImageMap`] is PMA by
//! contract. Non-PMA consumers must build their own loader against a
//! different asset type.
//!
//! Idempotency is guaranteed because the loader sees only the source PNG
//! bytes (raw / un-premultiplied per the file format); double-PMA is
//! impossible. Hot reload re-decodes from source on every change.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::asset::{AssetApp, AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;

use noesis_runtime::texture_provider::{ImageData, TextureInfo, TextureProvider};

// ─────────────────────────────────────────────────────────────────────────────
// ImageAsset + loader
// ─────────────────────────────────────────────────────────────────────────────

/// Decoded image as tightly-packed RGBA8 bytes.
///
/// `bytes.len() == width * height * 4`. `Arc<Vec<u8>>` so the registry
/// and the provider's shared map can share allocations without
/// copying on every sync.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct ImageAsset {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Tightly-packed RGBA8 pixels, premultiplied alpha (see module docs).
    pub bytes: Arc<Vec<u8>>,
}

/// Errors from [`ImageAssetLoader`]. Decode failures fold into one
/// variant because the `image` crate's errors aren't useful to expose
/// individually; the URI in the log line tells you which file broke.
#[derive(thiserror::Error, Debug)]
pub enum ImageLoadError {
    /// Reading the encoded bytes off the asset reader failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// The `image` crate could not decode the file; the string is its
    /// formatted error.
    #[error("decode: {0}")]
    Decode(String),
}

/// Decodes `.png` / `.jpg` / `.jpeg` files via the `image` crate into
/// RGBA8. The decoded buffer stays resident: Noesis needs the pixels hot
/// for its on-demand `LoadTexture` calls anyway.
#[derive(Default, TypePath)]
pub struct ImageAssetLoader;

impl AssetLoader for ImageAssetLoader {
    type Asset = ImageAsset;
    type Settings = ();
    type Error = ImageLoadError;

    async fn load(
        &self,
        reader: &mut dyn Reader,
        _settings: &(),
        _load_context: &mut LoadContext<'_>,
    ) -> Result<ImageAsset, ImageLoadError> {
        let mut encoded = Vec::new();
        reader.read_to_end(&mut encoded).await?;
        let decoded = image::load_from_memory(&encoded)
            .map_err(|e| ImageLoadError::Decode(e.to_string()))?
            .to_rgba8();
        let (width, height) = decoded.dimensions();
        let mut raw = decoded.into_raw();
        premultiply_alpha(&mut raw);
        let bytes = Arc::new(raw);
        Ok(ImageAsset {
            width,
            height,
            bytes,
        })
    }

    fn extensions(&self) -> &[&str] {
        &["png", "jpg", "jpeg"]
    }
}

/// Premultiply RGBA8 bytes in place: `(R, G, B, A) -> (R*A/255, G*A/255, B*A/255, A)`.
/// Required so [`ImageAsset::bytes`] satisfy Noesis's PMA blend assumption
/// (see module docs). `bytes.len()` must be a multiple of 4.
///
/// Uses 8-bit rounding (`+ 127) / 255`), matching what Photoshop / GIMP /
/// Unity's `NoesisGUIPackage` importer write for cooked sprites. Pure-zero
/// alpha pixels collapse to fully-transparent black, which is the
/// conventional PMA representation (avoids stale colour bleeding through
/// `SrcOver` edges).
#[inline]
fn premultiply_alpha(bytes: &mut [u8]) {
    debug_assert_eq!(
        bytes.len() % 4,
        0,
        "premultiply_alpha expects RGBA8: len = {}",
        bytes.len()
    );
    for chunk in bytes.chunks_exact_mut(4) {
        let a = u32::from(chunk[3]);
        if a == 255 {
            continue;
        }
        if a == 0 {
            chunk[0] = 0;
            chunk[1] = 0;
            chunk[2] = 0;
            continue;
        }
        // Rounded fixed-point: (c * a + 127) / 255. Within ±1 of the
        // floating-point reference; lossless at a == 0 and a == 255.
        chunk[0] = ((u32::from(chunk[0]) * a + 127) / 255) as u8;
        chunk[1] = ((u32::from(chunk[1]) * a + 127) / 255) as u8;
        chunk[2] = ((u32::from(chunk[2]) * a + 127) / 255) as u8;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ImageRegistry: uri -> decoded image
// ─────────────────────────────────────────────────────────────────────────────

/// Flat `uri` → decoded image map. Populated by
/// [`update_image_registry`] on `AssetEvent<ImageAsset>`. Synced into the
/// provider's [`SharedImageMap`] each frame; the `Arc` values make the sync
/// a cheap handle copy.
///
/// Keys are asset paths as strings (e.g. `"Images/BgTile.png"`),
/// matching the `ImageSource` attribute Noesis hands us verbatim.
#[derive(Resource, Default, Clone)]
pub struct ImageRegistry {
    pub(crate) entries: HashMap<String, RegisteredImage>,
}

#[derive(Clone)]
pub(crate) struct RegisteredImage {
    pub width: u32,
    pub height: u32,
    pub bytes: Arc<Vec<u8>>,
}

impl ImageRegistry {
    /// Look up a registered image.
    #[must_use]
    pub fn get(&self, uri: &str) -> Option<(u32, u32, &Arc<Vec<u8>>)> {
        self.entries
            .get(uri)
            .map(|img| (img.width, img.height, &img.bytes))
    }

    /// Number of registered images.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// `true` when no images have been registered yet. Useful for waiting
    /// on async asset loads from a downstream plugin.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over the registered URIs. Order is undefined.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Insert a pre-decoded image under `uri`. Handy for staging images
    /// that didn't come through Bevy's asset server (e.g. a theme
    /// loader synthesising textures).
    pub fn insert(&mut self, uri: impl Into<String>, width: u32, height: u32, bytes: Arc<Vec<u8>>) {
        self.entries.insert(
            uri.into(),
            RegisteredImage {
                width,
                height,
                bytes,
            },
        );
    }

    /// Drop the image staged under `uri`, reclaiming its buffer. Used by the
    /// imaging bridge's component-removal reap to reclaim a bitmap no live
    /// [`crate::imaging::NoesisImaging`] references any longer.
    pub(crate) fn remove(&mut self, uri: &str) {
        self.entries.remove(uri);
    }
}

/// Main-app system that keeps [`ImageRegistry`] in sync with the asset
/// system.
#[allow(clippy::needless_pass_by_value)]
pub fn update_image_registry(
    mut events: MessageReader<AssetEvent<ImageAsset>>,
    assets: Res<Assets<ImageAsset>>,
    asset_server: Res<AssetServer>,
    mut registry: ResMut<ImageRegistry>,
    // `AssetId` → registry key, so removal arms can find the entry after the
    // asset (and its path) are already gone: `get_path` returns `None` for a
    // dropped asset, so keying off the live path here would leave stale
    // entries and leaked byte buffers behind.
    mut keys: Local<HashMap<AssetId<ImageAsset>, String>>,
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
                let key = path.to_string();
                keys.insert(id, key.clone());
                registry.entries.insert(
                    key,
                    RegisteredImage {
                        width: asset.width,
                        height: asset.height,
                        bytes: Arc::clone(&asset.bytes),
                    },
                );
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
// BevyTextureProvider: the TextureProvider impl
// ─────────────────────────────────────────────────────────────────────────────

/// Shared `uri` → image map. The provider's boxed impl holds one Arc handle,
/// `NoesisRenderState` holds another so the sync system can refresh the map
/// from [`ImageRegistry`] each frame.
type ImageMapEntries = HashMap<String, RegisteredImage>;

/// Shared `uri` → image map behind an `Arc<Mutex<…>>`.
///
/// One handle lives inside the boxed [`BevyTextureProvider`], another in
/// `NoesisRenderState`, whose sync system calls [`SharedImageMap::sync_from`]
/// each frame to push the latest [`ImageRegistry`] into the map. Both run on
/// the main thread.
#[derive(Clone, Default)]
pub struct SharedImageMap(pub(crate) Arc<Mutex<ImageMapEntries>>);

impl SharedImageMap {
    /// Replace the map contents from the [`ImageRegistry`].
    ///
    /// # Panics
    ///
    /// Panics on mutex poisoning (a bug, not a runtime condition).
    pub fn sync_from(&self, registry: &ImageRegistry) {
        let mut guard = self.0.lock().expect("SharedImageMap mutex poisoned");
        guard.clone_from(&registry.entries);
    }
}

/// Implements [`TextureProvider`] against a [`SharedImageMap`].
///
/// `load` returns a borrow into `self.current`, which is rotated on
/// each call, like [`crate::xaml::BevyXamlProvider`] and
/// [`crate::font::BevyFontProvider`].
pub struct BevyTextureProvider {
    shared: SharedImageMap,
    current: Option<Arc<Vec<u8>>>,
    current_dims: (u32, u32),
}

impl BevyTextureProvider {
    /// Build a provider that resolves textures through the given
    /// [`SharedImageMap`]. The render plugin boxes this and hands it to
    /// Noesis as the active [`TextureProvider`].
    #[must_use]
    pub fn from_shared(map: SharedImageMap) -> Self {
        Self {
            shared: map,
            current: None,
            current_dims: (0, 0),
        }
    }
}

impl TextureProvider for BevyTextureProvider {
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn info(&mut self, uri: &str) -> Option<TextureInfo> {
        let guard = self.shared.0.lock().expect("SharedImageMap mutex poisoned");
        let img = guard.get(uri)?;
        Some(TextureInfo::new(img.width, img.height))
    }

    fn load(&mut self, uri: &str) -> Option<ImageData<'_>> {
        let (arc, w, h) = {
            let guard = self.shared.0.lock().expect("SharedImageMap mutex poisoned");
            let img = guard.get(uri)?;
            (Arc::clone(&img.bytes), img.width, img.height)
        };
        self.current = Some(arc);
        self.current_dims = (w, h);
        self.current.as_deref().map(|bytes| ImageData {
            width: w,
            height: h,
            bytes,
        })
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ImageAssetPlugin
// ─────────────────────────────────────────────────────────────────────────────

/// Registers [`ImageAsset`] + [`ImageAssetLoader`], initializes
/// [`ImageRegistry`], and keeps it current from asset events. Noesis-side
/// texture-provider registration happens in
/// [`crate::render::NoesisRenderPlugin`].
pub struct ImageAssetPlugin;

impl Plugin for ImageAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<ImageAsset>()
            .init_asset_loader::<ImageAssetLoader>()
            .init_resource::<ImageRegistry>()
            .add_systems(Update, update_image_registry);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_image(w: u32, h: u32, fill: [u8; 4]) -> Arc<Vec<u8>> {
        let mut bytes = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..(w * h) {
            bytes.extend_from_slice(&fill);
        }
        Arc::new(bytes)
    }

    #[test]
    fn provider_info_returns_dimensions() {
        let shared = SharedImageMap::default();
        {
            let mut guard = shared.0.lock().unwrap();
            guard.insert(
                "Images/BgTile.png".into(),
                RegisteredImage {
                    width: 6,
                    height: 6,
                    bytes: make_image(6, 6, [10, 20, 30, 255]),
                },
            );
        }
        let mut provider = BevyTextureProvider::from_shared(shared);
        let info = provider.info("Images/BgTile.png").unwrap();
        assert_eq!(info.width, 6);
        assert_eq!(info.height, 6);
        assert!(provider.info("Images/Missing.png").is_none());
    }

    #[test]
    fn premultiply_alpha_zero_collapses_to_transparent_black() {
        let mut bytes = vec![200, 150, 100, 0];
        premultiply_alpha(&mut bytes);
        assert_eq!(bytes, vec![0, 0, 0, 0]);
    }

    #[test]
    fn premultiply_alpha_full_preserves_bytes() {
        let mut bytes = vec![10, 20, 30, 255, 200, 150, 100, 255];
        premultiply_alpha(&mut bytes);
        assert_eq!(bytes, vec![10, 20, 30, 255, 200, 150, 100, 255]);
    }

    #[test]
    fn premultiply_alpha_half_alpha_halves_rgb() {
        // a = 128, c = 200 → (200 * 128 + 127) / 255 = 25727 / 255 = 100
        let mut bytes = vec![200, 200, 200, 128];
        premultiply_alpha(&mut bytes);
        assert_eq!(bytes, vec![100, 100, 100, 128]);
    }

    #[test]
    fn premultiply_alpha_arbitrary_alpha_matches_rounded_formula() {
        // Verify the rounded fixed-point formula on a heterogeneous batch.
        let mut bytes = vec![
            255, 128, 0, 64, // 25 % alpha
            100, 100, 100, 200, // 78 % alpha
            255, 255, 255, 1, // near-zero alpha
        ];
        premultiply_alpha(&mut bytes);
        // (255 * 64  + 127) / 255 = 16447 / 255 = 64
        // (128 * 64  + 127) / 255 =  8319 / 255 = 32
        //   (0 * 64  + 127) / 255 =   127 / 255 = 0
        // (100 * 200 + 127) / 255 = 20127 / 255 = 78
        // (255 *   1 + 127) / 255 =   382 / 255 = 1
        assert_eq!(bytes, vec![64, 32, 0, 64, 78, 78, 78, 200, 1, 1, 1, 1]);
    }

    #[test]
    fn provider_load_returns_bytes_with_expected_layout() {
        let shared = SharedImageMap::default();
        {
            let mut guard = shared.0.lock().unwrap();
            guard.insert(
                "Images/A.png".into(),
                RegisteredImage {
                    width: 2,
                    height: 2,
                    bytes: make_image(2, 2, [1, 2, 3, 4]),
                },
            );
        }
        let mut provider = BevyTextureProvider::from_shared(shared);
        let img = provider.load("Images/A.png").unwrap();
        assert_eq!(img.width, 2);
        assert_eq!(img.height, 2);
        assert_eq!(img.bytes.len(), 16);
        assert_eq!(&img.bytes[..4], &[1, 2, 3, 4]);
        assert!(provider.load("Images/Missing.png").is_none());
    }
}

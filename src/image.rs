//! Image assets and the Noesis texture provider, which back `<Image Source>`
//! and `<ImageBrush ImageSource>`.
//!
//! - [`ImageAsset`] / [`ImageAssetLoader`] decode `.png` / `.jpg` / `.jpeg`
//!   files to tightly-packed RGBA8 with premultiplied alpha.
//! - [`ImageRegistry`] indexes decoded images by asset path, which is the URI
//!   Noesis passes for `Source="Images/BgTile.png"`.
//! - [`BevyTextureProvider`] answers Noesis's texture requests (size via
//!   `GetTextureInfo`, pixels via `LoadTexture`) from a [`SharedImageMap`] the
//!   plugin refreshes from the registry each frame.
//!
//! [`ImageAssetPlugin`] (added by [`NoesisPlugin`](crate::NoesisPlugin)) keeps
//! the registry current. Load images with `asset_server.load("Images/BgTile.png")`
//! and keep the handle alive. Noesis caches a texture it couldn't find, so list
//! the URI in [`NoesisView::wait_for_images`](crate::NoesisView::wait_for_images)
//! to hold the scene build until the image has loaded. To feed pixels from
//! code, use [`ImageRegistry::insert`] or the
//! [`NoesisImaging`](crate::imaging::NoesisImaging) bridge.
//!
//! Noesis caches a texture per URI. When the bytes behind a URI that is
//! already registered change (hot reload, or a new `insert`), every live scene
//! is rebuilt so the new pixels show. Adding a URI no scene has resolved yet
//! triggers no rebuild.
//!
//! # Premultiplied alpha
//!
//! Noesis blends `SrcOver` as `(One, OneMinusSrcAlpha)`, which expects
//! premultiplied alpha; straight alpha fringes at partially transparent edges.
//! [`ImageAssetLoader`] premultiplies at decode time, so every byte in
//! [`ImageAsset::bytes`], [`ImageRegistry`], and [`SharedImageMap`] is
//! premultiplied. Bytes passed to [`ImageRegistry::insert`] must be
//! premultiplied too.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::asset::{AssetApp, AssetLoader, LoadContext, io::Reader};
use bevy::prelude::*;

use noesis_runtime::texture_provider::{ImageData, TextureInfo, TextureProvider};

/// A decoded image: tightly-packed, premultiplied RGBA8 with
/// `bytes.len() == width * height * 4`.
#[derive(Asset, TypePath, Debug, Clone)]
pub struct ImageAsset {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Tightly-packed RGBA8 pixels, premultiplied alpha (see module docs).
    pub bytes: Arc<Vec<u8>>,
}

/// Errors from [`ImageAssetLoader`].
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

/// Decodes `.png` / `.jpg` / `.jpeg` files into premultiplied RGBA8
/// [`ImageAsset`]s. The decoded pixels stay in memory for as long as the asset
/// lives, since Noesis loads textures on demand.
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

/// Premultiplies RGBA8 in place with rounding `(c * a + 127) / 255`.
/// `bytes.len()` must be a multiple of 4. Zero-alpha pixels become transparent
/// black so no stale colour bleeds through `SrcOver` edges.
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
        chunk[0] = ((u32::from(chunk[0]) * a + 127) / 255) as u8;
        chunk[1] = ((u32::from(chunk[1]) * a + 127) / 255) as u8;
        chunk[2] = ((u32::from(chunk[2]) * a + 127) / 255) as u8;
    }
}

/// Every image Noesis can see, keyed by URI.
///
/// [`update_image_registry`] fills it from loaded [`ImageAsset`]s, keyed by
/// asset path (`"Images/BgTile.png"`), and removes entries when the asset is
/// dropped. A XAML `Source` must match the key exactly.
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
    /// Looks up a registered image as `(width, height, bytes)`.
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

    /// `true` when no images are registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over the registered URIs. Order is undefined.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    /// Registers pixels that didn't come through the asset server. `bytes` must
    /// be tightly-packed premultiplied RGBA8, `width * height * 4` long.
    /// Replacing the bytes of an existing URI rebuilds live scenes (see the
    /// [module docs](self)).
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

    /// Drops the image under `uri`.
    pub(crate) fn remove(&mut self, uri: &str) {
        self.entries.remove(uri);
    }
}

/// Keeps [`ImageRegistry`] in sync with `AssetEvent<ImageAsset>`. Runs in
/// `Update`.
#[allow(clippy::needless_pass_by_value)]
pub fn update_image_registry(
    mut events: MessageReader<AssetEvent<ImageAsset>>,
    assets: Res<Assets<ImageAsset>>,
    asset_server: Res<AssetServer>,
    mut registry: ResMut<ImageRegistry>,
    // Removal events arrive after the path is gone (`get_path` is `None`), so
    // remember each id's key.
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

type ImageMapEntries = HashMap<String, RegisteredImage>;

/// The image map [`BevyTextureProvider`] reads. The plugin copies
/// [`ImageRegistry`] into it every frame. Cloning shares the same map.
#[derive(Clone, Default)]
pub struct SharedImageMap(pub(crate) Arc<Mutex<ImageMapEntries>>);

impl SharedImageMap {
    /// Replaces the map contents with the registry's.
    ///
    /// # Panics
    ///
    /// Panics on mutex poisoning (a bug, not a runtime condition).
    pub fn sync_from(&self, registry: &ImageRegistry) {
        let mut guard = self.0.lock().expect("SharedImageMap mutex poisoned");
        guard.clone_from(&registry.entries);
    }
}

/// The [`TextureProvider`] the plugin installs. Serves images from a
/// [`SharedImageMap`] by exact URI.
///
/// The pixels `load` returns borrow the provider and stay valid until the
/// next `load` call.
pub struct BevyTextureProvider {
    shared: SharedImageMap,
    current: Option<Arc<Vec<u8>>>,
    current_dims: (u32, u32),
}

impl BevyTextureProvider {
    /// Creates a provider that serves images from `map`.
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

/// Registers [`ImageAsset`], its loader, and [`ImageRegistry`], and keeps the
/// registry current. Added by [`NoesisPlugin`](crate::NoesisPlugin). The
/// provider itself is installed by
/// [`NoesisRenderPlugin`](crate::NoesisRenderPlugin).
pub struct ImageAssetPlugin;

impl Plugin for ImageAssetPlugin {
    fn build(&self, app: &mut App) {
        app.init_asset::<ImageAsset>()
            .init_asset_loader::<ImageAssetLoader>()
            .init_resource::<ImageRegistry>()
            .add_systems(Update, update_image_registry);
    }
}

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

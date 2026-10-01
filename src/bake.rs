//! Render XAML once into a static texture you can map onto your own geometry.
//!
//! A [`NoesisView`](crate::NoesisView) redraws a UI onto a camera every frame.
//! For a label, badge or in-world sign that rarely changes, add
//! [`NoesisLabelBakerPlugin`] and call [`NoesisLabelBaker::bake_label`] with a
//! template URI and the text for its named elements. You get a
//! [`Handle<Image>`] immediately; Noesis fills its texture a frame or so later.
//!
//! ```ignore
//! fn spawn_sign(
//!     baker: Res<NoesisLabelBaker>,
//!     mut images: ResMut<Assets<Image>>,
//!     mut materials: ResMut<Assets<StandardMaterial>>,
//! ) {
//!     let image = baker.bake_label(
//!         "sign:exit",
//!         "ui/sign.xaml",
//!         UVec2::new(256, 64),
//!         vec![("Caption".into(), "EXIT".into())],
//!         &mut images,
//!     );
//!     let material = materials.add(StandardMaterial {
//!         base_color_texture: Some(image),
//!         alpha_mode: AlphaMode::Premultiplied,
//!         ..default()
//!     });
//!     // ...spawn a mesh with `material`.
//! }
//! ```
//!
//! Results are cached by `content_key`: the same key returns the same handle and
//! bakes nothing, so repeated content shares one texture. The cache holds a
//! strong handle to every texture it baked for the app's lifetime; nothing is
//! evicted.
//!
//! A bake waits until the template's XAML asset has loaded and the font
//! fallback chain is installed (which happens once fonts load for the live
//! views), then retries each frame. [`NoesisLabelBaker::pending_count`] tells
//! you when the queue has drained.
//!
//! # Texture format
//!
//! Noesis renders straight into the image's GPU texture: no copy, no CPU
//! readback. The texture is `Rgba8UnormSrgb` with an `Rgba8Unorm` view format;
//! Noesis writes sRGB-encoded bytes through the `Rgba8Unorm` view, and sampling
//! the sRGB texture decodes them. Output is premultiplied alpha, so use
//! `AlphaMode::Premultiplied`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::prelude::*;
use bevy::render::render_resource::{
    Extent3d, Texture, TextureDimension, TextureFormat, TextureUsages,
};
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
    render_asset::RenderAssets,
    texture::GpuImage,
};

use crate::render::{NoesisRenderState, NoesisSet};

/// Sampling format of a baked label. `StandardMaterial` color maps want sRGB.
const SAMPLE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;
/// The `view_formats` alias Noesis renders into. Its pipelines compile against
/// `Rgba8Unorm`, and it writes sRGB bytes raw (no linearization).
const RENDER_FORMAT: TextureFormat = TextureFormat::Rgba8Unorm;
const RENDER_VIEW_FORMATS: &[TextureFormat] = &[RENDER_FORMAT];

/// A queued request to render `xaml_uri` into the image identified by `target`.
#[derive(Clone)]
struct BakeRequest {
    target: AssetId<Image>,
    xaml_uri: String,
    size: UVec2,
    fields: Vec<(String, String)>,
}

#[derive(Default)]
struct BakerState {
    /// Content key to handle. Identical keys reuse one baked texture.
    cache: HashMap<String, Handle<Image>>,
    /// Requests not yet baked.
    pending: Vec<BakeRequest>,
    /// Requests pulled out of `pending` for the current [`bake_into`] pass but
    /// not yet resolved (baked or requeued). Counted by [`pending_count`] so a
    /// loading state doesn't flash ready while a bake is mid-flight.
    ///
    /// [`bake_into`]: NoesisRenderState::bake_into
    /// [`pending_count`]: NoesisLabelBaker::pending_count
    in_flight: usize,
    /// Targets whose GPU texture the render world still has to resolve.
    want: HashSet<AssetId<Image>>,
    /// Resolved target textures, ready for the main-world bake. Each is the same
    /// GPU resource the `GpuImage` owns, not a copy.
    resolved: HashMap<AssetId<Image>, Texture>,
}

/// Bakes XAML templates into cached [`Image`] textures. Inserted as a resource
/// by [`NoesisLabelBakerPlugin`]; cheap to clone (clones share one cache and
/// queue).
#[derive(Resource, Clone, Default)]
pub struct NoesisLabelBaker {
    inner: Arc<Mutex<BakerState>>,
}

impl NoesisLabelBaker {
    /// Return a [`Handle<Image>`] for `content_key`, baking it from the XAML at
    /// `xaml_uri` at `size` pixels if the key isn't cached yet. `fields` are
    /// `(x:Name, text)` pairs written to the template's `Text` properties before
    /// rendering; a name the template lacks logs a warning.
    ///
    /// A cached key returns its existing handle and ignores the other arguments.
    /// A new texture is transparent until its bake runs. A zero `size` axis is
    /// allocated as 1 pixel.
    ///
    /// # Panics
    ///
    /// If the baker's mutex was poisoned by an earlier panic.
    pub fn bake_label(
        &self,
        content_key: impl Into<String>,
        xaml_uri: impl Into<String>,
        size: UVec2,
        fields: Vec<(String, String)>,
        images: &mut Assets<Image>,
    ) -> Handle<Image> {
        let key = content_key.into();
        let mut state = self.inner.lock().expect("NoesisLabelBaker poisoned");
        if let Some(handle) = state.cache.get(&key) {
            return handle.clone();
        }
        let handle = images.add(bake_target(size));
        state.cache.insert(key, handle.clone());
        state.want.insert(handle.id());
        state.pending.push(BakeRequest {
            target: handle.id(),
            xaml_uri: xaml_uri.into(),
            size,
            fields,
        });
        handle
    }

    /// Number of labels not yet baked. Reaches zero once every queued label has
    /// rendered, so a host can hold a loading screen until then. A label whose
    /// template or fonts never load stays counted forever.
    #[must_use]
    pub fn pending_count(&self) -> usize {
        let guard = self.inner.lock().expect("NoesisLabelBaker poisoned");
        guard.pending.len() + guard.in_flight
    }
}

impl ExtractResource for NoesisLabelBaker {
    type Source = NoesisLabelBaker;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

/// Allocate a label's target: no CPU upload, `Rgba8UnormSrgb` with an
/// `Rgba8Unorm` render alias.
fn bake_target(size: UVec2) -> Image {
    let mut image = Image::new_uninit(
        Extent3d {
            width: size.x.max(1),
            height: size.y.max(1),
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        SAMPLE_FORMAT,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.texture_descriptor.usage =
        TextureUsages::RENDER_ATTACHMENT | TextureUsages::TEXTURE_BINDING;
    image.texture_descriptor.view_formats = RENDER_VIEW_FORMATS;
    image.sampler = ImageSampler::linear();
    image
}

/// Render-world pass: hand each wanted target's GPU texture to the main world.
/// `GpuImage`s prepare in [`RenderSystems::PrepareAssets`], so the texture
/// exists by this `Prepare` system. Noesis never runs here.
#[allow(clippy::needless_pass_by_value)]
fn resolve_bake_textures(baker: Res<NoesisLabelBaker>, gpu_images: Res<RenderAssets<GpuImage>>) {
    let mut guard = baker.inner.lock().expect("NoesisLabelBaker poisoned");
    if guard.want.is_empty() {
        return;
    }
    let ready: Vec<AssetId<Image>> = guard
        .want
        .iter()
        .copied()
        .filter(|id| gpu_images.get(*id).is_some())
        .collect();
    for id in ready {
        if let Some(gpu) = gpu_images.get(id) {
            guard.resolved.insert(id, gpu.texture.clone());
            guard.want.remove(&id);
        }
    }
}

/// Main-world pass: render Noesis into each target whose texture has been
/// resolved. Runs on the main thread with [`NoesisRenderState`]. A request stays
/// queued until its texture is resolved and fonts and template are ready.
#[allow(clippy::needless_pass_by_value)]
fn bake_pending_labels(
    baker: Option<Res<NoesisLabelBaker>>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    let Some(baker) = baker else {
        return;
    };

    // Bake outside the lock so the render-world system never waits on `bake_into`.
    let mut work: Vec<(BakeRequest, Texture)> = Vec::new();
    {
        let mut guard = baker.inner.lock().expect("NoesisLabelBaker poisoned");
        if guard.pending.is_empty() {
            return;
        }
        let mut keep = Vec::new();
        for req in std::mem::take(&mut guard.pending) {
            match guard.resolved.get(&req.target).cloned() {
                Some(texture) => work.push((req, texture)),
                None => keep.push(req),
            }
        }
        guard.pending = keep;
        // Keeps `pending_count` from dipping to zero mid-bake.
        guard.in_flight = work.len();
    }
    if work.is_empty() {
        return;
    }

    let mut baked = Vec::new();
    let mut requeue = Vec::new();
    for (req, texture) in work {
        let render_view = texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("noesis label bake (render, Unorm)"),
            format: Some(RENDER_FORMAT),
            ..Default::default()
        });
        if state.bake_into(&render_view, &req.xaml_uri, req.size, &req.fields) {
            baked.push(req.target);
        } else {
            requeue.push(req);
        }
    }

    let mut guard = baker.inner.lock().expect("NoesisLabelBaker poisoned");
    for id in baked {
        guard.resolved.remove(&id);
    }
    guard.pending.append(&mut requeue);
    guard.in_flight = 0;
}

/// Inserts the [`NoesisLabelBaker`] resource and its systems. Not part of
/// [`crate::NoesisPlugin`]; add it after that plugin.
pub struct NoesisLabelBakerPlugin;

impl Plugin for NoesisLabelBakerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisLabelBaker>()
            .add_plugins(ExtractResourcePlugin::<NoesisLabelBaker>::default());

        app.add_systems(PostUpdate, bake_pending_labels.in_set(NoesisSet::Apply));

        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(Render, resolve_bake_textures.in_set(RenderSystems::Prepare));
        }
    }
}

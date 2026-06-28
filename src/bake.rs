//! Bake an XAML view to an offscreen [`Handle<Image>`].
//!
//! The live overlay ([`crate::render`]) renders one Noesis view onto a camera
//! every frame. Some hosts instead want a *static* texture rendered from XAML —
//! a label, a badge, an in-world panel — that they map onto their own geometry.
//! [`NoesisLabelBaker`] provides that: call [`NoesisLabelBaker::bake_label`]
//! with a template URI and the text to drop into its named elements, and get
//! back a [`Handle<Image>`] whose GPU texture Noesis fills in within ~1 frame.
//!
//! Results are cached by an opaque `content_key`: identical keys return the
//! same handle and bake nothing, so repeated content (every `74LS04` label)
//! shares one texture — the same reuse discipline the mesh cache follows.
//!
//! # How it renders without a copy or readback
//!
//! The image is allocated up front as a Bevy [`Image`] (so Bevy owns its
//! `GpuImage`), then Noesis renders *straight into* a `Rgba8Unorm` view of that
//! texture via the device's `set_onscreen_target` contract — the same one the
//! live intermediate uses. No `copy_texture_to_texture`, no CPU readback.
//!
//! The texture is created `Rgba8UnormSrgb` with `Rgba8Unorm` listed in
//! `view_formats`. Noesis writes sRGB-encoded bytes raw through the `Rgba8Unorm`
//! render alias; a `StandardMaterial` samples the sRGB texture and decodes them
//! back. This is the inverse of `create_intermediate`'s dual-alias trick. Output
//! is premultiplied alpha, so sample it with `AlphaMode::Premultiplied`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::asset::RenderAssetUsages;
use bevy::image::{Image, ImageSampler};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
    render_asset::RenderAssets,
    texture::GpuImage,
};

use crate::render::NoesisRenderState;

/// Sampling format of a baked label. `StandardMaterial` color maps want sRGB;
/// Noesis renders through the [`RENDER_FORMAT`] alias below.
const SAMPLE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;
/// The `view_formats` alias Noesis renders into — its pipeline cache compiles
/// against `Rgba8Unorm`, and it writes sRGB bytes raw (no linearization).
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
    /// Content key → handle. Identical keys reuse one baked texture.
    cache: HashMap<String, Handle<Image>>,
    /// Requests awaiting their first bake on the render side.
    pending: Vec<BakeRequest>,
}

/// Main-world handle to the label baker. Cheap to clone — it wraps a shared
/// cache + request queue the render world drains. Insert via
/// [`NoesisLabelBakerPlugin`].
#[derive(Resource, Clone, Default)]
pub struct NoesisLabelBaker {
    inner: Arc<Mutex<BakerState>>,
}

impl NoesisLabelBaker {
    /// Return a [`Handle<Image>`] for `content_key`, baking it from `xaml_uri`
    /// if it isn't already cached. `fields` are `(x:Name, text)` pairs written
    /// to the template's named `TextBlock`/`TextBox` elements before rendering.
    ///
    /// Identical `content_key`s return the same handle and enqueue no work. A
    /// freshly-baked texture is transparent until the bake completes (~1 frame);
    /// for static labels that warm-up frame is invisible.
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
        state.pending.push(BakeRequest {
            target: handle.id(),
            xaml_uri: xaml_uri.into(),
            size,
            fields,
        });
        handle
    }
}

impl ExtractResource for NoesisLabelBaker {
    type Source = NoesisLabelBaker;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

/// Allocate the offscreen target for a label: an uninitialized (no CPU upload)
/// `Rgba8UnormSrgb` image renderable through an `Rgba8Unorm` alias.
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
    // Crisp scaling when the panel is small on screen.
    image.sampler = ImageSampler::linear();
    image
}

/// Render-world buffer of requests not yet baked (their `GpuImage` may not be
/// prepared, or Noesis prerequisites — fonts/template — may not be ready). Held
/// across frames so a request retries until it succeeds.
#[derive(Resource, Default)]
struct PendingBakes(Vec<BakeRequest>);

#[allow(clippy::needless_pass_by_value)]
fn bake_pending_labels(
    baker: Option<Res<NoesisLabelBaker>>,
    mut retry: ResMut<PendingBakes>,
    gpu_images: Res<RenderAssets<GpuImage>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    if let Some(baker) = baker {
        let mut guard = baker.inner.lock().expect("NoesisLabelBaker poisoned");
        retry.0.append(&mut guard.pending);
    }
    if retry.0.is_empty() {
        return;
    }

    let mut still_pending = Vec::new();
    for req in std::mem::take(&mut retry.0) {
        let Some(gpu) = gpu_images.get(req.target) else {
            // GpuImage not prepared this frame yet — try again next frame.
            still_pending.push(req);
            continue;
        };
        let render_view = gpu.texture.create_view(&wgpu::TextureViewDescriptor {
            label: Some("noesis label bake (render, Unorm)"),
            format: Some(RENDER_FORMAT),
            ..Default::default()
        });
        if !state.bake_into(&render_view, &req.xaml_uri, req.size, &req.fields) {
            // Fonts / template not ready — retry on a later frame.
            still_pending.push(req);
        }
    }
    retry.0 = still_pending;
}

/// Wires [`NoesisLabelBaker`] into the app. Add after [`crate::NoesisPlugin`].
pub struct NoesisLabelBakerPlugin;

impl Plugin for NoesisLabelBakerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisLabelBaker>()
            .add_plugins(ExtractResourcePlugin::<NoesisLabelBaker>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        // Runs in `Prepare`, which is ordered after `PrepareAssets` (where
        // `GpuImage`s are created) — so a freshly-allocated target's texture is
        // available the same frame it's requested. Tolerant of running before
        // `NoesisRenderState` exists or fonts load: such requests just retry.
        render_app
            .init_resource::<PendingBakes>()
            .add_systems(Render, bake_pending_labels.in_set(RenderSystems::Prepare));
    }
}

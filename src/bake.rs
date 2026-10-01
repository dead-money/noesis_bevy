//! Bake an XAML view to an offscreen [`Handle<Image>`].
//!
//! The live overlay ([`crate::render`]) renders one Noesis view onto a camera
//! every frame. Some hosts instead want a *static* texture rendered from XAML
//! (a label, a badge, an in-world panel) that they map onto their own geometry.
//! [`NoesisLabelBaker`] provides that: call [`NoesisLabelBaker::bake_label`]
//! with a template URI and the text to drop into its named elements, and get
//! back a [`Handle<Image>`] whose GPU texture Noesis fills in within ~1 frame.
//!
//! Results are cached by an opaque `content_key`: identical keys return the
//! same handle and bake nothing, so repeated content (every `74LS04` label)
//! shares one texture.
//!
//! # How it renders without a copy or readback
//!
//! The image is allocated up front as a Bevy [`Image`] (so Bevy owns its
//! `GpuImage`), then Noesis renders *straight into* a `Rgba8Unorm` view of that
//! texture via the device's `set_onscreen_target` contract. No
//! `copy_texture_to_texture`, no CPU readback.
//!
//! The texture is created `Rgba8UnormSrgb` with `Rgba8Unorm` listed in
//! `view_formats`. Noesis writes sRGB-encoded bytes raw through the `Rgba8Unorm`
//! render alias; a `StandardMaterial` samples the sRGB texture and decodes them
//! back. Output is premultiplied alpha, so sample it with
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

/// Sampling format of a baked label. `StandardMaterial` color maps want sRGB;
/// Noesis renders through the [`RENDER_FORMAT`] alias below.
const SAMPLE_FORMAT: TextureFormat = TextureFormat::Rgba8UnormSrgb;
/// The `view_formats` alias Noesis renders into: its pipeline cache compiles
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
    /// Content key to handle. Identical keys reuse one baked texture.
    cache: HashMap<String, Handle<Image>>,
    /// Requests awaiting their first bake on the main side.
    pending: Vec<BakeRequest>,
    /// Requests pulled out of `pending` for the current [`bake_into`] pass but
    /// not yet resolved (baked or requeued). Counted by [`pending_count`] so a
    /// loading state doesn't flash ready while a bake is mid-flight.
    ///
    /// [`bake_into`]: NoesisRenderState::bake_into
    /// [`pending_count`]: NoesisLabelBaker::pending_count
    in_flight: usize,
    /// Targets whose GPU texture the render world still has to resolve.
    /// `bake_label` inserts here; the render-world system drains into `resolved`.
    want: HashSet<AssetId<Image>>,
    /// Target textures the render world resolved, ready for the main-world bake.
    /// The texture is the same GPU resource Bevy's `GpuImage` owns (no copy);
    /// it crosses the world boundary because `Texture` is `Send + Sync`.
    resolved: HashMap<AssetId<Image>, Texture>,
}

/// Main-world handle to the label baker. Cheap to clone; it wraps a shared
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
        state.want.insert(handle.id());
        state.pending.push(BakeRequest {
            target: handle.id(),
            xaml_uri: xaml_uri.into(),
            size,
            fields,
        });
        handle
    }

    /// Number of labels still waiting to bake. Drops to zero once every queued
    /// label has rendered, so a host can hold a loading state until then. A
    /// label whose template or fonts never load stays counted.
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

/// Render-world pass: hand each pending bake target's GPU texture back to the
/// main world. `GpuImage`s prepare in [`RenderSystems::PrepareAssets`], so by
/// this `Prepare` system the texture exists; cloning it is just another handle
/// to the same GPU resource. Noesis itself never runs here, only the texture
/// crosses worlds.
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

/// Main-world pass: render Noesis into each target whose texture the render
/// world resolved. Runs where [`NoesisRenderState`] lives (main thread, `!Send`)
/// and pulls only the resolved `Texture` across the boundary, so the bake stays
/// on the single Noesis thread. A request stays queued until its texture is
/// resolved and Noesis prerequisites (fonts, template) are ready.
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

    // Pull the bakeable requests (texture resolved) out from under the lock, so
    // `bake_into` (slow) never stalls the render thread holding the mutex.
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
        // Keep these counted while they render outside the lock, so
        // `pending_count` doesn't transiently drop to zero mid-bake.
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
            // Fonts/template not ready; retry on a later frame.
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

/// Wires [`NoesisLabelBaker`] into the app. Add after [`crate::NoesisPlugin`].
pub struct NoesisLabelBakerPlugin;

impl Plugin for NoesisLabelBakerPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisLabelBaker>()
            .add_plugins(ExtractResourcePlugin::<NoesisLabelBaker>::default());

        // Runs in `NoesisSet::Apply`, after the scene is ensured and before the
        // frame is driven. Tolerant of running before `NoesisRenderState` exists,
        // a target texture is resolved, or fonts load: such requests just retry.
        app.add_systems(PostUpdate, bake_pending_labels.in_set(NoesisSet::Apply));

        // Render-world half: resolve each queued target's GPU texture and hand it
        // back through the shared state for the main-world bake above.
        if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
            render_app.add_systems(Render, resolve_bake_textures.in_set(RenderSystems::Prepare));
        }
    }
}

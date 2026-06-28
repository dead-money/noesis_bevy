//! Render-graph integration for Noesis (Phase 4.D.3).
//!
//! [`NoesisRenderPlugin`] is a sub-plugin on [`RenderApp`] that:
//!
//! - Builds a [`WgpuRenderDevice`] against Bevy's shared `wgpu::Device` and
//!   registers it with Noesis in [`Plugin::finish`].
//! - Installs a [`BevyXamlProvider`] whose backing [`SharedXamlMap`] is
//!   refreshed each frame from the main world's [`XamlRegistry`] via a
//!   system running in [`ExtractSchedule`].
//! - Lazily builds a [`dm_noesis_runtime::view::View`] + intermediate `Rgba8Unorm`
//!   texture the first frame the configured XAML URI resolves.
//! - Drives Noesis (layout, render-tree snapshot, offscreen + onscreen
//!   render) from a `Render` schedule system — the graph node itself only
//!   blits the pre-populated intermediate into the camera's [`ViewTarget`].
//! - Registers [`NoesisNode`] into [`Core2d`] between
//!   [`Node2d::MainTransparentPass`] and [`Node2d::EndMainPass`].
//!
//! The intermediate is `Rgba8Unorm` because [`WgpuRenderDevice`]'s pipeline
//! cache compiles every shader variant against that one color format.
//! The blit pipeline is cached per encountered `ViewTarget` format. Phase 9
//! drops the intermediate + blit once `PipelineCache` keys on format.
//!
//! # Lifecycle ordering
//!
//! Noesis demands a strict teardown sequence: `Renderer::shutdown()` must
//! run while both the `View` and the registered `RenderDevice` are still
//! alive, then the `View` drops, then the device's [`Registered`] guard,
//! then the provider's. We enforce this by holding every Noesis handle
//! in [`NoesisRenderState`] and implementing [`Drop`] explicitly.
//!
//! [`Registered`]: dm_noesis_runtime::render_device::Registered

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use bevy::core_pipeline::core_2d::graph::{Core2d, Node2d};
use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};
use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_component::{ExtractComponent, ExtractComponentPlugin},
    extract_resource::{ExtractResource, ExtractResourcePlugin},
    render_graph::{
        NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
    },
    renderer::{RenderContext, RenderDevice, RenderQueue},
    view::ViewTarget,
};
use dm_noesis_runtime::events::{
    ClickSubscription, KeyDownSubscription, subscribe_click, subscribe_keydown,
};
use dm_noesis_runtime::view::{FrameworkElement, Key, View};

use crate::events::{SharedClickQueue, SharedKeyDownQueue};
use crate::focus::SharedFocusQueue;
use crate::font::{BevyFontProvider, FontRegistry, SharedFontMap};
use crate::geometry::SharedGeometryQueue;
use crate::image::{BevyTextureProvider, ImageRegistry, SharedImageMap};
use crate::items::{ItemsBinding, NoesisItemsSources};
use crate::render_device::WgpuRenderDevice;
use crate::text::{SharedTextChangedQueue, SharedTextWriteQueue};
use crate::viewmodel::{AttachTarget, NoesisViewModels, SharedVmChangedQueue, VmEntry};
use crate::visibility::SharedVisibilityQueue;
use crate::xaml::{BevyXamlProvider, SharedXamlMap, XamlRegistry};

/// Color format of the per-view intermediate Noesis paints into. Must match
/// the private `RT_COLOR_FORMAT` in `render_device::wgpu_device`; keeping
/// the coupling documented rather than sharing the const cross-module.
const INTERMEDIATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn flags_from(config: &NoesisScene) -> u32 {
    let mut f = 0;
    if config.ppaa {
        f |= dm_noesis_runtime::view::RenderFlag::Ppaa as u32;
    }
    f
}

/// `Rgba8UnormSrgb` alias of the intermediate, used for sampling during the
/// blit when `ViewTarget` is itself sRGB-encoded. See `create_intermediate`
/// and [`NoesisNode::run`] for the gamma-roundtrip rationale.
const INTERMEDIATE_SAMPLE_FORMAT_SRGB: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// Whether a `ViewTarget` colour format stores *linear* values that are
/// gamma-encoded downstream (i.e. an HDR camera's float target). These need the
/// same sRGB→linear decode on blit as a true sRGB target — see [`NoesisNode::run`].
/// A plain `Rgba8Unorm` target is excluded: it is written and displayed raw.
fn is_linear_float(format: wgpu::TextureFormat) -> bool {
    matches!(
        format,
        wgpu::TextureFormat::Rgba16Float
            | wgpu::TextureFormat::Rgba32Float
            | wgpu::TextureFormat::Rg11b10Ufloat
    )
}

fn create_intermediate(
    device: &wgpu::Device,
    size: UVec2,
) -> (wgpu::Texture, wgpu::TextureView, wgpu::TextureView) {
    // Two views over the same bytes:
    //
    //   `render_view`  — `Rgba8Unorm`. Noesis writes sRGB colours straight
    //                    into the bytes (no linearisation in the pipeline;
    //                    DeviceCaps::linearRendering = false), so the stored
    //                    byte values already match the sRGB representation.
    //
    //   `sample_view`  — `Rgba8UnormSrgb`. Used by the blit sampler when the
    //                    camera's ViewTarget is sRGB. Reading through the
    //                    sRGB alias applies the sRGB → linear decode; the
    //                    ViewTarget write re-applies linear → sRGB, and the
    //                    round-trip reproduces the stored byte value. Without
    //                    this the byte value would be treated as linear on
    //                    read and gamma-encoded on write, ending ~40 %
    //                    brighter than intended.
    //
    // Requires listing the sRGB alias in `view_formats` at creation time
    // (wgpu restricts which formats may be reinterpreted later).
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("noesis intermediate"),
        size: wgpu::Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: INTERMEDIATE_FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[INTERMEDIATE_SAMPLE_FORMAT_SRGB],
    });
    let render_view = tex.create_view(&wgpu::TextureViewDescriptor {
        label: Some("noesis intermediate (render, Unorm)"),
        format: Some(INTERMEDIATE_FORMAT),
        ..Default::default()
    });
    let sample_view = tex.create_view(&wgpu::TextureViewDescriptor {
        label: Some("noesis intermediate (sample, UnormSrgb)"),
        format: Some(INTERMEDIATE_SAMPLE_FORMAT_SRGB),
        ..Default::default()
    });
    (tex, render_view, sample_view)
}

// ─────────────────────────────────────────────────────────────────────────────
// Public configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Scene configuration for the single active Noesis View. Insert as a
/// [`Resource`] on the main app; the render app receives a copy via
/// [`ExtractResource`]. Phase 7 generalises to per-entity `NoesisView`
/// components; one-per-app is enough to bring up `hello_xaml`.
#[derive(Resource, ExtractResource, Clone, Debug)]
pub struct NoesisScene {
    /// Asset URI [`XamlRegistry`] keys on — typically the path passed to
    /// `AssetServer::load("foo.xaml")`.
    pub xaml_uri: String,
    /// Size of the intermediate render target Noesis paints into. The blit
    /// stretches this to fill whatever camera `ViewTarget` it composes on.
    pub size: UVec2,
    /// DPI scale for the view's content (1.0 == 96 ppi). Scales all UI crisply
    /// (vector re-tessellation, not an upscale blur) without changing
    /// [`size`](Self::size) — drive it from the window's scale factor for
    /// resolution-independent UI. Re-applied live via `View::set_scale` when it
    /// changes; no scene rebuild.
    pub scale: f32,
    /// Folder URIs whose fonts must be loaded before the XAML is parsed.
    /// Noesis's `CachedFontProvider` caches an empty folder the first
    /// time it scans one — so if fonts haven't loaded by the time we run
    /// `FrameworkElement::load`, all text in that folder renders
    /// invisibly forever. Populate this with each folder your XAML
    /// references in `FontFamily="Folder/#Family"` attributes.
    ///
    /// Folder URIs should match Noesis's form (no trailing slash) — e.g.
    /// `"Fonts"` for `FontFamily="Fonts/#Bitter"`.
    pub wait_for_fonts: Vec<String>,
    /// Specific `(folder, filename)` pairs that must be present in
    /// `FontRegistry` before scene build. Stronger guard than
    /// [`wait_for_fonts`](Self::wait_for_fonts): that one only checks "at
    /// least one entry in this folder", which unblocks scene creation as
    /// soon as the first font arrives — too early when the scene's
    /// application resources need *a specific* font (e.g. the theme's
    /// PT Root UI). Populate with the critical filenames your theme +
    /// scene jointly require.
    pub wait_for_font_files: Vec<(String, String)>,
    /// Specific image URIs that must be present in [`crate::ImageRegistry`]
    /// before scene build. Noesis's `TextureProvider::GetTextureInfo`
    /// returns an empty / zero-size info when the URI is unknown, and the
    /// XAML parser caches that as a permanent "no texture for this URI"
    /// — so an `<Image Source="Big.png"/>` whose decode hadn't finished
    /// when the scene built renders empty *forever*, even after the
    /// bytes land. Populate with the URIs your scene's images reference
    /// to keep the scene from building until they're all decoded.
    pub wait_for_images: Vec<String>,
    /// Toggle Noesis's built-in Per-Primitive AA ([`RenderFlag::Ppaa`]).
    /// Changing this at runtime re-calls `View::set_flags` — no scene
    /// rebuild, no View teardown.
    ///
    /// [`RenderFlag::Ppaa`]: dm_noesis_runtime::view::RenderFlag::Ppaa
    pub ppaa: bool,
    /// `ResourceDictionary` URIs to install as the process-global
    /// application resources (styles, brushes, `ControlTemplate`s), in
    /// dependency order. Each URI must resolve via the same XAML
    /// provider that serves `xaml_uri`. Loaded once on first scene
    /// build; later changes are ignored.
    ///
    /// The plugin uses
    /// [`dm_noesis_runtime::gui::install_app_resources_chain`] — an
    /// empty parent `ResourceDictionary` is installed up front, then
    /// each leaf is added to `parent.MergedDictionaries` and its
    /// `Source` assigned in order. This ensures cross-sibling
    /// `{StaticResource Foo}` references inside one leaf can find
    /// keys from earlier leaves (the simpler `LoadXaml +
    /// SetApplicationResources` path silently null-resolves them).
    ///
    /// A single-URI list works fine — it just installs that one dict
    /// as a merged child of an otherwise-empty parent. For the Noesis
    /// SDK sample themes, that means a `vec![\"NoesisTheme.DarkBlue.xaml\"]`
    /// is sufficient (the `xaml_viewer` example does this via
    /// `--theme`).
    pub application_resources: Vec<String>,
    /// Font families Noesis falls back to when an element doesn't
    /// resolve its declared `FontFamily`. Each entry is a Noesis-style
    /// path-rooted family — e.g. `"Fonts/#Bitter"` or
    /// `"Fonts/#PT Root UI"`. The first entry that has glyphs for a
    /// codepoint wins.
    ///
    /// Installed once per process via `Noesis::SetFontFallbacks` after
    /// the font registry has at least one entry. The plugin eagerly
    /// registers every loaded font face with Noesis's
    /// `CachedFontProvider` before scene build (and incrementally as
    /// new fonts arrive), so this list is purely the WPF-style fallback
    /// chain — there's no need to mention non-fallback families just
    /// to make `FontFamily="Fonts/#X"` references resolve.
    ///
    /// The default chain (`["Fonts/#Bitter"]`) keeps the SDK examples
    /// working; downstream apps shipping their own font set should
    /// override.
    pub font_fallbacks: Vec<String>,
}

impl Default for NoesisScene {
    fn default() -> Self {
        Self {
            xaml_uri: String::new(),
            size: UVec2::new(512, 512),
            scale: 1.0,
            wait_for_fonts: Vec::new(),
            wait_for_font_files: Vec::new(),
            wait_for_images: Vec::new(),
            ppaa: true,
            application_resources: Vec::new(),
            font_fallbacks: vec!["Fonts/#Bitter".to_string()],
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-world resource: owns Noesis handles + the per-scene instance
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Resource)]
pub(crate) struct NoesisRenderState {
    device: wgpu::Device,
    shared_map: SharedXamlMap,
    shared_fonts: SharedFontMap,
    shared_images: SharedImageMap,
    // `Option` so `Drop` can take + drop each in the right order.
    registered_device: Option<dm_noesis_runtime::render_device::Registered>,
    registered_provider: Option<dm_noesis_runtime::xaml_provider::Registered>,
    registered_fonts: Option<dm_noesis_runtime::font_provider::Registered>,
    registered_textures: Option<dm_noesis_runtime::texture_provider::Registered>,
    scene: Option<SceneInstance>,
    /// One-time flag for `SetFontFallbacks`/`SetFontDefaultProperties` —
    /// must fire AFTER Bevy has loaded at least one font into
    /// `SharedFontMap`, because Noesis's `SetFontFallbacks` eagerly runs
    /// `FontProvider::ScanFolder` to prime its cache.
    fallbacks_installed: bool,
    /// `(folder, filename)` pairs we've already eagerly registered with
    /// the C++ `CachedFontProvider` cache. Used by
    /// [`Self::register_pending_fonts`] to diff incrementally so we
    /// re-register each face exactly once. The eager-registration path
    /// makes scan-time gating irrelevant — any face present here is
    /// findable by `MatchFont` regardless of when `ScanFolder` ran.
    registered_faces: HashSet<(String, String)>,
    /// URI list already handed to
    /// `gui::install_app_resources_chain`. Guards us against
    /// re-installing the same chain every frame. `None` means we
    /// haven't installed anything yet; `Some(chain)` records exactly
    /// what we last installed.
    loaded_app_resources_chain: Option<Vec<String>>,
    /// Wall-clock origin for `View::Update(time)`. Bevy's `Time<Real>`
    /// isn't extracted to the render world by default (only
    /// `Time<Virtual>` and the generic `Time` are), so we keep our own.
    /// Drives storyboard progression; `elapsed_secs_f64()` each frame
    /// vs. Noesis's requirement for monotonically-increasing seconds.
    clock_origin: std::time::Instant,
    /// Last `swallow` set installed for each subscribed keydown name.
    /// Used by [`Self::sync_keydown_subscriptions`] to detect when a
    /// swallow list has changed and re-bind the C++-side handler with
    /// the new closure (which captures `swallow` by value).
    last_keydown_swallow: HashMap<String, Vec<Key>>,
    /// Reusable offscreen view for baking label panels (lazily built). Lives
    /// here so it shares the single registered device and its renderer is torn
    /// down before the device drops — see [`Drop`].
    bake_rig: Option<BakeRig>,
    /// Live Rust-owned view models (TODO §3 binding bridge). Each owns a
    /// `ClassInstance` + `ClassRegistration` and is attached as a scene
    /// element's `DataContext`. Stored here (not in [`SceneInstance`]) so a VM
    /// survives scene rebuilds — the attach pass re-binds it to the new view.
    /// Released in [`Drop`] before the registered device, while Noesis is still
    /// initialized. See [`crate::viewmodel`].
    view_models: Vec<VmEntry>,
    /// Rust-owned `ItemsSource` collections keyed by `x:Name` (TODO §3). Each
    /// owns an `ObservableCollection` bound to a named `ItemsControl`. Like
    /// [`Self::view_models`] they outlive scene rebuilds (re-bound by the apply
    /// pass) and are released in [`Drop`] before the registered device. See
    /// [`crate::items`].
    items_sources: HashMap<String, ItemsBinding>,
}

struct SceneInstance {
    view: View,
    renderer_initialized: bool,
    // The `Texture` only needs to outlive its views; nothing else reads
    // the field directly but we own it so the GPU allocation lives.
    #[allow(dead_code)]
    intermediate: wgpu::Texture,
    /// `Rgba8Unorm` view — handed to Noesis via `set_onscreen_target` each
    /// frame. Noesis writes sRGB-encoded byte values straight through.
    intermediate_view: wgpu::TextureView,
    /// `Rgba8UnormSrgb` alias — used by the blit sampler when `ViewTarget`
    /// is sRGB, so the stored bytes go through a lossless sRGB→linear→sRGB
    /// round-trip instead of getting double-encoded. See `create_intermediate`.
    intermediate_sample_view: wgpu::TextureView,
    size: UVec2,
    built_for_uri: String,
    /// Last render flags written to the view via `View::set_flags`.
    /// Re-applied only when [`NoesisScene`] changes; avoids the FFI call
    /// on every frame.
    applied_flags: u32,
    /// Last DPI scale written via `View::set_scale`. Re-applied only on change,
    /// like [`applied_flags`](Self::applied_flags).
    applied_scale: f32,
    /// Active `BaseButton::Click` subscriptions keyed by `x:Name`. Synced
    /// each frame against [`crate::events::NoesisClickWatch`] by
    /// [`NoesisRenderState::sync_click_subscriptions`]. Drops with the
    /// scene — orphaned subscriptions can't outlive their button.
    click_subs: HashMap<String, ClickSubscription>,
    /// Active `UIElement::KeyDown` subscriptions keyed by `x:Name`. Synced
    /// each frame against [`crate::events::NoesisKeyDownWatch`] by
    /// [`NoesisRenderState::sync_keydown_subscriptions`]. Same lifetime
    /// rules as `click_subs`.
    keydown_subs: HashMap<String, KeyDownSubscription>,
    /// Last text snapshot per name in [`crate::text::NoesisTextReadWatch`].
    /// Used to dedupe `NoesisTextChanged` emissions — only push when the
    /// text actually differs from the previous frame's snapshot. Names
    /// removed from the watch get pruned out of this map at sync time.
    text_snapshots: HashMap<String, String>,
}

/// A persistent, camera-less Noesis view reused to bake label panels to
/// offscreen textures. Unlike [`SceneInstance`] it owns no intermediate (it
/// renders straight into a caller-supplied [`wgpu::TextureView`]) and is never
/// composited to a camera. One rig serves every label of the same template;
/// see [`NoesisRenderState::bake_into`].
struct BakeRig {
    view: View,
    renderer_initialized: bool,
    /// Template URI the rig's content was loaded from; a different URI forces
    /// a rebuild.
    built_for_uri: String,
    /// Last size handed to `View::set_size`; re-applied only on change.
    size: UVec2,
}

impl NoesisRenderState {
    fn new(device: wgpu::Device, queue: wgpu::Queue) -> Self {
        let shared_map = SharedXamlMap::default();
        let shared_fonts = SharedFontMap::default();
        let shared_images = SharedImageMap::default();
        let wgpu_rd = WgpuRenderDevice::new(device.clone(), queue);
        let registered_device = dm_noesis_runtime::render_device::register(wgpu_rd);
        let xaml_prov = BevyXamlProvider::from_shared(shared_map.clone());
        let registered_provider = dm_noesis_runtime::xaml_provider::set_xaml_provider(xaml_prov);
        let font_prov = BevyFontProvider::from_shared(shared_fonts.clone());
        let registered_fonts = dm_noesis_runtime::font_provider::set_font_provider(font_prov);
        let texture_prov = BevyTextureProvider::from_shared(shared_images.clone());
        let registered_textures =
            dm_noesis_runtime::texture_provider::set_texture_provider(texture_prov);

        // Font fallbacks must NOT be installed here. Noesis's
        // `SetFontFallbacks` eagerly invokes `FontProvider::ScanFolder`
        // to prime its cache; if we call it before Bevy has finished
        // loading the font assets into `SharedFontMap`, the scan returns
        // empty and `CachedFontProvider` caches that empty result
        // forever. Defer installation until `ensure_scene` sees at least
        // one font (see `install_font_fallbacks_if_needed` below).

        Self {
            device,
            shared_map,
            shared_fonts,
            shared_images,
            registered_device: Some(registered_device),
            registered_provider: Some(registered_provider),
            registered_fonts: Some(registered_fonts),
            registered_textures: Some(registered_textures),
            scene: None,
            fallbacks_installed: false,
            registered_faces: HashSet::new(),
            loaded_app_resources_chain: None,
            clock_origin: std::time::Instant::now(),
            last_keydown_swallow: HashMap::new(),
            bake_rig: None,
            view_models: Vec::new(),
            items_sources: HashMap::new(),
        }
    }

    /// Drain pending view-model registrations from [`NoesisViewModels`]: for
    /// each, register the Noesis class, instantiate it, and wire its change
    /// forwarder to `changed`. Builds happen render-side because Noesis objects
    /// are thread-affine to the `View`. Idempotent on an empty queue.
    pub(crate) fn register_view_models(
        &mut self,
        vms: &NoesisViewModels,
        changed: &SharedVmChangedQueue,
    ) {
        let pending = vms.drain_registrations();
        for (id, def) in pending {
            match VmEntry::build(id, &def, changed) {
                Some(entry) => self.view_models.push(entry),
                None => warn!(
                    "NoesisViewModels: failed to register/instantiate class {:?} (duplicate name?)",
                    def.class_name(),
                ),
            }
        }
    }

    /// Drain pending view-model writes from [`NoesisViewModels`] and apply each
    /// to its instance's dependency property. Unknown ids / property names log
    /// a warning and are skipped. Idempotent on an empty queue.
    pub(crate) fn apply_view_model_writes(&mut self, vms: &NoesisViewModels) {
        let pending = vms.drain_writes();
        for (id, prop, value) in pending {
            let Some(entry) = self.view_models.iter().find(|e| e.id == id) else {
                warn!("NoesisViewModels: write to unknown view model {id:?}");
                continue;
            };
            if !entry.write(&prop, &value) {
                warn!("NoesisViewModels: view model {id:?} has no property {prop:?}");
            }
        }
    }

    /// Attach any not-yet-attached view model as its target's `DataContext`.
    /// No-op until the scene's `View` (and any named target) exists; retries
    /// each frame until the element resolves. Re-attaches after a scene
    /// rebuild (the URI key changes, or teardown reset the attach state).
    pub(crate) fn attach_view_models(&mut self) {
        if self.view_models.is_empty() {
            return;
        }
        let Some(scene) = self.scene.as_ref() else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        let uri = scene.built_for_uri.clone();
        for entry in &mut self.view_models {
            if !entry.needs_attach(&uri) {
                continue;
            }
            let target = match entry.target() {
                AttachTarget::Root => scene.view.content(),
                AttachTarget::Named(name) => content.find_name(name),
            };
            let Some(mut element) = target else {
                warn!(
                    "NoesisViewModels: attach target for {:?} not found in scene {:?}",
                    entry.id, scene.built_for_uri,
                );
                continue;
            };
            if element.set_data_context(entry.instance()) {
                entry.mark_attached(&uri);
            } else {
                warn!(
                    "NoesisViewModels: set_data_context returned false for {:?} \
                     (target not a FrameworkElement?)",
                    entry.id,
                );
            }
        }
    }

    /// Drain pending [`NoesisItemsSources`] edits, apply them to per-element
    /// [`ObservableCollection`](dm_noesis_runtime::binding::ObservableCollection)s
    /// (creating one per `x:Name` on first use), then bind any unbound
    /// collection to its element's `ItemsSource`. The apply step is independent
    /// of the scene (the collection holds the data regardless); binding waits
    /// until the named element exists and re-binds after a scene rebuild.
    pub(crate) fn apply_items_sources(&mut self, requests: &NoesisItemsSources) {
        for (name, op) in requests.drain() {
            crate::items::apply_op(&mut self.items_sources, name, op);
        }
        if self.items_sources.is_empty() {
            return;
        }
        let Some(scene) = self.scene.as_ref() else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        let uri = scene.built_for_uri.clone();
        for (name, binding) in &mut self.items_sources {
            if !binding.needs_bind(&uri) {
                continue;
            }
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisItemsSources: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if element.set_items_source(binding.collection()) {
                binding.mark_bound(&uri);
            } else {
                warn!(
                    "NoesisItemsSources: element {:?} is not an ItemsControl; \
                     set_items_source skipped",
                    name,
                );
            }
        }
    }

    /// Eagerly register every `(folder, filename)` pair currently in
    /// [`SharedFontMap`] that we haven't already handed to the C++
    /// `CachedFontProvider`. Called both before scene build (so the
    /// initial population is in place when XAML's first font lookup
    /// runs) and on every render-app sync (so fonts that arrive after
    /// scene build are picked up before they're ever requested).
    ///
    /// This bypasses Noesis's lazy `ScanFolder` model, which only fires
    /// once per folder and then caches its result — making any face that
    /// wasn't loaded at scan time permanently invisible without this.
    fn register_pending_fonts(&mut self) {
        let Some(registered) = self.registered_fonts.as_ref() else {
            return;
        };
        let pairs: Vec<(String, String)> = {
            let guard = self.shared_fonts.0.lock().expect("SharedFontMap poisoned");
            guard
                .keys()
                .filter(|pair| !self.registered_faces.contains(*pair))
                .cloned()
                .collect()
        };
        if pairs.is_empty() {
            return;
        }
        for (folder, filename) in pairs {
            registered.register_font(&folder, &filename);
            self.registered_faces.insert((folder, filename));
        }
    }

    /// Run once, the first time our font map holds any entries. Installs
    /// the Noesis font fallback chain and default properties so unstyled
    /// elements get a real font. Guarded by a flag because
    /// `SetFontFallbacks` is a process-global Noesis setting and Noesis
    /// caches its scan of each folder forever after first call.
    fn install_font_fallbacks_if_needed(&mut self, fallbacks: &[String]) {
        if self.fallbacks_installed {
            return;
        }
        let has_fonts = {
            let guard = self.shared_fonts.0.lock().expect("SharedFontMap poisoned");
            !guard.is_empty()
        };
        if !has_fonts {
            return;
        }
        let refs: Vec<&str> = fallbacks.iter().map(String::as_str).collect();
        dm_noesis_runtime::font_provider::set_font_fallbacks(&refs);
        // WPF defaults (size=12, weight=Normal=400, stretch=Normal=5,
        // style=Normal=0).
        dm_noesis_runtime::font_provider::set_font_default_properties(12.0, 400, 5, 0);
        info!(
            "NoesisRenderState: font fallbacks installed: {:?}",
            fallbacks,
        );
        self.fallbacks_installed = true;
    }

    fn shared_map(&self) -> &SharedXamlMap {
        &self.shared_map
    }

    fn shared_fonts(&self) -> &SharedFontMap {
        &self.shared_fonts
    }

    fn shared_images(&self) -> &SharedImageMap {
        &self.shared_images
    }

    /// Build (or rebuild) the scene instance if the configured URI has
    /// bytes in the shared map and the current instance (if any) is stale.
    /// No-op when the XAML hasn't arrived yet.
    fn ensure_scene(&mut self, config: &NoesisScene) {
        if config.xaml_uri.is_empty() {
            self.teardown_scene();
            return;
        }

        // Same URI, different size — resize in place without tearing down
        // the View. Rebuild just the intermediate texture; `View::set_size`
        // informs Noesis without invalidating the renderer. Important for
        // desktop window drags, which fire `WindowResized` at every pixel.
        if let Some(scene) = self.scene.as_mut()
            && scene.built_for_uri == config.xaml_uri
            && scene.size != config.size
        {
            scene.view.set_size(config.size.x, config.size.y);
            let (tex, render_view, sample_view) = create_intermediate(&self.device, config.size);
            scene.intermediate = tex;
            scene.intermediate_view = render_view;
            scene.intermediate_sample_view = sample_view;
            scene.size = config.size;
            return;
        }

        let up_to_date = self
            .scene
            .as_ref()
            .is_some_and(|s| s.built_for_uri == config.xaml_uri && s.size == config.size);
        if up_to_date {
            return;
        }

        self.teardown_scene();

        // Confirm the XAML is currently present; skip if not.
        {
            let guard = self.shared_map.0.lock().expect("SharedXamlMap poisoned");
            if !guard.contains_key(&config.xaml_uri) {
                return;
            }
        }
        // Defer scene creation until `wait_for_fonts` is satisfied (or
        // never set). Noesis's `CachedFontProvider` caches the result of
        // `ScanFolder` the first time it's called — if we build the View
        // before fonts have loaded, Noesis sees an empty folder and
        // never rescans, so all text renders invisible.
        for folder in &config.wait_for_fonts {
            let guard = self.shared_fonts.0.lock().expect("SharedFontMap poisoned");
            let have = guard.keys().any(|(f, _)| f == folder);
            if !have {
                return;
            }
        }
        for (folder, filename) in &config.wait_for_font_files {
            let guard = self.shared_fonts.0.lock().expect("SharedFontMap poisoned");
            let have = guard.contains_key(&(folder.clone(), filename.clone()));
            if !have {
                return;
            }
        }
        // Same problem as fonts but for textures: Noesis caches a
        // missing-texture lookup the first time GetTextureInfo returns
        // empty, and never asks again. Wait for every URI the scene
        // references to be in `SharedImageMap` before letting the View
        // construct.
        for uri in &config.wait_for_images {
            let guard = self
                .shared_images
                .0
                .lock()
                .expect("SharedImageMap poisoned");
            if !guard.contains_key(uri) {
                return;
            }
        }
        // If the scene references application resources (a theme),
        // wait for every URI in the chain to reach the provider —
        // otherwise the chain installer's `SetSource` calls hit empty
        // bytes and the leaf stays empty. Loop guards against any one
        // late-arriving URI delaying scene build for the rest.
        {
            let guard = self.shared_map.0.lock().expect("SharedXamlMap poisoned");
            for uri in &config.application_resources {
                if !guard.contains_key(uri) {
                    return;
                }
            }
        }

        // Eagerly register every font currently in `SharedFontMap` with
        // the C++ `CachedFontProvider` before XAML parsing runs. Noesis's
        // own lazy `ScanFolder` model fires once per folder during
        // `SetFontFallbacks` (or first font lookup) and caches its
        // result; pre-populating the cache here means a `FontFamily`
        // referenced from XAML resolves regardless of whether its file
        // happened to be loaded by scan time.
        self.register_pending_fonts();

        // Fonts are in the map now; this is the safe moment to install the
        // fallback chain. Noesis will immediately invoke our provider's
        // `scan_folder("Fonts")`; any face we haven't already eagerly
        // registered above gets picked up by the scan callback.
        self.install_font_fallbacks_if_needed(&config.font_fallbacks);

        // Application resources must be installed BEFORE the scene's XAML
        // is parsed, or the scene's `<Style TargetType="Button">` can't
        // resolve theme brushes. One-shot; re-configuring the URI later
        // would currently require a process restart.
        self.install_application_resources_if_needed(config);

        let Some(element) = FrameworkElement::load(&config.xaml_uri) else {
            warn!(
                "FrameworkElement::load({:?}) returned None despite bytes being in the registry",
                config.xaml_uri,
            );
            return;
        };
        let mut view = View::create(element);
        view.set_size(config.size.x, config.size.y);
        view.set_scale(config.scale);
        let initial_flags = flags_from(config);
        view.set_flags(initial_flags);
        // Intentionally do NOT call SetProjectionMatrix. Noesis derives the
        // primitive→clip projection from `DeviceCaps` (clip_space_y_inverted,
        // depth_range_zero_to_one); supplying an OpenGL-style ortho here makes
        // its render-tree visibility pass cull child elements (verified in
        // tests/headless_xaml_nested.rs). The reference IntegrationGLUT sample
        // also never calls SetProjectionMatrix.

        // `activate()` is required for Noesis to route keyboard input; the
        // view is created deactivated, so pre-seed it here. Focus loss is
        // picked up later via the NoesisInputEvent::Focus pipeline.
        view.activate();

        let (intermediate, intermediate_view, intermediate_sample_view) =
            create_intermediate(&self.device, config.size);
        info!(
            "NoesisRenderState: scene built — view + intermediate at {}x{} for uri {:?}",
            config.size.x, config.size.y, config.xaml_uri,
        );

        self.scene = Some(SceneInstance {
            view,
            renderer_initialized: false,
            intermediate,
            intermediate_view,
            intermediate_sample_view,
            size: config.size,
            built_for_uri: config.xaml_uri.clone(),
            applied_flags: initial_flags,
            applied_scale: config.scale,
            click_subs: HashMap::new(),
            keydown_subs: HashMap::new(),
            text_snapshots: HashMap::new(),
        });
    }

    /// Reconcile the active `BaseButton::Click` subscription set against
    /// `watch`. Adds a subscription for any name not already wired,
    /// drops subscriptions whose name has been removed from `watch`.
    /// No-op when the scene hasn't been built — subscriptions install on
    /// the first frame the View exists.
    /// Apply pending visibility writes, then clear the queue. Each entry
    /// is `(x:Name, visible)`; missing names log a warning once per drain
    /// and are otherwise skipped. Idempotent — calling with an empty
    /// queue is a no-op.
    pub(crate) fn apply_visibility_requests(&mut self, requests: &SharedVisibilityQueue) {
        let pending = requests.drain();
        if pending.is_empty() {
            return;
        }
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, visible) in pending {
            let Some(mut element) = content.find_name(&name) else {
                warn!(
                    "NoesisVisibility: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            element.set_visibility(visible);
        }
    }

    pub(crate) fn apply_layout_requests(&mut self, requests: &crate::layout::SharedLayoutQueue) {
        let pending = requests.drain();
        if pending.is_empty() {
            return;
        }
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, [left, top, right, bottom]) in pending {
            let Some(mut element) = content.find_name(&name) else {
                warn!(
                    "NoesisLayout: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            element.set_margin(left, top, right, bottom);
        }
    }

    pub(crate) fn sync_click_subscriptions(&mut self, watch: &[String], queue: &SharedClickQueue) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };

        // Drop subscriptions that are no longer requested. `retain` runs
        // each entry's drop in place, which fires the C++ unsubscribe.
        scene.click_subs.retain(|k, _| watch.iter().any(|w| w == k));

        // Add new subscriptions. Pull the View's content tree once per
        // frame; `find_name` is cheap but the FFI hop isn't free.
        let needs_new = watch.iter().any(|n| !scene.click_subs.contains_key(n));
        if !needs_new {
            return;
        }
        let Some(content) = scene.view.content() else {
            return;
        };
        for name in watch {
            if scene.click_subs.contains_key(name) {
                continue;
            }
            let Some(element) = content.find_name(name) else {
                warn!(
                    "NoesisClickWatch: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let queue_handle = queue.clone();
            let captured_name = name.clone();
            let Some(sub) = subscribe_click(&element, move || {
                queue_handle.push(captured_name.clone());
            }) else {
                warn!(
                    "NoesisClickWatch: element {:?} is not a BaseButton; skipping",
                    name,
                );
                continue;
            };
            scene.click_subs.insert(name.clone(), sub);
        }
    }

    /// Reconcile the active `UIElement::KeyDown` subscription set against
    /// `entries`. Mirrors [`Self::sync_click_subscriptions`] — adds /
    /// drops subscriptions to match the desired watch list. The
    /// per-entry `swallow` set is captured by the closure so each
    /// callback can mark `out_handled = true` for keys the watcher
    /// wants to suppress propagation on.
    pub(crate) fn sync_keydown_subscriptions(
        &mut self,
        entries: &[crate::events::KeyDownWatchEntry],
        queue: &SharedKeyDownQueue,
    ) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };

        scene
            .keydown_subs
            .retain(|k, _| entries.iter().any(|e| e.name == *k));

        // Always re-bind every entry. Swallow lists may change between
        // frames and the C++-side handler captured them at subscription
        // time, so we can't update in place — we drop and re-create.
        // Cheap: a single FFI ref-bump + delegate add per entry, only on
        // the frames the watch actually changes.
        for entry in entries {
            // If the existing subscription's swallow set matches the
            // requested one, leave it alone. We track this on the Bevy
            // side via a sibling map keyed by name.
            if scene.keydown_subs.contains_key(&entry.name)
                && self
                    .last_keydown_swallow
                    .get(&entry.name)
                    .is_some_and(|prev| prev == &entry.swallow)
            {
                continue;
            }

            let Some(content) = scene.view.content() else {
                return;
            };
            let Some(element) = content.find_name(&entry.name) else {
                warn!(
                    "NoesisKeyDownWatch: x:Name {:?} not found in scene {:?}",
                    entry.name, scene.built_for_uri,
                );
                continue;
            };

            let queue_handle = queue.clone();
            let captured_name = entry.name.clone();
            let swallow = entry.swallow.clone();
            let Some(sub) = subscribe_keydown(&element, move |key: Key| {
                queue_handle.push(captured_name.clone(), key);
                swallow.contains(&key)
            }) else {
                warn!(
                    "NoesisKeyDownWatch: element {:?} is not a UIElement; skipping",
                    entry.name,
                );
                continue;
            };
            scene.keydown_subs.insert(entry.name.clone(), sub);
            self.last_keydown_swallow
                .insert(entry.name.clone(), entry.swallow.clone());
        }

        // Prune swallow snapshots whose name is no longer watched.
        self.last_keydown_swallow
            .retain(|k, _| entries.iter().any(|e| e.name == *k));
    }

    /// Apply pending text writes from [`crate::text::NoesisTextRequests`].
    /// Each entry is `(x:Name, text)`; missing names log a warning, type
    /// mismatches (target isn't a TextBox/TextBlock) likewise. Idempotent
    /// on an empty queue.
    pub(crate) fn apply_text_writes(&mut self, requests: &SharedTextWriteQueue) {
        let pending = requests.drain();
        if pending.is_empty() {
            return;
        }
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, text) in pending {
            let Some(mut element) = content.find_name(&name) else {
                warn!(
                    "NoesisText: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if !element.set_text(&text) {
                warn!(
                    "NoesisText: element {:?} is not a TextBox/TextBlock; \
                     set_text skipped",
                    name,
                );
                continue;
            }
            // Update the snapshot eagerly so the next read pass doesn't
            // emit a phantom NoesisTextChanged event for a write we just
            // pushed ourselves.
            scene.text_snapshots.insert(name, text);
        }
    }

    /// Apply pending geometry writes from
    /// [`crate::geometry::NoesisGeometryRequests`]. Mirrors
    /// [`Self::apply_text_writes`]: drains the queue, looks up each named
    /// element, and assigns its `Path` geometry. Idempotent on empty.
    pub(crate) fn apply_geometry_writes(&mut self, requests: &SharedGeometryQueue) {
        let pending = requests.drain();
        if pending.is_empty() {
            return;
        }
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, points) in pending {
            let Some(mut element) = content.find_name(&name) else {
                warn!(
                    "NoesisGeometry: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if !element.set_path_points(&points) {
                warn!(
                    "NoesisGeometry: element {:?} is not a Path (or < 2 points); \
                     set_path_points skipped",
                    name,
                );
            }
        }
    }

    /// Apply pending focus requests from
    /// [`crate::focus::NoesisFocusRequests`]. Mirrors
    /// [`Self::apply_visibility_requests`]: drains the queue, looks up
    /// each named element, calls `Focus()`. Idempotent on empty.
    pub(crate) fn apply_focus_requests(&mut self, requests: &SharedFocusQueue) {
        let pending = requests.drain();
        if pending.is_empty() {
            return;
        }
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for name in pending {
            let Some(mut element) = content.find_name(&name) else {
                warn!(
                    "NoesisFocus: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if !element.focus() {
                warn!(
                    "NoesisFocus: element {:?} refused focus (non-focusable?)",
                    name,
                );
            }
        }
    }

    /// Poll text values for every name in `watched`, and push a
    /// `(name, text)` pair onto `queue` whenever the value differs from
    /// the previous frame's snapshot. Cheap when nothing's changed —
    /// one `find_name` + one text getter per watched name per frame.
    /// Names dropped from the watch get pruned out of the snapshot map.
    pub(crate) fn poll_text_reads(&mut self, watched: &[String], queue: &SharedTextChangedQueue) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        scene
            .text_snapshots
            .retain(|k, _| watched.iter().any(|w| w == k));

        if watched.is_empty() {
            return;
        }
        let Some(content) = scene.view.content() else {
            return;
        };
        for name in watched {
            let Some(element) = content.find_name(name) else {
                continue;
            };
            let current = element.text().unwrap_or_default();
            match scene.text_snapshots.get(name) {
                Some(prev) if prev == &current => continue,
                _ => {}
            }
            scene.text_snapshots.insert(name.clone(), current.clone());
            queue.push(name.clone(), current);
        }
    }

    /// Reapply per-frame tweakables that don't require a scene rebuild (the PPAA
    /// flag and the DPI scale). Called every frame before Noesis is driven.
    /// Cheap: a compare per knob; each FFI call only fires on change.
    fn apply_live_flags(&mut self, config: &NoesisScene) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let desired = flags_from(config);
        if desired != scene.applied_flags {
            scene.view.set_flags(desired);
            scene.applied_flags = desired;
        }
        // Exact dedup against the last-applied value (like `applied_flags`): the
        // caller already quantizes scale, so any difference is a real change.
        #[allow(clippy::float_cmp)]
        if config.scale != scene.applied_scale {
            scene.view.set_scale(config.scale);
            scene.applied_scale = config.scale;
        }
    }

    fn install_application_resources_if_needed(&mut self, config: &NoesisScene) {
        if config.application_resources.is_empty() {
            return;
        }
        if self
            .loaded_app_resources_chain
            .as_ref()
            .is_some_and(|loaded| loaded == &config.application_resources)
        {
            return;
        }
        // Every URI in the chain must already be in the provider map
        // — the chain installer's `SetSource` calls fire synchronously
        // and resolve through the same provider.
        {
            let guard = self.shared_map.0.lock().expect("SharedXamlMap poisoned");
            for uri in &config.application_resources {
                if !guard.contains_key(uri) {
                    return; // wait for the registry to pick it up
                }
            }
        }
        if dm_noesis_runtime::gui::install_app_resources_chain(&config.application_resources) {
            info!(
                "Installed Noesis application resources chain ({} entries): {:?}",
                config.application_resources.len(),
                config.application_resources,
            );
            self.loaded_app_resources_chain = Some(config.application_resources.clone());
        } else {
            warn!(
                "install_app_resources_chain returned false for {:?}",
                config.application_resources,
            );
        }
    }

    /// Apply a batch of queued input events onto the live View. No-op when
    /// the scene hasn't been built yet; events are lost in that case, which
    /// is fine — pre-scene input targets nothing.
    fn apply_input(&mut self, events: &[crate::input::NoesisInputEvent]) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        use crate::input::NoesisInputEvent as E;
        for ev in events {
            match *ev {
                E::MouseMove { x, y } => {
                    let _ = scene.view.mouse_move(x, y);
                }
                E::MouseButton {
                    down: true,
                    x,
                    y,
                    button,
                } => {
                    let _ = scene.view.mouse_button_down(x, y, button);
                }
                E::MouseButton {
                    down: false,
                    x,
                    y,
                    button,
                } => {
                    let _ = scene.view.mouse_button_up(x, y, button);
                }
                E::MouseWheel { x, y, delta } => {
                    let _ = scene.view.mouse_wheel(x, y, delta);
                }
                E::Scroll {
                    x,
                    y,
                    value,
                    horizontal: false,
                } => {
                    let _ = scene.view.scroll(x, y, value);
                }
                E::Scroll {
                    x,
                    y,
                    value,
                    horizontal: true,
                } => {
                    let _ = scene.view.hscroll(x, y, value);
                }
                E::TouchDown { x, y, id } => {
                    let _ = scene.view.touch_down(x, y, id);
                }
                E::TouchMove { x, y, id } => {
                    let _ = scene.view.touch_move(x, y, id);
                }
                E::TouchUp { x, y, id } => {
                    let _ = scene.view.touch_up(x, y, id);
                }
                E::KeyDown(k) => {
                    let _ = scene.view.key_down(k);
                }
                E::KeyUp(k) => {
                    let _ = scene.view.key_up(k);
                }
                E::Char(cp) => {
                    let _ = scene.view.char_input(cp);
                }
                E::Focus(true) => scene.view.activate(),
                E::Focus(false) => scene.view.deactivate(),
            }
        }
    }

    /// Drive one Noesis frame into the intermediate. Call during the
    /// `Render` schedule — before `NoesisNode::run` — so the intermediate
    /// is populated when the node blits.
    fn drive_frame(&mut self) {
        let time_secs = self.clock_origin.elapsed().as_secs_f64();
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let registered_device = self
            .registered_device
            .as_mut()
            .expect("registered_device dropped mid-frame");

        // Lazy renderer init — needs both the live View and the registered
        // device, so it happens on first frame here (not at scene creation).
        if !scene.renderer_initialized {
            let mut renderer = scene.view.renderer();
            renderer.init(registered_device);
            // Don't call renderer.shutdown() here — keep the init live.
            scene.renderer_initialized = true;
        }

        // Point the device at the intermediate for this frame's onscreen phase.
        registered_device
            .device_mut::<WgpuRenderDevice>()
            .set_onscreen_target(scene.intermediate_view.clone());

        let _changed = scene.view.update(time_secs);
        let mut renderer = scene.view.renderer();
        // UpdateRenderTree latches the latest view snapshot into the
        // renderer. `render_offscreen` populates any render-target ramps
        // /glyphs / shadows first; then `render` paints the onscreen
        // (intermediate) target. `clear=true` — we own the intermediate.
        let _ = renderer.update_render_tree();
        let _ = renderer.render_offscreen();
        renderer.render(false, true);
        // WgpuRenderDevice auto-submits at end_onscreen_render, so the
        // intermediate is ready to sample by the time the graph runs.
    }

    /// Render label template `xaml_uri` — with `fields` applied to named
    /// `TextBlock`/`TextBox` elements — into `target` at `size`, reusing one
    /// persistent offscreen rig view. Mirrors [`Self::drive_frame`] but points
    /// the device at the caller's texture instead of an intermediate, and never
    /// blits to a camera.
    ///
    /// Returns `false` when prerequisites aren't ready yet (the font fallback
    /// chain isn't installed, or the template's bytes haven't reached the XAML
    /// provider), so the caller can retry on a later frame.
    pub(crate) fn bake_into(
        &mut self,
        target: &wgpu::TextureView,
        xaml_uri: &str,
        size: UVec2,
        fields: &[(String, String)],
    ) -> bool {
        // Gate on fonts: the live scene installs the process-global fallback
        // chain once fonts load (see `install_font_fallbacks_if_needed`).
        // Baking before that point would rasterize invisible text.
        if !self.fallbacks_installed {
            return false;
        }

        let needs_build = self
            .bake_rig
            .as_ref()
            .is_none_or(|rig| rig.built_for_uri != xaml_uri);
        if needs_build {
            {
                let guard = self.shared_map.0.lock().expect("SharedXamlMap poisoned");
                if !guard.contains_key(xaml_uri) {
                    return false;
                }
            }
            self.teardown_bake_rig();
            let Some(element) = FrameworkElement::load(xaml_uri) else {
                warn!("bake_into: FrameworkElement::load({xaml_uri:?}) returned None");
                return false;
            };
            let mut view = View::create(element);
            view.set_size(size.x, size.y);
            view.set_scale(1.0);
            view.activate();
            self.bake_rig = Some(BakeRig {
                view,
                renderer_initialized: false,
                built_for_uri: xaml_uri.to_string(),
                size,
            });
        }

        let time_secs = self.clock_origin.elapsed().as_secs_f64();
        let registered_device = self
            .registered_device
            .as_mut()
            .expect("registered_device dropped mid-bake");
        let rig = self.bake_rig.as_mut().expect("rig built above");

        if let Some(content) = rig.view.content() {
            for (name, text) in fields {
                if let Some(mut element) = content.find_name(name) {
                    element.set_text(text);
                } else {
                    warn!("bake_into: x:Name {name:?} not found in {xaml_uri:?}");
                }
            }
        }
        if rig.size != size {
            rig.view.set_size(size.x, size.y);
            rig.size = size;
        }

        if !rig.renderer_initialized {
            let mut renderer = rig.view.renderer();
            renderer.init(registered_device);
            rig.renderer_initialized = true;
        }

        registered_device
            .device_mut::<WgpuRenderDevice>()
            .set_onscreen_target(target.clone());

        let _ = rig.view.update(time_secs);
        let mut renderer = rig.view.renderer();
        let _ = renderer.update_render_tree();
        let _ = renderer.render_offscreen();
        renderer.render(false, true);
        true
    }

    fn teardown_bake_rig(&mut self) {
        let Some(mut rig) = self.bake_rig.take() else {
            return;
        };
        if rig.renderer_initialized {
            // Must run while the registered device is still alive.
            rig.view.renderer().shutdown();
        }
        drop(rig);
    }

    fn teardown_scene(&mut self) {
        // View models + items sources outlive the scene; mark them detached so
        // the next apply/attach pass re-binds them against the rebuilt view.
        for entry in &mut self.view_models {
            entry.reset_attach();
        }
        for binding in self.items_sources.values_mut() {
            binding.reset_bind();
        }
        let Some(mut scene) = self.scene.take() else {
            return;
        };
        if scene.renderer_initialized {
            // Must run while the registered device is still alive.
            scene.view.renderer().shutdown();
        }
        drop(scene);
    }
}

impl Drop for NoesisRenderState {
    fn drop(&mut self) {
        // Release view-model instances + registrations first, while Noesis is
        // still initialized and the registered device/providers are alive.
        // (They don't need the device, but releasing them deterministically
        // here keeps teardown ordering obvious.)
        self.view_models.clear();
        self.items_sources.clear();
        self.teardown_scene();
        self.teardown_bake_rig();
        drop(self.registered_device.take());
        drop(self.registered_provider.take());
        drop(self.registered_fonts.take());
        drop(self.registered_textures.take());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Blit pipeline — stretch the intermediate onto ViewTarget
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) struct BlitPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// Premultiplied-alpha "over": `result = src.rgb + dst.rgb * (1 - src.a)`. Used
/// by the Core3d overlay node to composite the UI *directly* onto the camera's
/// finished scene — transparent intermediate texels (a == 0) leave the scene
/// intact, anti-aliased edges (premultiplied by Noesis) blend correctly.
const PREMULTIPLIED_OVER: wgpu::BlendState = wgpu::BlendState {
    color: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
    alpha: wgpu::BlendComponent {
        src_factor: wgpu::BlendFactor::One,
        dst_factor: wgpu::BlendFactor::OneMinusSrcAlpha,
        operation: wgpu::BlendOperation::Add,
    },
};

impl BlitPipeline {
    /// `blend = None` overwrites the target 1:1 (Core2d path, where Bevy's own
    /// multi-camera step alpha-composites the result); `Some(PREMULTIPLIED_OVER)`
    /// composites onto an existing image (Core3d overlay path).
    fn new(
        device: &wgpu::Device,
        target_format: wgpu::TextureFormat,
        blend: Option<wgpu::BlendState>,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("noesis blit shader"),
            source: wgpu::ShaderSource::Wgsl(Cow::Borrowed(BLIT_WGSL)),
        });

        let bind_group_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("noesis blit bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("noesis blit sampler"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("noesis blit pipeline layout"),
            bind_group_layouts: &[&bind_group_layout],
            push_constant_ranges: &[],
        });

        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("noesis blit pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format: target_format,
                    // Overwrite (None) for the Core2d path — Noesis paints the
                    // whole intermediate and Bevy's multi-camera composite folds
                    // it over the 3D camera using the alpha channel. The Core3d
                    // overlay path passes `Some(PREMULTIPLIED_OVER)` to composite
                    // straight onto the scene instead.
                    blend,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            multiview: None,
            cache: None,
        });

        Self {
            pipeline,
            bind_group_layout,
            sampler,
        }
    }

    fn bind_group(&self, device: &wgpu::Device, src: &wgpu::TextureView) -> wgpu::BindGroup {
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("noesis blit bg"),
            layout: &self.bind_group_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(src),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&self.sampler),
                },
            ],
        })
    }
}

const BLIT_WGSL: &str = r"
struct VertexOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
}

@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOut {
    // Fullscreen triangle covering NDC [-1, 3] x [-1, 3] clipped to viewport.
    let x = f32((idx << 1u) & 2u);
    let y = f32(idx & 2u);
    var out: VertexOut;
    out.pos = vec4<f32>(x * 2.0 - 1.0, 1.0 - y * 2.0, 0.0, 1.0);
    out.uv  = vec2<f32>(x, y);
    return out;
}

@group(0) @binding(0) var src_texture: texture_2d<f32>;
@group(0) @binding(1) var src_sampler: sampler;

@fragment
fn fs_main(in: VertexOut) -> @location(0) vec4<f32> {
    return textureSample(src_texture, src_sampler, in.uv);
}
";

#[derive(Resource, Default)]
pub(crate) struct BlitPipelineCache {
    /// 1:1 overwrite — the Core2d (separate UI camera) path.
    overwrite: HashMap<wgpu::TextureFormat, BlitPipeline>,
    /// Premultiplied-alpha "over" — the Core3d (overlay-on-scene) path.
    over: HashMap<wgpu::TextureFormat, BlitPipeline>,
}

impl BlitPipelineCache {
    fn get_overwrite(&self, format: wgpu::TextureFormat) -> Option<&BlitPipeline> {
        self.overwrite.get(&format)
    }

    fn get_over(&self, format: wgpu::TextureFormat) -> Option<&BlitPipeline> {
        self.over.get(&format)
    }

    fn ensure(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        self.overwrite
            .entry(format)
            .or_insert_with(|| BlitPipeline::new(device, format, None));
        self.over
            .entry(format)
            .or_insert_with(|| BlitPipeline::new(device, format, Some(PREMULTIPLIED_OVER)));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems (render app)
// ─────────────────────────────────────────────────────────────────────────────

/// Build [`NoesisRenderState`] the first frame. Now that `NoesisRenderState`
/// is `Send + Sync` (via unsafe-impls in `dm_noesis_runtime` for the View/Renderer/
/// Registered wrappers), it can live as a regular `Resource` and systems
/// here use idiomatic `Res`/`ResMut` params.
#[allow(clippy::needless_pass_by_value)]
fn init_noesis_render_state(
    mut commands: Commands,
    existing: Option<Res<NoesisRenderState>>,
    render_device: Res<RenderDevice>,
    queue: Res<RenderQueue>,
) {
    if existing.is_some() {
        return;
    }
    let device = render_device.wgpu_device().clone();
    let queue = (**queue.0).clone();
    commands.insert_resource(NoesisRenderState::new(device, queue));
}

/// Copy the extracted [`XamlRegistry`] into the [`SharedXamlMap`] backing
/// [`BevyXamlProvider`]. Runs on the render thread after the built-in
/// extract system has already populated the render-world copy of
/// [`XamlRegistry`].
#[allow(clippy::needless_pass_by_value)]
fn sync_xaml_provider_map(
    registry: Option<Res<XamlRegistry>>,
    state: Option<Res<NoesisRenderState>>,
) {
    let (Some(registry), Some(state)) = (registry, state) else {
        return;
    };
    state.shared_map().sync_from(&registry);
}

/// Copy the extracted [`FontRegistry`] into the [`SharedFontMap`] backing
/// [`BevyFontProvider`], then eagerly register any newly-arrived fonts
/// with the C++ `CachedFontProvider` cache. Mirrors
/// [`sync_xaml_provider_map`] for fonts but with the extra eager-register
/// step — fonts that finish loading after scene build get picked up here
/// before any later XAML lookup (or `FontFamily` change at runtime) tries
/// to resolve them.
#[allow(clippy::needless_pass_by_value)]
fn sync_font_provider_map(
    registry: Option<Res<FontRegistry>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(registry), Some(mut state)) = (registry, state) else {
        return;
    };
    state.shared_fonts().sync_from(&registry);
    state.register_pending_fonts();
}

/// Copy the extracted [`ImageRegistry`] into the [`SharedImageMap`]
/// backing [`BevyTextureProvider`]. Mirrors the XAML / font sync
/// systems.
#[allow(clippy::needless_pass_by_value)]
fn sync_texture_provider_map(
    registry: Option<Res<ImageRegistry>>,
    state: Option<Res<NoesisRenderState>>,
) {
    let (Some(registry), Some(state)) = (registry, state) else {
        return;
    };
    state.shared_images().sync_from(&registry);
}

/// Ensure a live [`SceneInstance`] exists for the configured URI once its
/// bytes land in the shared map.
#[allow(clippy::needless_pass_by_value)]
fn ensure_noesis_scene(config: Option<Res<NoesisScene>>, state: Option<ResMut<NoesisRenderState>>) {
    let (Some(config), Some(mut state)) = (config, state) else {
        return;
    };
    state.ensure_scene(&config);
}

/// Re-apply the per-frame live settings on [`NoesisScene`] (currently
/// just PPAA). Cheap — compares one `u32` and only fires an FFI call on
/// change.
#[allow(clippy::needless_pass_by_value)]
fn apply_live_scene_flags(
    config: Option<Res<NoesisScene>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(config), Some(mut state)) = (config, state) else {
        return;
    };
    state.apply_live_flags(&config);
}

/// Drain [`NoesisInputQueue`] onto the live View. Runs after
/// [`ensure_noesis_scene`] — so the scene exists — and before
/// [`drive_noesis_frame`] — so `View::Update` picks up the state these
/// events produced (hover highlights, button presses, etc.).
#[allow(clippy::needless_pass_by_value)]
fn apply_noesis_input(
    queue: Option<Res<crate::input::NoesisInputQueue>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(queue), Some(mut state)) = (queue, state) else {
        return;
    };
    if queue.events.is_empty() {
        return;
    }
    state.apply_input(&queue.events);
}

/// Drive Noesis for the frame — layout, update render tree, render into
/// the intermediate. Runs after [`ensure_noesis_scene`] and before the
/// graph executes.
#[allow(clippy::needless_pass_by_value)]
fn drive_noesis_frame(state: Option<ResMut<NoesisRenderState>>) {
    let Some(mut state) = state else {
        return;
    };
    state.drive_frame();
}

/// Build a blit pipeline per-ViewTarget-format encountered. Cheap miss +
/// no-op on hit.
#[allow(clippy::needless_pass_by_value)]
fn prepare_noesis_blit(
    render_device: Res<RenderDevice>,
    targets: Query<&ViewTarget>,
    mut cache: ResMut<BlitPipelineCache>,
) {
    for target in &targets {
        cache.ensure(render_device.wgpu_device(), target.main_texture_format());
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// NoesisNode — blits the intermediate into ViewTarget
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct NoesisNodeLabel;

/// Marks the camera Noesis composites its UI onto. Add this to the camera —
/// `Camera2d` or `Camera3d` — whose final image the UI should overlay.
///
/// The blit runs *inside that camera's* render graph (Core2d or Core3d), after
/// its post-processing, so it composes cleanly with whatever the camera does
/// (HDR, image-based lighting, bloom, DOF, …). Crucially it does **not** rely on
/// a second window-targeting camera sharing the 3D camera's `ViewTarget`, which
/// breaks the moment the host adds standard 3D features. Tag exactly the
/// camera(s) you want the UI on; untagged cameras (e.g. offscreen effect passes)
/// are skipped.
#[derive(Component, ExtractComponent, Clone, Copy, Default, Debug)]
pub struct NoesisCamera;

/// Shared blit body for both nodes. `overlay = false` overwrites the target 1:1
/// (Core2d, separate UI camera — Bevy composites it afterwards); `overlay = true`
/// premultiplied-alpha composites the UI straight onto the camera's finished
/// scene (Core3d, single-camera).
fn blit_noesis_ui(
    render_context: &mut RenderContext<'_>,
    view_target: &ViewTarget,
    world: &World,
    overlay: bool,
) -> Result<(), NodeRunError> {
    let Some(state) = world.get_resource::<NoesisRenderState>() else {
        return Ok(());
    };
    let Some(scene) = state.scene.as_ref() else {
        return Ok(());
    };
    if !scene.renderer_initialized {
        return Ok(());
    }

    let Some(cache) = world.get_resource::<BlitPipelineCache>() else {
        return Ok(());
    };
    let target_format = view_target.main_texture_format();
    let blit = if overlay {
        cache.get_over(target_format)
    } else {
        cache.get_overwrite(target_format)
    };
    let Some(blit) = blit else {
        // prepare_noesis_blit should have created it; if it hasn't by
        // now we'd draw garbage so skip cleanly.
        return Ok(());
    };

    // Choose how to sample Noesis's sRGB-encoded intermediate bytes so the
    // composite is correct for the camera's ViewTarget colour space:
    //
    // - sRGB target (`Rgba8UnormSrgb`): the write path applies linear→sRGB,
    //   so sample through the sRGB alias (sRGB→linear on read). The encode
    //   undoes the decode and the stored bytes round-trip exactly.
    // - HDR/float target (`Rgba16Float`, …): values are *linear*
    //   scene-referred and get tonemapped + sRGB-encoded downstream on the
    //   way to the swapchain. So, like the sRGB case, the UI's sRGB bytes
    //   must be decoded to linear on read — otherwise they're treated as
    //   linear and encoded a second time, washing the UI out too bright.
    // - plain `Rgba8Unorm`: written and displayed raw, so sample the raw
    //   view and skip the gamma decode.
    let decode_srgb = target_format.is_srgb() || is_linear_float(target_format);
    let sample_view = if decode_srgb {
        &scene.intermediate_sample_view
    } else {
        &scene.intermediate_view
    };
    let bg = blit.bind_group(render_context.render_device().wgpu_device(), sample_view);

    let encoder = render_context.command_encoder();
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("NoesisNode blit"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: view_target.main_texture_view(),
            resolve_target: None,
            depth_slice: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
    });
    pass.set_pipeline(&blit.pipeline);
    pass.set_bind_group(0, &bg, &[]);
    pass.draw(0..3, 0..1);
    drop(pass);

    Ok(())
}

/// Core2d compositing node — runs on **every** Core2d view (unchanged, classic
/// behaviour). A 2D UI camera layered over the scene relies on Bevy's own
/// multi-camera step to fold this overwrite blit over the lower camera.
#[derive(Default)]
pub struct NoesisNode;

impl ViewNode for NoesisNode {
    type ViewQuery = &'static ViewTarget;

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        view_target: &'w ViewTarget,
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        blit_noesis_ui(render_context, view_target, world, false)
    }
}

#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct NoesisOverlayNodeLabel;

/// Core3d overlay node — runs only on views tagged [`NoesisCamera`], compositing
/// the UI premultiplied-alpha over the camera's finished scene. This is the
/// single-camera path that stays robust when the host adds IBL/bloom/DOF.
#[derive(Default)]
pub struct NoesisOverlayNode;

impl ViewNode for NoesisOverlayNode {
    type ViewQuery = (&'static ViewTarget, &'static NoesisCamera);

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        (view_target, _marker): (&'w ViewTarget, &'w NoesisCamera),
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        blit_noesis_ui(render_context, view_target, world, true)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

pub struct NoesisRenderPlugin;

impl Plugin for NoesisRenderPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins((
            ExtractResourcePlugin::<NoesisScene>::default(),
            // The blit node reads `NoesisCamera` on the render-world view entity.
            ExtractComponentPlugin::<NoesisCamera>::default(),
        ));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            warn!("RenderApp not present; NoesisRenderPlugin is a no-op");
            return;
        };

        render_app
            .init_resource::<BlitPipelineCache>()
            .add_systems(
                Render,
                (
                    init_noesis_render_state,
                    sync_xaml_provider_map,
                    sync_font_provider_map,
                    sync_texture_provider_map,
                    ensure_noesis_scene,
                    apply_live_scene_flags,
                    apply_noesis_input,
                    drive_noesis_frame,
                    prepare_noesis_blit,
                )
                    .chain()
                    .in_set(RenderSystems::Prepare),
            )
            // Core2d: classic overwrite blit on every 2D view (unchanged). A
            // host that overlays UI with a `Camera2d` keeps working as before.
            .add_render_graph_node::<ViewNodeRunner<NoesisNode>>(Core2d, NoesisNodeLabel)
            .add_render_graph_edges(
                Core2d,
                (
                    Node2d::MainTransparentPass,
                    NoesisNodeLabel,
                    Node2d::EndMainPass,
                ),
            )
            // Core3d: opt-in overlay on views tagged `NoesisCamera`, composited
            // after all 3D post-processing (tonemapping, bloom, the host's own
            // passes) and before upscaling — so it survives IBL/bloom/DOF and
            // needs no second window-targeting camera.
            .add_render_graph_node::<ViewNodeRunner<NoesisOverlayNode>>(
                Core3d,
                NoesisOverlayNodeLabel,
            )
            .add_render_graph_edges(
                Core3d,
                (
                    Node3d::EndMainPassPostProcessing,
                    NoesisOverlayNodeLabel,
                    Node3d::Upscaling,
                ),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::is_linear_float;
    use wgpu::TextureFormat;

    #[test]
    fn hdr_float_targets_decode_srgb() {
        // HDR camera ViewTargets store linear values (encoded downstream), so the
        // blit must treat them like sRGB targets and decode on sample.
        assert!(is_linear_float(TextureFormat::Rgba16Float));
        assert!(is_linear_float(TextureFormat::Rgba32Float));
        assert!(is_linear_float(TextureFormat::Rg11b10Ufloat));
    }

    #[test]
    fn ldr_unorm_targets_sample_raw() {
        // Plain 8-bit targets are written/displayed raw — no gamma decode. (sRGB
        // targets are handled separately via TextureFormat::is_srgb.)
        assert!(!is_linear_float(TextureFormat::Rgba8Unorm));
        assert!(!is_linear_float(TextureFormat::Bgra8Unorm));
        assert!(!is_linear_float(TextureFormat::Rgba8UnormSrgb));
    }
}

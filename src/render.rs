//! Render-graph integration for Noesis (Phase 4.D.3).
//!
//! [`NoesisRenderPlugin`] is a sub-plugin on [`RenderApp`] that:
//!
//! - Builds a [`WgpuRenderDevice`] against Bevy's shared `wgpu::Device` and
//!   registers it with Noesis in [`Plugin::finish`].
//! - Installs a [`BevyXamlProvider`] whose backing [`SharedXamlMap`] is
//!   refreshed each frame from the main world's [`XamlRegistry`] via a
//!   system running in [`ExtractSchedule`].
//! - Lazily builds a [`noesis_runtime::view::View`] + intermediate `Rgba8Unorm`
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
//! [`Registered`]: noesis_runtime::render_device::Registered

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};

use bevy::core_pipeline::core_2d::graph::{Core2d, Node2d};
use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};
use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_component::{ExtractComponent, ExtractComponentPlugin},
    render_graph::{
        NodeRunError, RenderGraphContext, RenderGraphExt, RenderLabel, ViewNode, ViewNodeRunner,
    },
    renderer::{RenderContext, RenderDevice, RenderQueue},
    view::ViewTarget,
};
use noesis_runtime::animation::{Animation, DoubleAnimation, Timeline};
use noesis_runtime::commands::Command;
use noesis_runtime::events::{
    ClickSubscription, EventArgs, EventSubscription, KeyDownSubscription, subscribe_click,
    subscribe_event, subscribe_keydown,
};
use noesis_runtime::input::KeyBinding;
use noesis_runtime::transforms::CompositeTransform;
use noesis_runtime::view::{FrameworkElement, Key, View};

use crate::commands::{CommandEntry, CommandsDef, SharedCommandQueue};
use crate::events::{SharedClickQueue, SharedKeyDownQueue};
use crate::font::{BevyFontProvider, FontRegistry, SharedFontMap};
use crate::image::{BevyTextureProvider, ImageRegistry, SharedImageMap};
use crate::items::ItemsBinding;
use crate::plain_vm::PlainVmEntry;
use crate::render_device::WgpuRenderDevice;
use crate::routed_events::{RoutedEventSnapshot, SharedRoutedEventQueue};
use crate::viewmodel::{AttachTarget, SharedVmChangedQueue, ViewModelDef, VmEntry, VmValue};
use crate::xaml::{BevyXamlProvider, SharedXamlMap, XamlRegistry};

/// Color format of the per-view intermediate Noesis paints into. Must match
/// the private `RT_COLOR_FORMAT` in `render_device::wgpu_device`; keeping
/// the coupling documented rather than sharing the const cross-module.
const INTERMEDIATE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

fn flags_from(config: &NoesisView) -> u32 {
    let mut f = 0;
    if config.ppaa {
        f |= noesis_runtime::view::RenderFlag::Ppaa as u32;
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

/// One double-buffer slot: a Noesis-painted texture plus its two views (a raw
/// `Rgba8Unorm` render view + an `Rgba8UnormSrgb` alias for sampling).
struct Intermediate {
    // Owned so the GPU allocation outlives the views; not read directly.
    #[allow(dead_code)]
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    sample_view: wgpu::TextureView,
}

fn create_intermediate(device: &wgpu::Device, size: UVec2) -> Intermediate {
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
    Intermediate {
        texture: tex,
        view: render_view,
        sample_view,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Public configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view scene configuration. Add as a [`Component`] to the camera entity
/// you also tag with [`NoesisCamera`]; the render app receives a copy on that
/// entity via [`ExtractComponent`]. One [`NoesisView`] == one live Noesis
/// `View` + intermediate, composited onto that camera. Multiple tagged
/// cameras drive multiple independent views.
#[derive(Component, ExtractComponent, Clone, Debug)]
pub struct NoesisView {
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
    /// [`RenderFlag::Ppaa`]: noesis_runtime::view::RenderFlag::Ppaa
    pub ppaa: bool,
    /// `ResourceDictionary` URIs to install as the process-global
    /// application resources (styles, brushes, `ControlTemplate`s), in
    /// dependency order. Each URI must resolve via the same XAML
    /// provider that serves `xaml_uri`. Loaded once on first scene
    /// build; later changes are ignored.
    ///
    /// The plugin uses
    /// [`noesis_runtime::gui::install_app_resources_chain`] — an
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

impl Default for NoesisView {
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

/// **Non-send** render-world resource: the runtime's `View`/`Renderer`/device
/// handles are `!Send`/`!Sync` (`NonNull`-based), so this cannot be a regular
/// Bevy `Resource`. Inserted via `World::insert_non_send_resource` and accessed
/// through `NonSend`/`NonSendMut`; it stays pinned to the render thread, which
/// is where every Noesis call must happen. See the lifecycle invariants in
/// `CLAUDE.md`.
pub(crate) struct NoesisRenderState {
    device: wgpu::Device,
    shared_map: SharedXamlMap,
    shared_fonts: SharedFontMap,
    shared_images: SharedImageMap,
    // `Option` so `Drop` can take + drop each in the right order.
    registered_device: Option<noesis_runtime::render_device::Registered>,
    registered_provider: Option<noesis_runtime::xaml_provider::Registered>,
    registered_fonts: Option<noesis_runtime::font_provider::Registered>,
    registered_textures: Option<noesis_runtime::texture_provider::Registered>,
    /// Live scenes keyed by the render-world view entity (the camera carrying
    /// [`NoesisView`] + [`NoesisCamera`]). Built/torn down per entity by
    /// [`Self::ensure_scene`]; the blit node looks each one up by its view
    /// entity. Empty until the first `NoesisView`'s XAML resolves.
    scenes: HashMap<Entity, SceneInstance>,
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
    last_keydown_swallow: HashMap<(Entity, String), Vec<Key>>,
    /// Last `(mark_handled, handled_too)` flags installed per subscribed
    /// `(view, x:Name, event name)`. The routed-event callback captures these
    /// by value, so a flag change can't be patched in place — we detect it
    /// here and drop + re-create the subscription. Mirrors
    /// [`Self::last_keydown_swallow`]. Keyed across views; pruned per view at
    /// sync time.
    last_event_config: HashMap<(Entity, String, &'static str), (bool, bool)>,
    /// Reusable offscreen view for baking label panels (lazily built). Lives
    /// here so it shares the single registered device and its renderer is torn
    /// down before the device drops — see [`Drop`].
    bake_rig: Option<BakeRig>,
    /// Live Rust-owned view models (TODO §3 binding bridge). Each owns a
    /// `ClassInstance` + `ClassRegistration` and is attached as a scene
    /// element's `DataContext`. Stored here (not in [`SceneInstance`]) so a VM
    /// survives scene rebuilds — the attach pass re-binds it to the new view.
    /// Released in [`Drop`] before the registered device, while Noesis is still
    /// initialized. Keyed by view entity. See [`crate::viewmodel`].
    view_models: HashMap<Entity, VmEntry>,
    /// Rust-owned `ItemsSource` collections keyed by `x:Name` (TODO §3). Each
    /// owns an `ObservableCollection` bound to a named `ItemsControl`. Like
    /// [`Self::view_models`] they outlive scene rebuilds (re-bound by the apply
    /// pass) and are released in [`Drop`] before the registered device. See
    /// [`crate::items`]. Keyed by `(view entity, x:Name)` so each view owns its
    /// list collections.
    items_sources: HashMap<(Entity, String), ItemsBinding>,
    /// Rust-owned plain-struct view models keyed by view entity + component
    /// `TypeId` (TODO §3/§9). Each owns a reflected `PlainVmClass` + instance
    /// bound as that view's element `DataContext`. Same rebuild/teardown rules as
    /// [`Self::view_models`]; released in [`Drop`] before the registered device.
    /// See [`crate::plain_vm`].
    plain_vms: HashMap<(Entity, std::any::TypeId), PlainVmEntry>,
    /// Live Rust-owned command hosts (`ICommand` bridge). Each owns a
    /// `ClassInstance` + `ClassRegistration` + the per-command `Command`
    /// objects, bound as a scene element's `DataContext`. Outlives scene
    /// rebuilds (re-bound by the attach pass) and is released in `Drop`
    /// before the registered device. Keyed by view entity. See
    /// [`crate::commands`].
    command_hosts: HashMap<Entity, CommandEntry>,
}

struct SceneInstance {
    view: View,
    renderer_initialized: bool,
    /// Double-buffered intermediates. Each frame the main thread paints
    /// `intermediates[write_index]`, publishes it as the view's
    /// [`NoesisIntermediate`], then flips `write_index`. With Bevy's
    /// 1-frame-deep pipelined rendering, the render thread blits frame N's
    /// buffer while the main thread paints frame N+1 into the *other* buffer —
    /// so the two never touch the same texture and the composite can't tear.
    intermediates: [Intermediate; 2],
    write_index: usize,
    size: UVec2,
    built_for_uri: String,
    /// Last render flags written to the view via `View::set_flags`.
    /// Re-applied only when [`NoesisView`] changes; avoids the FFI call
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
    /// Active generic `RoutedEvent` subscriptions keyed by `(x:Name, event
    /// name)` — one element may be watched for several events. Synced each
    /// frame against [`crate::routed_events::NoesisEventWatch`] by
    /// [`NoesisRenderState::sync_event_subscriptions_for`]. Drops with the
    /// scene; same orphan-safety rules as `click_subs` / `keydown_subs`. The
    /// `&'static str` half is `RoutedEvent::as_str()` (the enum is not `Hash`,
    /// its stable name is).
    event_subs: HashMap<(String, &'static str), EventSubscription>,
    /// Last text snapshot per name in [`crate::text::NoesisTextReadWatch`].
    /// Used to dedupe `NoesisTextChanged` emissions — only push when the
    /// text actually differs from the previous frame's snapshot. Names
    /// removed from the watch get pruned out of this map at sync time.
    text_snapshots: HashMap<String, String>,
    /// Last value snapshot per `(x:Name, property)` in
    /// [`crate::dp::NoesisDpReadWatch`]. Same dedupe role as
    /// [`Self::text_snapshots`] but for arbitrary typed DPs; lives in the
    /// scene so it resets on rebuild.
    dp_snapshots: HashMap<(String, String), crate::dp::DpValue>,
    /// `CompositeTransform` handles assigned as elements' `RenderTransform` by
    /// [`crate::transforms::NoesisTransform`], keyed by `x:Name`. Each is held at
    /// +1 so it stays alive while it is the element's live transform; it is the
    /// *same* object Noesis stores (assignment `AddRef`'s our pointer, it does not
    /// clone), so reading it back reflects the element's true transform. Drops
    /// with the scene; reassigning a name replaces (and releases) the old one.
    transform_handles: HashMap<String, CompositeTransform>,
    /// Last [`TransformSpec`](crate::transforms::TransformSpec) snapshot per
    /// `x:Name`, to dedupe [`crate::transforms::NoesisTransformChanged`]
    /// emissions. Resets on scene rebuild.
    transform_snapshots: HashMap<String, crate::transforms::TransformSpec>,
    /// Last brush read back per `(x:Name, property)` painted by
    /// [`crate::brushes::NoesisBrushes`]. Dedupes [`crate::brushes::NoesisBrushChanged`]
    /// emissions; resets on scene rebuild.
    brush_snapshots: HashMap<(String, String), crate::brushes::BrushReadback>,
    /// Last typed font value per `(x:Name, field)` watched by
    /// [`crate::typography::NoesisTypography`]. Dedupes `NoesisTypographyChanged`
    /// emissions; resets on scene rebuild. Distinct from [`Self::dp_snapshots`]
    /// because enum font DPs need the typed getters, not the generic `i32` read.
    typo_snapshots:
        HashMap<(String, crate::typography::TypographyField), crate::typography::TypographyValue>,
    /// Installed `KeyBinding`s keyed by `(x:Name, key ordinal, modifier
    /// bits)`, synced against [`crate::focus_input::NoesisFocusControl::bindings`].
    /// Drops with the scene; cannot be detached mid-life (no Noesis remove API).
    input_bindings: HashMap<(String, i32, i32), InstalledKeyBinding>,
    /// Last `(candidate, matches_expected)` per [`crate::focus_input::FocusPredict`]
    /// ident, to dedupe [`crate::focus_input::NoesisFocusPredicted`] emissions.
    /// Resets on scene rebuild.
    predict_snapshots: HashMap<(String, i32, Option<String>), (bool, bool)>,
}

/// One installed `KeyBinding`. Holds the `Command` *and* the `KeyBinding` at +1
/// so neither is released while the binding lives in the element's
/// `InputBindings`. Dropping it (scene teardown) releases our references; the
/// binding is NOT detached from the element — Noesis exposes no remove (see the
/// `focus_input` NOTES). Kept keyed so we never double-install the same chord.
#[allow(dead_code)] // held only to keep the +1 references alive.
struct InstalledKeyBinding {
    command: Command,
    binding: KeyBinding,
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
        let registered_device = noesis_runtime::render_device::register(wgpu_rd);
        let xaml_prov = BevyXamlProvider::from_shared(shared_map.clone());
        let registered_provider = noesis_runtime::xaml_provider::set_xaml_provider(xaml_prov);
        let font_prov = BevyFontProvider::from_shared(shared_fonts.clone());
        let registered_fonts = noesis_runtime::font_provider::set_font_provider(font_prov);
        let texture_prov = BevyTextureProvider::from_shared(shared_images.clone());
        let registered_textures =
            noesis_runtime::texture_provider::set_texture_provider(texture_prov);

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
            scenes: HashMap::new(),
            fallbacks_installed: false,
            registered_faces: HashSet::new(),
            loaded_app_resources_chain: None,
            clock_origin: std::time::Instant::now(),
            last_keydown_swallow: HashMap::new(),
            last_event_config: HashMap::new(),
            bake_rig: None,
            view_models: HashMap::new(),
            items_sources: HashMap::new(),
            plain_vms: HashMap::new(),
            command_hosts: HashMap::new(),
        }
    }

    // Migration scaffolding (Phase 1a step ii): the name-keyed bridges
    // (visibility/layout/text/dp/click/keydown/focus/geometry/view-models/items)
    // still target the first live scene via `self.scenes.values().next()`
    // inline. They become per-view-entity components in step ii; until then a
    // single `NoesisView` is the supported configuration for those bridges.

    /// Drain pending view-model registrations from [`NoesisViewModels`]: for
    /// each, register the Noesis class, instantiate it, and wire its change
    /// forwarder to `changed`. Builds happen render-side because Noesis objects
    /// are thread-affine to the `View`. Idempotent on an empty queue.
    /// Build view `entity`'s [`VmEntry`] on first sight (register the Noesis
    /// class, instantiate, wire the entity-tagged change forwarder). No-op if it
    /// already exists. Main-thread only.
    pub(crate) fn ensure_view_model(
        &mut self,
        entity: Entity,
        def: &ViewModelDef,
        changed: &SharedVmChangedQueue,
    ) {
        if self.view_models.contains_key(&entity) {
            return;
        }
        match VmEntry::build(entity, def, changed) {
            Some(entry) => {
                self.view_models.insert(entity, entry);
            }
            None => warn!(
                "NoesisViewModel: failed to register/instantiate class {:?} (duplicate name?)",
                def.class_name(),
            ),
        }
    }

    /// Apply queued writes to view `entity`'s view-model instance. Unknown
    /// property names log a warning. No-op when the entry isn't built yet.
    pub(crate) fn apply_view_model_writes_for(
        &mut self,
        entity: Entity,
        writes: &[(String, VmValue)],
    ) {
        let Some(entry) = self.view_models.get(&entity) else {
            return;
        };
        for (prop, value) in writes {
            if !entry.write(prop, value) {
                warn!("NoesisViewModel: view {entity:?} has no property {prop:?}");
            }
        }
    }

    /// Attach any not-yet-attached view model as its target's `DataContext` in
    /// its own view's scene. No-op until that view (and any named target) exists;
    /// retries each frame, and re-attaches after a scene rebuild.
    pub(crate) fn attach_view_models(&mut self) {
        for (&entity, entry) in &mut self.view_models {
            let Some(scene) = self.scenes.get(&entity) else {
                continue;
            };
            let Some(content) = scene.view.content() else {
                continue;
            };
            let uri = &scene.built_for_uri;
            if !entry.needs_attach(uri) {
                continue;
            }
            let target = match entry.target() {
                AttachTarget::Root => scene.view.content(),
                AttachTarget::Named(name) => content.find_name(name),
            };
            let Some(mut element) = target else {
                warn!(
                    "NoesisViewModel: attach target for view {:?} not found in scene {:?}",
                    entity, scene.built_for_uri,
                );
                continue;
            };
            if element.set_data_context(entry.instance()) {
                entry.mark_attached(uri);
            } else {
                warn!("NoesisViewModel: set_data_context returned false for view {entity:?}");
            }
        }
    }

    /// Build view `entity`'s [`CommandEntry`] on first sight (register the Noesis
    /// command-host class, instantiate, build a `Command` per declared name tagged
    /// with `entity` and pushing to `queue`). No-op if it already exists.
    /// Main-thread only.
    pub(crate) fn ensure_commands(
        &mut self,
        entity: Entity,
        def: &CommandsDef,
        queue: &SharedCommandQueue,
    ) {
        if self.command_hosts.contains_key(&entity) {
            return;
        }
        match CommandEntry::build(entity, def, queue) {
            Some(entry) => {
                self.command_hosts.insert(entity, entry);
            }
            None => warn!(
                "NoesisCommands: failed to register/instantiate class {:?} (duplicate name?)",
                def.class_name(),
            ),
        }
    }

    /// Apply queued enabled-state edits to view `entity`'s command host. Unknown
    /// command names log a warning. No-op when the host isn't built yet.
    pub(crate) fn apply_command_enables_for(&mut self, entity: Entity, enables: &[(String, bool)]) {
        let Some(entry) = self.command_hosts.get(&entity) else {
            return;
        };
        for (name, value) in enables {
            if !entry.set_enabled(name, *value) {
                warn!("NoesisCommands: view {entity:?} has no command {name:?}");
            }
        }
    }

    /// Attach any not-yet-attached command host as its target's `DataContext` in
    /// its own view's scene. No-op until that view (and any named target) exists;
    /// retries each frame, and re-attaches after a scene rebuild. Mirrors
    /// [`Self::attach_view_models`].
    pub(crate) fn attach_commands(&mut self) {
        for (&entity, entry) in &mut self.command_hosts {
            let Some(scene) = self.scenes.get(&entity) else {
                continue;
            };
            let Some(content) = scene.view.content() else {
                continue;
            };
            let uri = &scene.built_for_uri;
            if !entry.needs_attach(uri) {
                continue;
            }
            let target = match entry.target() {
                AttachTarget::Root => scene.view.content(),
                AttachTarget::Named(name) => content.find_name(name),
            };
            let Some(mut element) = target else {
                warn!(
                    "NoesisCommands: attach target for view {:?} not found in scene {:?}",
                    entity, scene.built_for_uri,
                );
                continue;
            };
            if element.set_data_context(entry.instance()) {
                entry.mark_attached(uri);
            } else {
                warn!("NoesisCommands: set_data_context returned false for view {entity:?}");
            }
        }
    }

    /// Drain pending [`NoesisItemsSources`] edits, apply them to per-element
    /// [`ObservableCollection`](noesis_runtime::binding::ObservableCollection)s
    /// (creating one per `x:Name` on first use), then bind any unbound
    /// collection to its element's `ItemsSource`. The apply step is independent
    /// of the scene (the collection holds the data regardless); binding waits
    /// until the named element exists and re-binds after a scene rebuild.
    /// Reconcile view `entity`'s [`NoesisItems`] component. When `changed`, set
    /// each named element's collection to the desired item list (creating a
    /// collection per `(entity, name)` on first use, pruning names no longer
    /// present). Every frame, bind any unbound collection to its element's
    /// `ItemsSource` — handles first resolution and re-binding after a rebuild.
    pub(crate) fn apply_items_for(
        &mut self,
        entity: Entity,
        sources: &HashMap<String, Vec<String>>,
        changed: bool,
    ) {
        if changed {
            // Prune this view's collections whose name was removed.
            self.items_sources
                .retain(|(ent, name), _| *ent != entity || sources.contains_key(name));
            for (name, items) in sources {
                self.items_sources
                    .entry((entity, name.clone()))
                    .or_default()
                    .set(items);
            }
        }

        let Some(scene) = self.scenes.get(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        let uri = scene.built_for_uri.clone();
        for ((ent, name), binding) in &mut self.items_sources {
            if *ent != entity || !binding.needs_bind(&uri) {
                continue;
            }
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisItems: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if element.set_items_source(binding.collection()) {
                binding.mark_bound(&uri);
            } else {
                warn!("NoesisItems: element {name:?} is not an ItemsControl; skipped");
            }
        }
    }

    /// Reconcile view `entity`'s plain-struct view model of type `type_id`:
    /// register + instantiate on first sight, apply a pending field snapshot
    /// (Rust→UI), attach it as its target's `DataContext` once the view + element
    /// exist (re-attaching after a rebuild), and return any queued two-way edits
    /// (UI→Rust) for the caller to apply back into the component. The metadata is
    /// passed in (not generic) so one method serves every VM type. See
    /// [`crate::plain_vm`].
    pub(crate) fn sync_plain_vm(
        &mut self,
        entity: Entity,
        type_id: std::any::TypeId,
        type_name: &str,
        props: &[(&'static str, crate::plain_vm::PlainType)],
        target: &AttachTarget,
        snapshot: Option<Vec<crate::plain_vm::PlainValue>>,
    ) -> Vec<(u32, crate::plain_vm::PlainValue)> {
        let key = (entity, type_id);
        if let std::collections::hash_map::Entry::Vacant(slot) = self.plain_vms.entry(key) {
            let Some(entry) = PlainVmEntry::build(type_name, props, target.clone()) else {
                warn!(
                    "NoesisViewModel: failed to register plain VM {type_name:?} (duplicate name?)",
                );
                return Vec::new();
            };
            slot.insert(entry);
        }

        if let (Some(entry), Some(snapshot)) = (self.plain_vms.get(&key), snapshot) {
            entry.apply_snapshot(&snapshot);
        }

        // Attach as DataContext once this view's scene + target element exist;
        // re-attach after a rebuild. Disjoint field borrows (`scenes` vs
        // `plain_vms`) keep the borrow checker happy.
        if let Some(scene) = self.scenes.get(&entity)
            && let Some(content) = scene.view.content()
        {
            let uri = scene.built_for_uri.clone();
            if let Some(entry) = self.plain_vms.get_mut(&key)
                && entry.needs_attach(&uri)
            {
                let element = match entry.target() {
                    AttachTarget::Root => scene.view.content(),
                    AttachTarget::Named(name) => content.find_name(name),
                };
                match element {
                    Some(mut element) => {
                        if !entry.attach_to(&mut element, &uri) {
                            warn!(
                                "NoesisViewModel: set_data_context returned false for \
                                 {type_name:?} (target not a FrameworkElement?)",
                            );
                        }
                    }
                    None => warn!(
                        "NoesisViewModel: attach target for {type_name:?} not found in scene {:?}",
                        scene.built_for_uri,
                    ),
                }
            }
        }

        self.plain_vms
            .get(&key)
            .map(PlainVmEntry::drain_writebacks)
            .unwrap_or_default()
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
        noesis_runtime::font_provider::set_font_fallbacks(&refs);
        // WPF defaults (size=12, weight=Normal=400, stretch=Normal=5,
        // style=Normal=0).
        noesis_runtime::font_provider::set_font_default_properties(12.0, 400, 5, 0);
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
    fn ensure_scene(&mut self, entity: Entity, config: &NoesisView) {
        if config.xaml_uri.is_empty() {
            self.teardown_scene(entity);
            return;
        }

        // Same URI, different size — resize in place without tearing down
        // the View. Rebuild just the intermediate texture; `View::set_size`
        // informs Noesis without invalidating the renderer. Important for
        // desktop window drags, which fire `WindowResized` at every pixel.
        if let Some(scene) = self.scenes.get_mut(&entity)
            && scene.built_for_uri == config.xaml_uri
            && scene.size != config.size
        {
            scene.view.set_size(config.size.x, config.size.y);
            scene.intermediates = [
                create_intermediate(&self.device, config.size),
                create_intermediate(&self.device, config.size),
            ];
            scene.size = config.size;
            return;
        }

        let up_to_date = self
            .scenes
            .get(&entity)
            .is_some_and(|s| s.built_for_uri == config.xaml_uri && s.size == config.size);
        if up_to_date {
            return;
        }

        self.teardown_scene(entity);

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

        let intermediates = [
            create_intermediate(&self.device, config.size),
            create_intermediate(&self.device, config.size),
        ];
        info!(
            "NoesisRenderState: scene built — view + intermediate at {}x{} for uri {:?}",
            config.size.x, config.size.y, config.xaml_uri,
        );

        self.scenes.insert(
            entity,
            SceneInstance {
                view,
                renderer_initialized: false,
                intermediates,
                write_index: 0,
                size: config.size,
                built_for_uri: config.xaml_uri.clone(),
                applied_flags: initial_flags,
                applied_scale: config.scale,
                click_subs: HashMap::new(),
                keydown_subs: HashMap::new(),
                event_subs: HashMap::new(),
                text_snapshots: HashMap::new(),
                dp_snapshots: HashMap::new(),
                transform_handles: HashMap::new(),
                transform_snapshots: HashMap::new(),
                brush_snapshots: HashMap::new(),
                typo_snapshots: HashMap::new(),
                input_bindings: HashMap::new(),
                predict_snapshots: HashMap::new(),
            },
        );
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
    /// Apply view `entity`'s desired element visibility (`x:Name → visible`).
    /// No-op until the scene exists; missing names warn.
    pub(crate) fn apply_visibility_for(&mut self, entity: Entity, desired: &HashMap<String, bool>) {
        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, &visible) in desired {
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisVisibility: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            element.set_visibility(visible);
        }
    }

    /// Apply view `entity`'s desired element margins (`x:Name → [l, t, r, b]`).
    pub(crate) fn apply_layout_for(&mut self, entity: Entity, desired: &HashMap<String, [f32; 4]>) {
        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, &[left, top, right, bottom]) in desired {
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisLayout: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            element.set_margin(left, top, right, bottom);
        }
    }

    /// Apply view `entity`'s desired visual-state transitions
    /// (`x:Name → (state, use_transitions)`) via `VisualStateManager::GoToState`.
    /// No-op until the scene exists; missing names warn, and a failed transition
    /// (non-templated element, or unknown state) warns too.
    pub(crate) fn apply_visual_state_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, (String, bool)>,
    ) {
        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, (state, use_transitions)) in desired {
            // `go_to_state` takes `&self`, so no `mut` binding needed.
            let Some(element) = content.find_name(name) else {
                warn!(
                    "NoesisVisualState: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if !element.go_to_state(state, *use_transitions) {
                warn!(
                    "NoesisVisualState: GoToState({state:?}) failed for {name:?} \
                     in scene {:?} (not a templated control, or unknown state)",
                    scene.built_for_uri,
                );
            }
        }
    }

    /// Begin view `entity`'s requested code-built animations
    /// (`x:Name → AnimationSpec`) via `Animation::begin_on` against each named
    /// element's scalar dependency property. No-op until the scene exists;
    /// missing names warn, and a failed begin (unknown / non-`float` property,
    /// or a disconnected target) warns too. Re-begin replaces any clock already
    /// running on the same property (`SnapshotAndReplace`).
    pub(crate) fn begin_animations_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, crate::animation::AnimationSpec>,
    ) {
        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, spec) in desired {
            let Some(element) = content.find_name(name) else {
                warn!(
                    "NoesisAnimation: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let mut anim = DoubleAnimation::new();
            // From/To/Duration return false only on a type/read-only mismatch,
            // impossible on a freshly-created DoubleAnimation — ignore them.
            let _ = anim.set_from(spec.from);
            let _ = anim.set_to(Some(spec.to));
            let _ = anim.set_duration_secs(spec.duration_secs);
            if !anim.begin_on(
                &element,
                &spec.property,
                noesis_runtime::animation::HandoffBehavior::SnapshotAndReplace,
            ) {
                warn!(
                    "NoesisAnimation: begin_on({:?}) failed for {name:?} in scene {:?} \
                     (unknown / non-float property, or disconnected target)",
                    spec.property, scene.built_for_uri,
                );
            }
        }
    }

    /// Reconcile view `entity`'s `BaseButton::Click` subscriptions against its
    /// [`NoesisClickWatch`] component's names. Each callback pushes
    /// `(entity, name)` so the emitted [`NoesisClicked`] carries the view.
    pub(crate) fn sync_click_subscriptions_for(
        &mut self,
        entity: Entity,
        watch: &[String],
        queue: &SharedClickQueue,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
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
                queue_handle.push(entity, captured_name.clone());
            }) else {
                warn!("NoesisClickWatch: element {name:?} is not a BaseButton; skipping");
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
    pub(crate) fn sync_keydown_subscriptions_for(
        &mut self,
        entity: Entity,
        entries: &[crate::events::KeyDownWatchEntry],
        queue: &SharedKeyDownQueue,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
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
                    .get(&(entity, entry.name.clone()))
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
                queue_handle.push(entity, captured_name.clone(), key);
                swallow.contains(&key)
            }) else {
                warn!(
                    "NoesisKeyDownWatch: element {:?} is not a UIElement; skipping",
                    entry.name
                );
                continue;
            };
            scene.keydown_subs.insert(entry.name.clone(), sub);
            self.last_keydown_swallow
                .insert((entity, entry.name.clone()), entry.swallow.clone());
        }

        // Prune this view's swallow snapshots whose name is no longer watched
        // (leave other views' entries intact).
        self.last_keydown_swallow
            .retain(|(ent, name), _| *ent != entity || entries.iter().any(|e| &e.name == name));
    }

    /// Reconcile view `entity`'s generic `RoutedEvent` subscriptions against
    /// `entries`. Mirrors [`Self::sync_keydown_subscriptions_for`] — adds /
    /// drops subscriptions to match the desired watch list, and re-binds an
    /// entry whose `(mark_handled, handled_too)` flags changed (the callback
    /// captures them by value). Each callback snapshots the live args and pushes
    /// `(entity, name, event, snapshot)` so the emitted [`NoesisRoutedEvent`]
    /// carries the originating view.
    pub(crate) fn sync_event_subscriptions_for(
        &mut self,
        entity: Entity,
        entries: &[crate::routed_events::EventWatchEntry],
        queue: &SharedRoutedEventQueue,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };

        // Drop subscriptions that are no longer requested. `retain` runs each
        // entry's drop in place, which fires the C++ unsubscribe.
        scene.event_subs.retain(|(name, evname), _| {
            entries
                .iter()
                .any(|e| e.name == *name && e.event.as_str() == *evname)
        });

        for entry in entries {
            let evname = entry.event.as_str();
            let key = (entry.name.clone(), evname);

            // Leave an existing subscription alone iff its captured flags still
            // match the requested ones (sibling map keyed by view + name + event).
            if scene.event_subs.contains_key(&key)
                && self
                    .last_event_config
                    .get(&(entity, entry.name.clone(), evname))
                    .is_some_and(|prev| *prev == (entry.mark_handled, entry.handled_too))
            {
                continue;
            }

            // Pull the content tree per change. `find_name` is cheap but the FFI
            // hop isn't free, so we only reach here on the frames the watch moved.
            let Some(content) = scene.view.content() else {
                return;
            };
            let Some(element) = content.find_name(&entry.name) else {
                warn!(
                    "NoesisEventWatch: x:Name {:?} not found in scene {:?}",
                    entry.name, scene.built_for_uri,
                );
                continue;
            };

            let queue_handle = queue.clone();
            let captured_name = entry.name.clone();
            let captured_event = entry.event;
            let mark_handled = entry.mark_handled;
            let Some(sub) = subscribe_event(
                &element,
                entry.event,
                entry.handled_too,
                move |args: &EventArgs| {
                    let snapshot = RoutedEventSnapshot::capture(args);
                    queue_handle.push(entity, captured_name.clone(), captured_event, snapshot);
                    mark_handled
                },
            ) else {
                warn!(
                    "NoesisEventWatch: element {:?} not a UIElement / event {:?} unknown; skipping",
                    entry.name, evname,
                );
                continue;
            };

            // Replace any stale sub (flag change) — drop runs the C++ unsubscribe.
            scene.event_subs.insert(key, sub);
            self.last_event_config.insert(
                (entity, entry.name.clone(), evname),
                (entry.mark_handled, entry.handled_too),
            );
        }

        // Prune this view's config snapshots whose (name, event) is no longer
        // watched (leave other views' entries intact).
        self.last_event_config.retain(|(ent, name, evname), _| {
            *ent != entity
                || entries
                    .iter()
                    .any(|e| &e.name == name && e.event.as_str() == *evname)
        });
    }

    /// Write each `(x:Name → text)` desired by view `entity`'s [`NoesisText`]
    /// component onto that view's elements. Missing names / non-text targets log
    /// a warning. No-op until the view's scene exists.
    pub(crate) fn apply_text_writes_for(
        &mut self,
        entity: Entity,
        set: &std::collections::HashMap<String, String>,
    ) {
        if set.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, text) in set {
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisText: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if !element.set_text(text) {
                warn!("NoesisText: element {name:?} is not a TextBox/TextBlock; set_text skipped");
                continue;
            }
            // Update the snapshot eagerly so the read pass doesn't emit a phantom
            // NoesisTextChanged for a write we just made ourselves.
            scene.text_snapshots.insert(name.clone(), text.clone());
        }
    }

    /// Apply each `(x:Name → FontStyling)` desired by view `entity`'s
    /// [`NoesisTypography`](crate::typography::NoesisTypography) component onto
    /// that view's `TextElement`s. Each block's `Some` fields are written; `None`
    /// fields are skipped. Missing names log a warning; a field a target doesn't
    /// expose is logged at debug. No-op until the view's scene exists.
    pub(crate) fn apply_typography_for(
        &mut self,
        entity: Entity,
        set: &HashMap<String, crate::typography::FontStyling>,
    ) {
        if set.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, styling) in set {
            if styling.is_empty() {
                continue;
            }
            let Some(element) = content.find_name(name) else {
                warn!(
                    "NoesisTypography: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if let Some(size) = styling.font_size
                && !noesis_runtime::typography::set_font_size(&element, size)
            {
                debug!("NoesisTypography: {name:?} did not accept FontSize");
            }
            if let Some(source) = &styling.font_family {
                // A fresh FontFamily holds one +1 ref; set_font_family AddRefs on
                // the Noesis side, so the handle can drop at scope end.
                let family = noesis_runtime::typography::FontFamily::new(source);
                if !noesis_runtime::typography::set_font_family(&element, &family) {
                    debug!("NoesisTypography: {name:?} did not accept FontFamily");
                }
            }
            if let Some(weight) = styling.font_weight
                && !noesis_runtime::typography::set_font_weight(&element, weight)
            {
                debug!("NoesisTypography: {name:?} did not accept FontWeight");
            }
            if let Some(style) = styling.font_style
                && !noesis_runtime::typography::set_font_style(&element, style)
            {
                debug!("NoesisTypography: {name:?} did not accept FontStyle");
            }
            if let Some(stretch) = styling.font_stretch
                && !noesis_runtime::typography::set_font_stretch(&element, stretch)
            {
                debug!("NoesisTypography: {name:?} did not accept FontStretch");
            }
        }
    }

    /// Poll view `entity`'s watched typed font properties, returning
    /// `(x:Name, value)` for each that changed since last frame (deduped against
    /// the per-scene snapshot). First poll after a watch is added always reports.
    ///
    /// Uses the runtime's *typed* font getters rather than the generic DP read
    /// path: `FontWeight`/`FontStyle`/`FontStretch` are enum DPs and don't
    /// round-trip through `get_i32`.
    pub(crate) fn poll_typography_reads_for(
        &mut self,
        entity: Entity,
        watched: &[crate::typography::TypographyWatch],
    ) -> Vec<(String, crate::typography::TypographyValue)> {
        use crate::typography::{TypographyField, TypographyValue};
        use noesis_runtime::typography as ty;

        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene.typo_snapshots.retain(|(name, field), _| {
            watched.iter().any(|w| &w.name == name && w.field == *field)
        });
        if watched.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for watch in watched {
            let Some(element) = content.find_name(&watch.name) else {
                continue;
            };
            let current = match watch.field {
                TypographyField::FontSize => ty::font_size(&element).map(TypographyValue::FontSize),
                TypographyField::FontFamily => {
                    ty::get_font_family(&element).map(|f| TypographyValue::FontFamily(f.source()))
                }
                TypographyField::FontWeight => {
                    ty::font_weight(&element).map(TypographyValue::FontWeight)
                }
                TypographyField::FontStyle => {
                    ty::font_style(&element).map(TypographyValue::FontStyle)
                }
                TypographyField::FontStretch => {
                    ty::font_stretch(&element).map(TypographyValue::FontStretch)
                }
            };
            let Some(current) = current else {
                continue;
            };
            let key = (watch.name.clone(), watch.field);
            if scene.typo_snapshots.get(&key) == Some(&current) {
                continue;
            }
            scene.typo_snapshots.insert(key, current.clone());
            changed.push((watch.name.clone(), current));
        }
        changed
    }

    /// Apply pending geometry writes from
    /// [`crate::geometry::NoesisGeometryRequests`]. Mirrors
    /// [`Self::apply_text_writes`]: drains the queue, looks up each named
    /// element, and assigns its `Path` geometry. Idempotent on empty.
    /// Apply view `entity`'s desired `Path` geometries (`x:Name → points`).
    pub(crate) fn apply_geometry_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, Vec<[f32; 2]>>,
    ) {
        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, points) in desired {
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisGeometry: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if !element.set_path_points(points) {
                warn!("NoesisGeometry: element {name:?} is not a Path (or < 2 points); skipped",);
            }
        }
    }

    /// Move keyboard focus to `target` (an `x:Name`) in view `entity`, if set.
    /// Called when the view's `NoesisFocus` component changes.
    pub(crate) fn apply_focus_for(&mut self, entity: Entity, target: Option<&str>) {
        let Some(name) = target else {
            return;
        };
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        let Some(mut element) = content.find_name(name) else {
            warn!(
                "NoesisFocus: x:Name {:?} not found in scene {:?}",
                name, scene.built_for_uri,
            );
            return;
        };
        if !element.focus() {
            warn!("NoesisFocus: element {name:?} refused focus (non-focusable?)");
        }
    }

    /// Apply view `entity`'s one-shot directional / tab focus moves
    /// (`UIElement::MoveFocus`). Missing names warn; a move that didn't shift
    /// focus warns (non-traversable direction / no neighbour).
    pub(crate) fn apply_focus_moves_for(
        &mut self,
        entity: Entity,
        moves: &[crate::focus_input::FocusMove],
    ) {
        if moves.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for m in moves {
            let Some(mut element) = content.find_name(&m.from) else {
                warn!(
                    "NoesisFocusControl: move-from x:Name {:?} not found in scene {:?}",
                    m.from, scene.built_for_uri,
                );
                continue;
            };
            if !element.move_focus(m.direction, m.wrapped) {
                warn!(
                    "NoesisFocusControl: MoveFocus({:?}, wrapped={}) from {:?} moved nothing",
                    m.direction, m.wrapped, m.from,
                );
            }
        }
    }

    /// Apply view `entity`'s one-shot focus-engagement actions
    /// (`UIElement::Focus(engage)`).
    pub(crate) fn apply_focus_engages_for(
        &mut self,
        entity: Entity,
        engages: &[crate::focus_input::FocusEngage],
    ) {
        if engages.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for e in engages {
            let Some(mut element) = content.find_name(&e.name) else {
                warn!(
                    "NoesisFocusControl: engage x:Name {:?} not found in scene {:?}",
                    e.name, scene.built_for_uri,
                );
                continue;
            };
            if !element.focus_engage(e.engage) {
                warn!(
                    "NoesisFocusControl: element {:?} refused focus(engage={})",
                    e.name, e.engage,
                );
            }
        }
    }

    /// Reconcile view `entity`'s `KeyBinding`s against `specs`. Each binding's
    /// command callback pushes `(entity, name, key, modifiers)` onto `queue`, so
    /// the emitted `NoesisFocusBindingFired` carries the originating view.
    /// Bindings already installed are left alone; bindings dropped from `specs`
    /// release their `+1` references (but stay attached to the element — no
    /// Noesis remove API). Mirrors `sync_click_subscriptions_for`.
    pub(crate) fn sync_key_bindings_for(
        &mut self,
        entity: Entity,
        specs: &[crate::focus_input::KeyBindingSpec],
        queue: &crate::focus_input::SharedFocusBindingQueue,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };

        // Forget bindings no longer requested (releases our +1s in place).
        scene
            .input_bindings
            .retain(|k, _| specs.iter().any(|s| &s.ident() == k));

        let needs_new = specs
            .iter()
            .any(|s| !scene.input_bindings.contains_key(&s.ident()));
        if !needs_new {
            return;
        }
        let Some(content) = scene.view.content() else {
            return;
        };
        for spec in specs {
            let ident = spec.ident();
            if scene.input_bindings.contains_key(&ident) {
                continue;
            }
            let Some(element) = content.find_name(&spec.name) else {
                warn!(
                    "NoesisFocusControl: binding x:Name {:?} not found in scene {:?}",
                    spec.name, scene.built_for_uri,
                );
                continue;
            };

            let queue_handle = queue.clone();
            let view = entity;
            let name = spec.name.clone();
            let key = spec.key;
            let modifiers = spec.modifiers;
            // Fire-always command: pushes the chord onto the shared queue.
            let command = Command::new(move |_param| {
                queue_handle.push(view, name.clone(), key, modifiers);
            });

            let Some(binding) = KeyBinding::new(&command, spec.key, spec.modifiers) else {
                warn!(
                    "NoesisFocusControl: could not build KeyBinding for {:?} (command not an ICommand?)",
                    spec.name,
                );
                continue;
            };
            if !binding.add_to(&element) {
                warn!(
                    "NoesisFocusControl: element {:?} is not a UIElement; binding skipped",
                    spec.name,
                );
                continue;
            }
            scene
                .input_bindings
                .insert(ident, InstalledKeyBinding { command, binding });
        }
    }

    /// Poll view `entity`'s focus predictions. For each `FocusPredict` returns
    /// `(from, direction, candidate, matches_expected)` when the answer changed
    /// since last frame (deduped against the per-scene snapshot). First poll
    /// after a watch is added always reports. `matches_expected` is a safe
    /// raw-pointer identity compare of `PredictFocus`'s borrowed result against
    /// the `expect` element's pointer (no deref). Mirrors `poll_dp_reads_for`.
    pub(crate) fn poll_focus_predictions_for(
        &mut self,
        entity: Entity,
        predicts: &[crate::focus_input::FocusPredict],
    ) -> Vec<(
        String,
        crate::focus_input::FocusNavigationDirection,
        bool,
        bool,
    )> {
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene
            .predict_snapshots
            .retain(|k, _| predicts.iter().any(|p| &p.ident() == k));
        if predicts.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for p in predicts {
            let Some(from) = content.find_name(&p.from) else {
                continue;
            };
            let predicted = from.predict_focus(p.direction);
            let candidate = predicted.is_some();
            let matches_expected = match (&p.expect, predicted) {
                (Some(expect), Some(ptr)) => content
                    .find_name(expect)
                    .is_some_and(|target| target.raw() == ptr.as_ptr()),
                _ => false,
            };
            let ident = p.ident();
            if scene.predict_snapshots.get(&ident) == Some(&(candidate, matches_expected)) {
                continue;
            }
            scene
                .predict_snapshots
                .insert(ident, (candidate, matches_expected));
            changed.push((p.from.clone(), p.direction, candidate, matches_expected));
        }
        changed
    }

    /// Poll text values for every name in `watched`, and push a
    /// `(name, text)` pair onto `queue` whenever the value differs from
    /// the previous frame's snapshot. Cheap when nothing's changed —
    /// one `find_name` + one text getter per watched name per frame.
    /// Names dropped from the watch get pruned out of the snapshot map.
    /// Poll the `Text` of each watched element in view `entity`, returning the
    /// `(x:Name, text)` pairs that changed since last frame (deduped against the
    /// per-scene snapshot). The first poll after a name is watched always
    /// reports (snapshot starts empty), so callers see the current value.
    pub(crate) fn poll_text_reads_for(
        &mut self,
        entity: Entity,
        watched: &[String],
    ) -> Vec<(String, String)> {
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene
            .text_snapshots
            .retain(|k, _| watched.iter().any(|w| w == k));
        if watched.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for name in watched {
            let Some(element) = content.find_name(name) else {
                continue;
            };
            let current = element.text().unwrap_or_default();
            if scene.text_snapshots.get(name) == Some(&current) {
                continue;
            }
            scene.text_snapshots.insert(name.clone(), current.clone());
            changed.push((name.clone(), current));
        }
        changed
    }

    /// Apply view `entity`'s desired generic-DP writes, keyed by
    /// `(x:Name, property)`. Missing names / type mismatches warn.
    pub(crate) fn apply_dp_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<(String, String), crate::dp::DpValue>,
    ) {
        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for ((name, property), value) in desired {
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisDp: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            if value.write_to(&mut element, property) {
                // Update the snapshot eagerly so the read pass doesn't emit a
                // phantom change for a write we just issued ourselves.
                scene
                    .dp_snapshots
                    .insert((name.clone(), property.clone()), value.clone());
            } else {
                warn!("NoesisDp: write to {name:?}.{property:?} failed (unknown property or type)");
            }
        }
    }

    /// Poll view `entity`'s watched DPs, returning `(name, property, value)`
    /// for each that changed since last frame (deduped against the per-scene
    /// snapshot). First poll after a watch is added always reports.
    pub(crate) fn poll_dp_reads_for(
        &mut self,
        entity: Entity,
        watched: &[crate::dp::DpWatch],
    ) -> Vec<(String, String, crate::dp::DpValue)> {
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene.dp_snapshots.retain(|(name, property), _| {
            watched
                .iter()
                .any(|w| &w.name == name && &w.property == property)
        });
        if watched.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for watch in watched {
            let Some(element) = content.find_name(&watch.name) else {
                continue;
            };
            let Some(current) = watch.kind.read_from(&element, &watch.property) else {
                continue;
            };
            let key = (watch.name.clone(), watch.property.clone());
            if scene.dp_snapshots.get(&key) == Some(&current) {
                continue;
            }
            scene.dp_snapshots.insert(key, current.clone());
            changed.push((watch.name.clone(), watch.property.clone(), current));
        }
        changed
    }

    /// Assign view `entity`'s desired `RenderTransform`s (`x:Name → spec`). Each
    /// spec becomes a `CompositeTransform` held at +1 in
    /// [`Self::transform_handles`] (the same object Noesis stores), so the poll
    /// can read it back. Missing names / non-`UIElement` targets warn.
    pub(crate) fn apply_transforms_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, crate::transforms::TransformSpec>,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        // Drop handles for names no longer requested; releasing each handle's +1
        // (Noesis still holds its own ref until the DP is overwritten / cleared).
        scene
            .transform_handles
            .retain(|k, _| desired.contains_key(k));
        if desired.is_empty() {
            return;
        }
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, spec) in desired {
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisTransform: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let transform = CompositeTransform::new(spec.to_fields());
            if element.set_render_transform(&transform) {
                scene.transform_handles.insert(name.clone(), transform);
            } else {
                warn!(
                    "NoesisTransform: {name:?} has no RenderTransform (not a UIElement?) \
                     in scene {:?}",
                    scene.built_for_uri,
                );
            }
        }
    }

    /// Paint view `entity`'s elements with the desired code-built brushes
    /// (`(x:Name, target) → spec`). Each spec is built into a fresh Noesis brush
    /// and assigned through the element's typed brush sugar; Noesis takes its own
    /// reference, so the Rust handle is dropped right after. Missing names warn;
    /// a target the element lacks (e.g. `Fill` on a `Border`) warns too.
    /// Called when the view's `NoesisBrushes` component changes.
    pub(crate) fn apply_brushes_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<(String, crate::brushes::BrushTarget), crate::brushes::BrushSpec>,
    ) {
        use crate::brushes::{BrushSpec, BrushTarget};
        use noesis_runtime::brushes::{GradientStop, LinearGradientBrush, SolidColorBrush};
        use noesis_runtime::view::FrameworkElement;

        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };

        // Assign any `Brush` to `target`'s DP via the element's safe sugar.
        fn assign(
            target: BrushTarget,
            el: &mut FrameworkElement,
            brush: &impl noesis_runtime::brushes::Brush,
        ) -> bool {
            match target {
                BrushTarget::Background => el.set_background(brush),
                BrushTarget::Foreground => el.set_foreground(brush),
                BrushTarget::Fill => el.set_fill(brush),
                BrushTarget::Stroke => el.set_stroke(brush),
            }
        }

        for ((name, target), spec) in desired {
            let Some(mut element) = content.find_name(name) else {
                warn!(
                    "NoesisBrushes: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let ok = match spec {
                BrushSpec::Solid(rgba) => {
                    let brush = SolidColorBrush::new(*rgba);
                    assign(*target, &mut element, &brush)
                }
                BrushSpec::LinearGradient { start, end, stops } => {
                    let mut brush = LinearGradientBrush::new();
                    brush.set_start_point(start[0], start[1]);
                    brush.set_end_point(end[0], end[1]);
                    for stop in stops {
                        brush.add_stop(GradientStop::new(stop.offset, stop.color));
                    }
                    assign(*target, &mut element, &brush)
                }
            };
            if !ok {
                warn!(
                    "NoesisBrushes: assigning {:?} to {name:?} failed (no such property on this element type)",
                    target.property(),
                );
            }
        }
    }

    /// Poll view `entity`'s named elements' live `RenderTransform`s, returning
    /// `(name, spec)` for each that changed since last frame (deduped against the
    /// per-scene snapshot). A name only reports while the element's current
    /// `RenderTransform` is the exact object we assigned (pointer identity), so
    /// the read-back is element-sourced proof the assignment took — not an echo
    /// of the component. First poll after assignment always reports.
    pub(crate) fn poll_transforms_for(
        &mut self,
        entity: Entity,
        names: &[&str],
    ) -> Vec<(String, crate::transforms::TransformSpec)> {
        use crate::transforms::TransformSpec;
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene
            .transform_snapshots
            .retain(|name, _| names.contains(&name.as_str()));
        if names.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for &name in names {
            let Some(handle) = scene.transform_handles.get(name) else {
                continue;
            };
            let Some(element) = content.find_name(name) else {
                continue;
            };
            // Read the element's live RenderTransform; only trust it when it is
            // the very object we assigned (Noesis stores our pointer, no clone).
            let Some(live) = element.render_transform() else {
                continue;
            };
            if live.raw() != handle.raw() {
                continue;
            }
            let current = TransformSpec::from_fields(handle.get());
            if scene.transform_snapshots.get(name) == Some(&current) {
                continue;
            }
            scene.transform_snapshots.insert(name.to_string(), current);
            changed.push((name.to_string(), current));
        }
        changed
    }

    /// Poll view `entity`'s painted targets, returning `(name, target, readback)`
    /// for each whose live brush changed since last frame (deduped against the
    /// per-scene snapshot). A `SolidColorBrush` reports its exact color
    /// ([`BrushReadback::Solid`](crate::brushes::BrushReadback::Solid)); any other
    /// live brush (e.g. a gradient) reports
    /// [`BrushReadback::NonSolid`](crate::brushes::BrushReadback::NonSolid); a
    /// target with no brush at all (unpainted / failed assign) reports nothing.
    /// The read-back proves the assignment landed; first poll after a target is
    /// painted always reports.
    pub(crate) fn poll_brush_reads_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<(String, crate::brushes::BrushTarget), crate::brushes::BrushSpec>,
    ) -> Vec<(
        String,
        crate::brushes::BrushTarget,
        crate::brushes::BrushReadback,
    )> {
        use crate::brushes::BrushReadback;
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene.brush_snapshots.retain(|(name, property), _| {
            desired
                .keys()
                .any(|(n, t)| n == name && t.property() == property)
        });
        if desired.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for (name, target) in desired.keys() {
            let Some(element) = content.find_name(name) else {
                continue;
            };
            let property = target.property();
            // Solid: read the exact color. Otherwise, if a brush is present at
            // all (non-null DP), it's a non-solid brush (e.g. a gradient) — the
            // only gradient signal the unsafe-free crate can read. No brush ⇒
            // nothing landed ⇒ stay silent.
            let current = if let Some(color) = element.solid_brush_color(property) {
                BrushReadback::Solid(color)
            } else if element.get_component(property).is_some() {
                BrushReadback::NonSolid
            } else {
                continue;
            };
            let key = (name.clone(), property.to_string());
            if scene.brush_snapshots.get(&key) == Some(&current) {
                continue;
            }
            scene.brush_snapshots.insert(key, current);
            changed.push((name.clone(), *target, current));
        }
        changed
    }

    /// Reapply per-frame tweakables that don't require a scene rebuild (the PPAA
    /// flag and the DPI scale). Called every frame before Noesis is driven.
    /// Cheap: a compare per knob; each FFI call only fires on change.
    fn apply_live_flags(&mut self, entity: Entity, config: &NoesisView) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
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

    fn install_application_resources_if_needed(&mut self, config: &NoesisView) {
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
        if noesis_runtime::gui::install_app_resources_chain(&config.application_resources) {
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
        // Migration scaffolding (Phase 1a step ii): input routes to the single
        // primary view. Multi-view input routing (which view owns the pointer)
        // is Phase 3A interaction work.
        let Some(scene) = self.scenes.values_mut().next() else {
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
        // Split the borrow so each scene can use the shared registered device
        // while we iterate the scene map mutably.
        let Self {
            scenes,
            registered_device,
            ..
        } = self;
        if scenes.is_empty() {
            return;
        }
        let registered_device = registered_device
            .as_mut()
            .expect("registered_device dropped mid-frame");

        for scene in scenes.values_mut() {
            // Lazy renderer init — needs both the live View and the registered
            // device, so it happens on first frame here (not at scene creation).
            if !scene.renderer_initialized {
                let mut renderer = scene.view.renderer();
                renderer.init(registered_device);
                // Don't call renderer.shutdown() here — keep the init live.
                scene.renderer_initialized = true;
            }

            // Paint into this frame's back buffer (the one the render thread is
            // not currently blitting). `publish_intermediates` flips the index.
            registered_device
                .device_mut::<WgpuRenderDevice>()
                .set_onscreen_target(scene.intermediates[scene.write_index].view.clone());

            let _changed = scene.view.update(time_secs);
            let mut renderer = scene.view.renderer();
            let _ = renderer.update_render_tree();
            let _ = renderer.render_offscreen();
            renderer.render(false, true);
            // WgpuRenderDevice auto-submits at end_onscreen_render, so the
            // intermediate is ready to sample by the time the graph runs.
        }
    }

    /// Publish each rendered scene's just-painted buffer onto its camera entity
    /// as a [`NoesisIntermediate`] component (the main→render handoff — only
    /// `Send + Sync` `TextureView`s cross the boundary), then flip `write_index`
    /// so the next frame paints the *other* buffer while the render thread blits
    /// this one.
    fn publish_intermediates(&mut self, commands: &mut Commands) {
        for (&entity, scene) in &mut self.scenes {
            if !scene.renderer_initialized {
                continue;
            }
            let published = &scene.intermediates[scene.write_index];
            commands.entity(entity).insert(NoesisIntermediate {
                view: published.view.clone(),
                sample_view: published.sample_view.clone(),
            });
            scene.write_index ^= 1;
        }
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
                    let _ = element.set_text(text);
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

    /// Tear down the scene for `entity` (if any), in Noesis's required order:
    /// `Renderer::shutdown` while the registered device is still alive, then the
    /// `View` drops. No-op when that entity has no live scene.
    fn teardown_scene(&mut self, entity: Entity) {
        // This view's view models / items collections / plain VMs outlive the
        // scene rebuild; mark them unbound so the next apply pass re-binds
        // against the new tree. All are keyed by (or filtered to) this entity.
        if let Some(entry) = self.view_models.get_mut(&entity) {
            entry.reset_attach();
        }
        for ((ent, _), binding) in &mut self.items_sources {
            if *ent == entity {
                binding.reset_bind();
            }
        }
        for ((ent, _), entry) in &mut self.plain_vms {
            if *ent == entity {
                entry.reset_attach();
            }
        }
        if let Some(entry) = self.command_hosts.get_mut(&entity) {
            entry.reset_attach();
        }
        let Some(mut scene) = self.scenes.remove(&entity) else {
            return;
        };
        if scene.renderer_initialized {
            // Must run while the registered device is still alive.
            scene.view.renderer().shutdown();
        }
        drop(scene);
    }

    /// Tear down every live scene (for app/render-state teardown). Same ordered
    /// shutdown as [`Self::teardown_scene`], applied to all entities.
    fn teardown_all_scenes(&mut self) {
        let entities: Vec<Entity> = self.scenes.keys().copied().collect();
        for entity in entities {
            self.teardown_scene(entity);
        }
    }
}

impl Drop for NoesisRenderState {
    fn drop(&mut self) {
        // Strict teardown order (Noesis demands it):
        //   1. scenes (Renderer::shutdown + View drop) FIRST — a `View` holds
        //      refs to the VM `ClassInstance`s / plain-VM instances it was given
        //      as `DataContext` and to the `ObservableCollection`s set as
        //      `ItemsSource`. Those refs must release before we drop the owners,
        //      or the owner's `ClassRegistration` unregisters the class while a
        //      live (View-held) instance still references it → use-after-free.
        //   2. view-models / items / plain-vms — now the last refs; safe to drop.
        //   3. bake rig, registered device + providers, then global `shutdown()`.
        // This `Drop` owns `shutdown()` (rather than a separate guard) so the
        // ordering is guaranteed: Bevy gives no drop order between two main-world
        // resources, and calling `shutdown()` early deadlocks/crashes.
        self.teardown_all_scenes();
        self.view_models.clear();
        self.items_sources.clear();
        self.plain_vms.clear();
        self.command_hosts.clear();
        self.teardown_bake_rig();
        drop(self.registered_device.take());
        drop(self.registered_provider.take());
        drop(self.registered_fonts.take());
        drop(self.registered_textures.take());
        noesis_runtime::shutdown();
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
/// is `Send + Sync` (via unsafe-impls in `noesis_runtime` for the View/Renderer/
/// Registered wrappers), it can live as a regular `Resource` and systems
/// here use idiomatic `Res`/`ResMut` params.
/// Copy the [`XamlRegistry`] into the [`SharedXamlMap`] backing
/// [`BevyXamlProvider`]. Runs on the main thread (alongside the rest of the
/// Noesis driving pipeline) directly against the main-world registry.
#[allow(clippy::needless_pass_by_value)]
fn sync_xaml_provider_map(
    registry: Option<Res<XamlRegistry>>,
    state: Option<NonSend<NoesisRenderState>>,
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
    state: Option<NonSendMut<NoesisRenderState>>,
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
    state: Option<NonSend<NoesisRenderState>>,
) {
    let (Some(registry), Some(state)) = (registry, state) else {
        return;
    };
    state.shared_images().sync_from(&registry);
}

/// Ensure a live [`SceneInstance`] exists for each [`NoesisView`] entity once
/// its XAML bytes land in the shared map. Iterates the extracted view entities;
/// each drives its own scene keyed by entity.
#[allow(clippy::needless_pass_by_value)]
fn ensure_noesis_scene(
    views: Query<(Entity, &NoesisView)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, config) in &views {
        state.ensure_scene(entity, config);
    }
}

/// Re-apply each view's per-frame live settings (PPAA + DPI scale). Cheap —
/// compares against the last-applied value and only fires an FFI call on change.
#[allow(clippy::needless_pass_by_value)]
fn apply_live_scene_flags(
    views: Query<(Entity, &NoesisView)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, config) in &views {
        state.apply_live_flags(entity, config);
    }
}

/// Drain [`NoesisInputQueue`] onto the live View. Runs after
/// [`ensure_noesis_scene`] — so the scene exists — and before
/// [`drive_noesis_frame`] — so `View::Update` picks up the state these
/// events produced (hover highlights, button presses, etc.).
#[allow(clippy::needless_pass_by_value)]
fn apply_noesis_input(
    queue: Option<Res<crate::input::NoesisInputQueue>>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let (Some(queue), Some(mut state)) = (queue, state) else {
        return;
    };
    if queue.events.is_empty() {
        return;
    }
    state.apply_input(&queue.events);
}

/// Drive Noesis for the frame — layout, update render tree, render into each
/// view's intermediate — then publish each intermediate onto its camera entity
/// for the render world to blit. Runs last in the main-world driving chain.
#[allow(clippy::needless_pass_by_value)]
fn drive_noesis_frame(mut commands: Commands, state: Option<NonSendMut<NoesisRenderState>>) {
    let Some(mut state) = state else {
        return;
    };
    state.drive_frame();
    state.publish_intermediates(&mut commands);
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

/// The painted intermediate for a view, published onto the camera entity by the
/// main-world driving systems and `ExtractComponent`'d to the render world for
/// the blit. This is the **only** Noesis data that crosses to the render world —
/// `View`/`Renderer` stay pinned to the main thread (see [`NoesisRenderState`]).
/// Both fields are `wgpu::TextureView` (Arc-backed, `Send + Sync`), so the
/// cross-world hand-off is a cheap clone.
#[derive(Component, ExtractComponent, Clone)]
pub struct NoesisIntermediate {
    /// `Rgba8Unorm` raw view — sampled when the target is plain `Rgba8Unorm`.
    view: wgpu::TextureView,
    /// `Rgba8UnormSrgb` alias — sampled when the target is sRGB/HDR so the
    /// stored bytes round-trip through an sRGB→linear→sRGB decode/encode.
    sample_view: wgpu::TextureView,
}

/// Ordering for the main-world Noesis driving pipeline (all on the main thread).
/// Bridge plugins add their per-view apply systems to [`NoesisSet::Apply`] so
/// element writes land before the frame is driven. Phases run in listed order.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum NoesisSet {
    /// Copy asset registries into the provider-backing shared maps.
    Sync,
    /// Build / resize each [`NoesisView`]'s live scene.
    Ensure,
    /// Apply queued element writes + input onto the live views (bridges here).
    Apply,
    /// `View::Update` + `Renderer::Render` into each intermediate, then publish.
    Drive,
}

/// Shared blit body for both nodes. `overlay = false` overwrites the target 1:1
/// (Core2d, separate UI camera — Bevy composites it afterwards); `overlay = true`
/// premultiplied-alpha composites the UI straight onto the camera's finished
/// scene (Core3d, single-camera).
fn blit_noesis_ui(
    render_context: &mut RenderContext<'_>,
    intermediate: &NoesisIntermediate,
    view_target: &ViewTarget,
    world: &World,
    overlay: bool,
) -> Result<(), NodeRunError> {
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
        &intermediate.sample_view
    } else {
        &intermediate.view
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
    type ViewQuery = (&'static ViewTarget, &'static NoesisIntermediate);

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        (view_target, intermediate): (&'w ViewTarget, &'w NoesisIntermediate),
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        blit_noesis_ui(render_context, intermediate, view_target, world, false)
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
    type ViewQuery = (
        &'static ViewTarget,
        &'static NoesisCamera,
        &'static NoesisIntermediate,
    );

    fn run<'w>(
        &self,
        _graph: &mut RenderGraphContext,
        render_context: &mut RenderContext<'w>,
        (view_target, _marker, intermediate): (
            &'w ViewTarget,
            &'w NoesisCamera,
            &'w NoesisIntermediate,
        ),
        world: &'w World,
    ) -> Result<(), NodeRunError> {
        blit_noesis_ui(render_context, intermediate, view_target, world, true)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

pub struct NoesisRenderPlugin;

impl Plugin for NoesisRenderPlugin {
    fn build(&self, app: &mut App) {
        // The painted intermediate is the only Noesis data the render world sees;
        // it rides each camera entity. (`NoesisView` + the bridges stay
        // main-world — Noesis is thread-affine and lives on the main thread.)
        app.add_plugins((
            ExtractComponentPlugin::<NoesisIntermediate>::default(),
            ExtractComponentPlugin::<NoesisCamera>::default(),
        ));

        // Main-world driving pipeline — all on the main thread (the one thread
        // Bevy pins reliably), satisfying Noesis's thread-affinity contract.
        // Bridge plugins slot their per-view apply systems into `NoesisSet::Apply`.
        app.configure_sets(
            PostUpdate,
            (
                NoesisSet::Sync,
                NoesisSet::Ensure,
                NoesisSet::Apply,
                NoesisSet::Drive,
            )
                .chain(),
        )
        .add_systems(
            PostUpdate,
            (
                (
                    sync_xaml_provider_map,
                    sync_font_provider_map,
                    sync_texture_provider_map,
                )
                    .in_set(NoesisSet::Sync),
                ensure_noesis_scene.in_set(NoesisSet::Ensure),
                (apply_live_scene_flags, apply_noesis_input).in_set(NoesisSet::Apply),
                drive_noesis_frame.in_set(NoesisSet::Drive),
            ),
        );

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            warn!("RenderApp not present; NoesisRenderPlugin compositing is a no-op");
            return;
        };

        render_app
            .init_resource::<BlitPipelineCache>()
            .add_systems(Render, prepare_noesis_blit.in_set(RenderSystems::Prepare))
            // Core2d: overwrite blit on any 2D view that has published an
            // intermediate (the `ViewQuery` gates on `NoesisIntermediate`).
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

    /// Create [`NoesisRenderState`] as a **main-world non-send resource**. Runs
    /// on the main thread (so the resource — and every Noesis handle it owns —
    /// is pinned there, satisfying thread-affinity) and after `RenderPlugin::finish`
    /// has populated the render sub-app's `RenderDevice`/`RenderQueue`, which we
    /// clone out (both are `Arc`-backed, `Send + Sync`).
    fn finish(&self, app: &mut App) {
        let Some(render_app) = app.get_sub_app(RenderApp) else {
            return;
        };
        let device = render_app
            .world()
            .resource::<RenderDevice>()
            .wgpu_device()
            .clone();
        let queue = (**render_app.world().resource::<RenderQueue>().0).clone();
        app.insert_non_send_resource(NoesisRenderState::new(device, queue));
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

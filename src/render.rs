//! Render-graph integration: composites each [`NoesisView`] camera's UI into
//! the Bevy frame.
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
//!   render) from a `Render` schedule system; the graph node itself only
//!   blits the pre-populated intermediate into the camera's [`ViewTarget`].
//! - Registers [`NoesisNode`] into [`Core2d`] between
//!   [`Node2d::MainTransparentPass`] and [`Node2d::EndMainPass`].
//!
//! The intermediate is `Rgba8Unorm` because [`WgpuRenderDevice`]'s pipeline
//! cache compiles every shader variant against that one color format.
//! The blit pipeline is cached per encountered `ViewTarget` format. Both go
//! away once `PipelineCache` keys on format.
//!
//! # Lifecycle ordering
//!
//! Noesis demands a strict teardown sequence: `Renderer::shutdown()` must
//! run while both the `View` and the registered `RenderDevice` are still
//! alive, then the `View` drops, then the device's [`Registered`] guard,
//! then the provider's. We enforce this by holding every Noesis handle
//! in `NoesisRenderState` and implementing [`Drop`] explicitly.
//!
//! [`Registered`]: noesis_runtime::render_device::Registered

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bevy::core_pipeline::core_2d::graph::{Core2d, Node2d};
use bevy::core_pipeline::core_3d::graph::{Core3d, Node3d};
use bevy::ecs::schedule::IntoScheduleConfigs;
use bevy::ecs::system::ScheduleSystem;
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
    ClickSubscription, EventArgs, EventSubscription, KeyDownSubscription, RoutedEvent,
    subscribe_click, subscribe_event, subscribe_keydown,
};
use noesis_runtime::input::KeyBinding;
use noesis_runtime::transforms::{CompositeTransform, CompositeTransform3D, MatrixTransform3D};
use noesis_runtime::view::{FrameworkElement, Key, View};

use crate::binding::{BindingEntry, BuiltBinding};
use crate::commands::{CommandEntry, CommandsDef, SharedCommandQueue};
use crate::events::{SharedClickQueue, SharedKeyDownQueue};
use crate::font::{BevyFontProvider, FontRegistry, SharedFontMap};
use crate::image::{BevyTextureProvider, ImageRegistry, SharedImageMap};
use crate::items::{CollectionViewOp, ItemValue, ItemsBinding, ObjectSource};
use crate::plain_vm::{PlainType, PlainValue, PlainVmEntry, SetSink, unbox};
use crate::render_device::WgpuRenderDevice;
use crate::routed_events::{RoutedEventSnapshot, SharedRoutedEventQueue};
use crate::viewmodel::{AttachTarget, SharedVmChangedQueue, ViewModelDef, VmEntry, VmValue};
use crate::xaml::{BevyXamlProvider, SharedXamlMap, XamlRegistry};
use noesis_runtime::element_tree::panel_children;
use noesis_runtime::plain_vm::{PlainInstance, PlainValueRef, PlainVmBuilder, PlainVmClass};

thread_local! {
    /// Cumulative count of FFI "hops" (calls that cross into the Noesis C++
    /// engine) made on the Noesis thread, this frame and every frame before it.
    /// Lives in a `thread_local` rather than on [`NoesisRenderState`] because the
    /// dominant choke point, [`resolve_named`], is a free function with no `self`
    /// to hang a field off. All Noesis work happens on one thread (the engine is
    /// thread-affine), so a non-atomic `Cell` is both correct and as cheap as a
    /// plain field. Surfaced per frame through
    /// [`NoesisDiagnostics::ffi_hops`](crate::diagnostics::NoesisDiagnostics::ffi_hops).
    static FFI_HOPS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Record one FFI hop into the engine. Cheap: a single non-atomic `Cell` bump on
/// the Noesis thread. Called at the FFI choke points (element resolution via
/// [`resolve_named`], DP get/set, and collection ops) so later perf work can
/// reason about how much engine traffic a frame costs. See [`FFI_HOPS`].
#[inline]
pub(crate) fn record_ffi_hop() {
    FFI_HOPS.with(|c| c.set(c.get().wrapping_add(1)));
}

/// Read the cumulative FFI-hop count for the Noesis thread. Zero until the first
/// engine call. See [`FFI_HOPS`].
#[inline]
#[must_use]
pub(crate) fn ffi_hops() -> u64 {
    FFI_HOPS.with(std::cell::Cell::get)
}

/// Wall-time of the most recent [`NoesisSet::Apply`] phase, plus the in-flight
/// start stamp. A plain `Send` resource (not Noesis state) so it can be read from
/// ordinary systems; [`apply_timer_start`]/[`apply_timer_end`] bracket the whole
/// Apply set (they sit at the tail of Ensure and the head of Drive, and the four
/// `NoesisSet` phases are `.chain()`-ed), and [`crate::diagnostics`] mirrors
/// [`last`](Self::last) into [`NoesisDiagnostics`](crate::diagnostics::NoesisDiagnostics).
#[derive(Resource, Default)]
pub(crate) struct NoesisApplyTimer {
    started: Option<std::time::Instant>,
    /// Duration of the previous frame's Apply phase.
    pub(crate) last: std::time::Duration,
}

/// Color format of the per-view intermediate Noesis paints into. Must match
/// the private `RT_COLOR_FORMAT` in `render_device::wgpu_device`.
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
/// same sRGB→linear decode on blit as a true sRGB target (see [`NoesisNode::run`]).
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
    //   `render_view` (`Rgba8Unorm`): Noesis writes sRGB colours straight
    //                    into the bytes (no linearisation in the pipeline;
    //                    DeviceCaps::linearRendering = false), so the stored
    //                    byte values already match the sRGB representation.
    //
    //   `sample_view` (`Rgba8UnormSrgb`): used by the blit sampler when the
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

/// Build a live `Noesis::Style` from a [`StyleSpec`](crate::styles::StyleSpec):
/// its `BasedOn` base (recursively), unconditional setters, and property / data /
/// multi triggers. `name` / `uri` are for diagnostics only. Returns `None` (and
/// warns) when the target type can't be resolved; an unresolvable base style
/// warns and is skipped, but the style is still built. The returned style is
/// unsealed; seal it by applying it to an element.
fn build_noesis_style(
    spec: &crate::styles::StyleSpec,
    name: &str,
    uri: &str,
) -> Option<noesis_runtime::styles::Style> {
    use noesis_runtime::binding::Binding;
    use noesis_runtime::styles::{DataTrigger, MultiTrigger, Style, Trigger};

    let mut style = Style::new();
    if !style.set_target_type(&spec.target_type) {
        warn!(
            "NoesisStyles: unknown TargetType {:?} for {name:?} in scene {uri:?}",
            spec.target_type,
        );
        return None;
    }

    // BasedOn: build the base chain first and link it. Noesis takes its own
    // reference, so `base` may drop at the end of this scope.
    if let Some(base_spec) = &spec.based_on {
        if let Some(base) = build_noesis_style(base_spec, name, uri) {
            style.set_based_on(&base);
        } else {
            warn!(
                "NoesisStyles: BasedOn style (TargetType {:?}) skipped for {name:?} in scene {uri:?}",
                base_spec.target_type,
            );
        }
    }

    for (property, value) in &spec.setters {
        if !style.add_setter(property, &value.to_boxed()) {
            warn!(
                "NoesisStyles: setter {property:?} unresolved on {:?} ({name:?})",
                spec.target_type,
            );
        }
    }

    for trig in &spec.triggers {
        let mut trigger = Trigger::new();
        if !trigger.set_property(&spec.target_type, &trig.property) {
            warn!(
                "NoesisStyles: trigger property {:?} unresolved on {:?} ({name:?})",
                trig.property, spec.target_type,
            );
            continue;
        }
        if !trigger.set_value(&trig.value.to_boxed()) {
            warn!(
                "NoesisStyles: trigger value for {:?} rejected ({name:?})",
                trig.property
            );
        }
        for (property, value) in &trig.setters {
            if !trigger.add_setter(&spec.target_type, property, &value.to_boxed()) {
                warn!(
                    "NoesisStyles: trigger setter {property:?} unresolved on {:?} ({name:?})",
                    spec.target_type,
                );
            }
        }
        let _ = style.add_trigger(&trigger);
    }

    for dt in &spec.data_triggers {
        let mut trigger = DataTrigger::new();
        let mut binding = Binding::new(&dt.binding_path);
        if dt.relative_source_self {
            binding = binding.relative_source_self();
        }
        let _ = trigger.set_binding(&binding);
        if !trigger.set_value(&dt.value.to_boxed()) {
            warn!(
                "NoesisStyles: data-trigger value for {:?} rejected ({name:?})",
                dt.binding_path
            );
        }
        for (property, value) in &dt.setters {
            if !trigger.add_setter(&spec.target_type, property, &value.to_boxed()) {
                warn!(
                    "NoesisStyles: data-trigger setter {property:?} unresolved on {:?} ({name:?})",
                    spec.target_type,
                );
            }
        }
        let _ = style.add_trigger(&trigger);
    }

    for mt in &spec.multi_triggers {
        let mut trigger = MultiTrigger::new();
        for (property, value) in &mt.conditions {
            if !trigger.add_condition(&spec.target_type, property, &value.to_boxed()) {
                warn!(
                    "NoesisStyles: multi-trigger condition {property:?} unresolved on {:?} ({name:?})",
                    spec.target_type,
                );
            }
        }
        for (property, value) in &mt.setters {
            if !trigger.add_setter(&spec.target_type, property, &value.to_boxed()) {
                warn!(
                    "NoesisStyles: multi-trigger setter {property:?} unresolved on {:?} ({name:?})",
                    spec.target_type,
                );
            }
        }
        let _ = style.add_trigger(&trigger);
    }

    Some(style)
}

// ─────────────────────────────────────────────────────────────────────────────
// Public configuration
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view scene configuration. Add as a [`Component`] to the camera entity
/// you also tag with [`NoesisCamera`]; the render app receives a copy on that
/// entity via [`ExtractComponent`]. One [`NoesisView`] == one live Noesis
/// `View` + intermediate, composited onto that camera. Multiple tagged
/// cameras drive multiple independent views.
///
/// Spawning a `NoesisView` auto-attaches every per-view bridge component (text,
/// visibility, dependency properties, items, focus, geometry, and the rest) via
/// required components, each defaulting to empty. This is what makes a write
/// survive when it is set before the scene exists: the component is already
/// there, so a `Startup`/`OnEnter` system can write into it and the value lands
/// once the scene builds (a freshly built scene re-applies each bridge's current
/// state). An empty bridge component costs nothing: its `Default` allocates no
/// heap and its reconcile pass returns immediately. The data-binding bridges
/// [`NoesisVm`](crate::NoesisVm) and [`NoesisCommands`](crate::commands::NoesisCommands)
/// are not auto-attached: they require an explicit class or command-host name, so
/// you add them yourself, and they keep their own state across scene rebuilds.
#[derive(Component, ExtractComponent, Clone, Debug)]
#[require(
    crate::animation::NoesisAnimation,
    crate::binding::NoesisBinding,
    crate::brushes::NoesisBrushes,
    crate::dp::NoesisDp,
    crate::events::NoesisClickWatch,
    crate::events::NoesisKeyDownWatch,
    crate::focus::NoesisFocus,
    crate::focus_input::NoesisFocusControl,
    crate::geometry::NoesisGeometry,
    crate::imaging::NoesisImaging,
    crate::inlines::NoesisInlines,
    crate::items::NoesisItems,
    crate::layout::NoesisLayout,
    crate::routed_events::NoesisEventWatch,
    crate::shapes::NoesisShapes,
    crate::styles::NoesisStyles,
    crate::svg::NoesisSvg,
    crate::text::NoesisText,
    crate::transforms::NoesisTransform,
    crate::transforms3d::NoesisTransform3D,
    crate::typography::NoesisTypography,
    crate::visibility::NoesisVisibility,
    crate::visual_state::NoesisVisualState
)]
pub struct NoesisView {
    /// Asset URI [`XamlRegistry`] keys on, typically the path passed to
    /// `AssetServer::load("foo.xaml")`.
    pub xaml_uri: String,
    /// Size of the intermediate render target Noesis paints into. The blit
    /// stretches this to fill whatever camera `ViewTarget` it composes on.
    pub size: UVec2,
    /// DPI scale for the view's content (1.0 == 96 ppi). Scales all UI crisply
    /// (vector re-tessellation, not an upscale blur) without changing
    /// [`size`](Self::size). Drive it from the window's scale factor for
    /// resolution-independent UI. Re-applied live via `View::set_scale` when it
    /// changes; no scene rebuild.
    pub scale: f32,
    /// Folder URIs whose fonts must be loaded before the XAML is parsed.
    /// Noesis's `CachedFontProvider` caches an empty folder the first
    /// time it scans one, so if fonts haven't loaded by the time we run
    /// `FrameworkElement::load`, all text in that folder renders
    /// invisibly forever. Populate this with each folder your XAML
    /// references in `FontFamily="Folder/#Family"` attributes.
    ///
    /// Folder URIs should match Noesis's form (no trailing slash), e.g.
    /// `"Fonts"` for `FontFamily="Fonts/#Bitter"`.
    pub wait_for_fonts: Vec<String>,
    /// Specific `(folder, filename)` pairs that must be present in
    /// `FontRegistry` before scene build. Stronger guard than
    /// [`wait_for_fonts`](Self::wait_for_fonts): that one only checks "at
    /// least one entry in this folder", which unblocks scene creation as
    /// soon as the first font arrives, too early when the scene's
    /// application resources need *a specific* font (e.g. the theme's
    /// PT Root UI). Populate with the critical filenames your theme +
    /// scene jointly require.
    pub wait_for_font_files: Vec<(String, String)>,
    /// Specific image URIs that must be present in [`crate::ImageRegistry`]
    /// before scene build. Noesis's `TextureProvider::GetTextureInfo`
    /// returns an empty / zero-size info when the URI is unknown, and the
    /// XAML parser caches that as a permanent "no texture for this URI",
    /// so an `<Image Source="Big.png"/>` whose decode hadn't finished
    /// when the scene built renders empty *forever*, even after the
    /// bytes land. Populate with the URIs your scene's images reference
    /// to keep the scene from building until they're all decoded.
    pub wait_for_images: Vec<String>,
    /// Toggle Noesis's built-in Per-Primitive AA ([`RenderFlag::Ppaa`]).
    /// Changing this at runtime re-calls `View::set_flags`; no scene
    /// rebuild, no View teardown.
    ///
    /// [`RenderFlag::Ppaa`]: noesis_runtime::view::RenderFlag::Ppaa
    pub ppaa: bool,
    /// `ResourceDictionary` URIs to merge into the process-global
    /// application resources (styles, brushes, `ControlTemplate`s), in
    /// dependency order. Each URI must resolve via the same XAML
    /// provider that serves `xaml_uri`.
    ///
    /// These URIs are merged into the one process-global dictionary the
    /// [`NoesisResources`](crate::resources::NoesisResources) bridge also
    /// feeds: each URI becomes a merged dictionary, and any code-built
    /// `NoesisResources` entries are layered on top as base entries (so a
    /// code-built override wins over the theme). Reconciled in the `Sync` phase
    /// before any scene parses. Every view's list is unioned into that shared
    /// dictionary, so declaring the same theme on several views is fine;
    /// declaring *different* chains merges them all (with a warning) since the
    /// resources are process-wide.
    ///
    /// A `{StaticResource}` in a later URI that references an earlier URI's key
    /// resolves in dependency order **when no code-built `NoesisResources`
    /// entries are present** (the chain then installs leaf-by-leaf with the
    /// shared parent scope wired in first). If you also supply code-built
    /// entries or `merged_xaml`, each chain leaf is re-parsed standalone and
    /// such cross-leaf references null-resolve at parse time; keep cross-leaf
    /// `{StaticResource}`s within a single URI (or its own nested `Source`
    /// children) in that case.
    ///
    /// For the Noesis SDK sample themes a single-URI list such as
    /// `vec![\"NoesisTheme.DarkBlue.xaml\"]` is sufficient (the `xaml_viewer`
    /// example does this via `--theme`).
    pub application_resources: Vec<String>,
    /// Font families Noesis falls back to when an element doesn't
    /// resolve its declared `FontFamily`. Each entry is a Noesis-style
    /// path-rooted family, e.g. `"Fonts/#Bitter"` or
    /// `"Fonts/#PT Root UI"`. The first entry that has glyphs for a
    /// codepoint wins.
    ///
    /// Installed once per process via `Noesis::SetFontFallbacks` after
    /// the font registry has at least one entry. The plugin eagerly
    /// registers every loaded font face with Noesis's
    /// `CachedFontProvider` before scene build (and incrementally as
    /// new fonts arrive), so this list is purely the WPF-style fallback
    /// chain; there's no need to mention non-fallback families just
    /// to make `FontFamily="Fonts/#X"` references resolve.
    ///
    /// Defaults to empty: with no fallback declared, Noesis uses the faces it
    /// resolves from `FontFamily` references directly. Set this to your own
    /// chain (e.g. `["Fonts/#Bitter"]`) when you want a process-wide fallback.
    /// An earlier release defaulted to `["Fonts/#Bitter"]`, which warned in apps
    /// that didn't ship that font.
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
            font_fallbacks: Vec::new(),
        }
    }
}

/// The inputs that produced the currently-installed process-global application
/// resources. Compared field-by-field so
/// [`NoesisRenderState::reconcile_app_resources`] reinstalls only when the
/// code-built entries, merged XAML, or URI chain actually change.
struct AppResourcesSnapshot {
    entries: HashMap<String, crate::resources::ResourceEntry>,
    merged_xaml: Vec<String>,
    chain_uris: Vec<String>,
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
    /// Entities that currently carry a published [`NoesisIntermediate`]. Tracked
    /// so [`Self::publish_intermediates`] can strip the component off entities
    /// whose scene has since been torn down (`xaml_uri` cleared, uri swapped to
    /// not-yet-loaded bytes, readiness gate re-blocked) but that survive as
    /// entities — otherwise the last-painted frame keeps compositing forever.
    published_intermediates: HashSet<Entity>,
    /// Entities whose scene was (re)built during this frame's Ensure pass. The
    /// Apply-set bridges read it so a write applies even when the component last
    /// changed before its scene existed: a freshly-built scene re-runs every
    /// bridge's apply against the current component state. Cleared at the top of
    /// each Ensure pass.
    scenes_built_this_frame: HashSet<Entity>,
    /// Panel entities whose fragment first mounted into its host this frame. The
    /// focus bridge ORs this so a once-set `NoesisFocus` on a panel still lands
    /// once the fragment exists (a panel has no scene, so `scenes_built_this_frame`
    /// never covers it). Cleared alongside `scenes_built_this_frame`.
    panels_mounted_this_frame: HashSet<Entity>,
    /// Whether the last pointer event hit-tested onto UI. [`Self::apply_input`]
    /// sets it from the `View` mouse return; [`apply_noesis_input`] mirrors it to
    /// the main-world [`NoesisPointerOverUi`](crate::input::NoesisPointerOverUi).
    /// Persists between pointer events (a still cursor keeps its status).
    pointer_over_ui: bool,
    /// One-time flag for `SetFontFallbacks`/`SetFontDefaultProperties`;
    /// must fire AFTER Bevy has loaded at least one font into
    /// `SharedFontMap`, because Noesis's `SetFontFallbacks` eagerly runs
    /// `FontProvider::ScanFolder` to prime its cache.
    fallbacks_installed: bool,
    /// `(folder, filename)` pairs we've already eagerly registered with
    /// the C++ `CachedFontProvider` cache. Used by
    /// [`Self::register_pending_fonts`] to diff incrementally so we
    /// re-register each face exactly once. The eager-registration path
    /// makes scan-time gating irrelevant: any face present here is
    /// findable by `MatchFont` regardless of when `ScanFolder` ran.
    registered_faces: HashSet<(String, String)>,
    /// Snapshot of the last process-global application resources we installed
    /// (code-built entries + merged XAML + the views' URI chain), so
    /// [`Self::reconcile_app_resources`] can skip a rebuild when nothing
    /// changed. `None` until the first install. Both the
    /// [`NoesisResources`](crate::resources::NoesisResources) bridge and the
    /// per-view `application_resources` chain feed one merged dictionary here —
    /// they no longer clobber each other.
    installed_app_resources: Option<AppResourcesSnapshot>,
    /// Wall-clock origin for `View::Update(time)`. Bevy's `Time<Real>`
    /// isn't extracted to the render world by default (only
    /// `Time<Virtual>` and the generic `Time` are), so we keep our own.
    /// Drives storyboard progression; `elapsed_secs_f64()` each frame
    /// vs. Noesis's requirement for monotonically-increasing seconds.
    clock_origin: std::time::Instant,
    /// Last `(swallow set, UiKeyDown target)` installed for each subscribed
    /// keydown name. Used by [`Self::sync_keydown_subscriptions`] to detect when
    /// either has changed and re-bind the C++-side handler with the new closure
    /// (which captures both by value).
    last_keydown_swallow: HashMap<(Entity, String), (Vec<Key>, Entity)>,
    /// Last [`UiClicked`](crate::events::UiClicked) target installed per
    /// subscribed `(view, x:Name)`. The click callback captures the target by
    /// value, so a target change re-binds the C++-side handler. Mirrors
    /// [`Self::last_keydown_swallow`].
    last_click_target: HashMap<(Entity, String), Entity>,
    /// Last `(mark_handled, handled_too)` flags installed per subscribed
    /// `(view, x:Name, event name)`. The routed-event callback captures these
    /// by value, so a flag change can't be patched in place; we detect it
    /// here and drop + re-create the subscription. Mirrors
    /// [`Self::last_keydown_swallow`]. Keyed across views; pruned per view at
    /// sync time.
    last_event_config: HashMap<(Entity, String, &'static str), (bool, bool, Entity)>,
    /// Reusable offscreen view for baking label panels (lazily built). Lives
    /// here so it shares the single registered device and its renderer is torn
    /// down before the device drops (see [`Drop`]).
    bake_rig: Option<BakeRig>,
    /// Live Rust-owned view models. Each owns a
    /// `ClassInstance` + `ClassRegistration` and is attached as a scene
    /// element's `DataContext`. Stored here (not in [`SceneInstance`]) so a VM
    /// survives scene rebuilds; the attach pass re-binds it to the new view.
    /// Released in [`Drop`] before the registered device, while Noesis is still
    /// initialized. Keyed by view entity. See [`crate::viewmodel`].
    view_models: HashMap<Entity, VmEntry>,
    /// Rust-owned `ItemsSource` collections keyed by `x:Name`. Each
    /// owns an `ObservableCollection` bound to a named `ItemsControl`. Like
    /// [`Self::view_models`] they outlive scene rebuilds (re-bound by the apply
    /// pass) and are released in [`Drop`] before the registered device. See
    /// [`crate::items`]. Keyed by `(view entity, x:Name)` so each view owns its
    /// list collections.
    items_sources: HashMap<(Entity, String), ItemsBinding>,
    /// Rust-owned plain-struct view models keyed by view entity + component
    /// `TypeId`. Each owns a reflected `PlainVmClass` + instance
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
    /// View entities whose `NoesisVm` / `NoesisCommands` host failed to
    /// register+instantiate, tagged by host kind (`"NoesisVm"` /
    /// `"NoesisCommands"`), so the `warn!` fires once instead of every frame: a
    /// failed build leaves the `view_models` / `command_hosts` slot vacant, so
    /// [`Self::ensure_view_model`] / [`Self::ensure_commands`] retry it each
    /// frame. Cleared on success and on teardown so a fixed/respawned host
    /// re-warns. Mirrors [`Self::failed_fragments`].
    warned_host_build_failures: HashSet<(Entity, &'static str)>,
    /// `(view, target)` pairs already warned about a `DataContext` collision, so
    /// each clash is reported once rather than every frame. A `NoesisVm`,
    /// `NoesisCommands`, and/or plain view models all defaulting to
    /// [`AttachTarget::Root`] on the same view silently clobber each other's
    /// `DataContext` (last attach wins); this dedupes the diagnostic. Cleared
    /// per-entity on teardown so a respawn re-warns. See [`Self::warn_datacontext_collisions`].
    warned_dc_collisions: HashSet<(Entity, AttachTarget)>,
    /// Rust-owned converted/multi bindings keyed by `(view entity, x:Name,
    /// property)`. Each owns the built `Binding`/`MultiBinding` + its
    /// `Converter`/`MultiConverter`, attached to a named element's DP. Same
    /// rebuild/teardown rules as [`Self::items_sources`] (re-bound by the apply
    /// pass after a scene rebuild); released in [`Drop`] before the registered
    /// device. See [`crate::binding`].
    binding_entries: HashMap<(Entity, String, String), BindingEntry>,
    /// Live mounted panels keyed by the **panel entity** (`UiPanel`, distinct
    /// from the host [`NoesisView`] entity). Each owns a loaded sub-XAML fragment
    /// plus an aggregated plain-VM (the union of the panel entity's bound
    /// components) set as that fragment's `DataContext`, mounted as a hosted child
    /// into a named `Panel` in its host view's scene. Re-mounts after a host-scene
    /// rebuild (the host's [`Self::teardown_scene`] resets each child's stamp) and
    /// is reaped (fragment removed from the host panel first) by
    /// [`Self::teardown_panel_for`] on despawn. Released in [`Drop`] after the
    /// scenes (so the host has already released its child refs). See
    /// [`crate::panel`].
    panels: HashMap<Entity, PanelEntry>,
    /// `(panel entity, fragment uri)` pairs whose XAML failed to load, so the
    /// `error!` fires once instead of every frame (a failed build leaves the
    /// `panels` slot vacant, so `sync_panel` retries it each frame).
    failed_fragments: HashSet<(Entity, String)>,
    /// Rust-owned, entity-keyed list bindings (Primitive 2), keyed by `(view
    /// entity, x:Name)`. Each owns an `ObservableCollection` of row instances
    /// reconciled from the view's row query, bound to a named `ItemsControl`.
    /// Outlives scene rebuilds (re-bound by the apply pass) and is released in
    /// [`Drop`] / [`Self::teardown_for`] before the registered device, in the
    /// collection→instances→registration order its fields encode. See
    /// [`crate::list`].
    lists: HashMap<(Entity, String), crate::list::ListBinding>,
    /// Process-global integration callback guards (cursor / open-URL /
    /// play-audio) registered once by [`crate::integration::NoesisIntegrationPlugin`].
    /// Owned here (rather than in that plugin's own resource) so their `Drop`
    /// (which unregisters via FFI) is guaranteed to run BEFORE `shutdown()`:
    /// Bevy gives no drop order between two main-world resources, and
    /// unregistering after `shutdown()` dereferences torn-down engine state.
    /// Type-erased so `render` needn't name the per-hook guard types. See
    /// [`Self::own_integration_guards`].
    integration_guards: Vec<Box<dyn std::any::Any + Send>>,
}

struct SceneInstance {
    view: View,
    renderer_initialized: bool,
    /// Double-buffered intermediates. Each frame the main thread paints
    /// `intermediates[write_index]`, publishes it as the view's
    /// [`NoesisIntermediate`], then flips `write_index`. With Bevy's
    /// 1-frame-deep pipelined rendering, the render thread blits frame N's
    /// buffer while the main thread paints frame N+1 into the *other* buffer,
    /// so the two never touch the same texture and the composite can't tear.
    intermediates: [Intermediate; 2],
    write_index: usize,
    size: UVec2,
    built_for_uri: String,
    /// The exact XAML bytes this scene was parsed from. Held so
    /// [`NoesisRenderState::ensure_scene`] can detect a hot-reload: when the
    /// shared map's `Arc<Vec<u8>>` for [`Self::built_for_uri`] no longer points
    /// at these bytes (an asset `Modified` event or a direct
    /// [`XamlRegistry::insert`] both allocate a fresh `Arc`), the markup
    /// changed and the scene is rebuilt against the new bytes.
    built_bytes: Arc<Vec<u8>>,
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
    /// scene; orphaned subscriptions can't outlive their button.
    click_subs: HashMap<String, ClickSubscription>,
    /// Active `UIElement::KeyDown` subscriptions keyed by `x:Name`. Synced
    /// each frame against [`crate::events::NoesisKeyDownWatch`] by
    /// [`NoesisRenderState::sync_keydown_subscriptions`]. Same lifetime
    /// rules as `click_subs`.
    keydown_subs: HashMap<String, KeyDownSubscription>,
    /// Active generic `RoutedEvent` subscriptions keyed by `(x:Name, event
    /// name)`; one element may be watched for several events. Synced each
    /// frame against [`crate::routed_events::NoesisEventWatch`] by
    /// [`NoesisRenderState::sync_event_subscriptions_for`]. Drops with the
    /// scene; same orphan-safety rules as `click_subs` / `keydown_subs`. The
    /// `&'static str` half is `RoutedEvent::as_str()` (the enum is not `Hash`,
    /// its stable name is).
    event_subs: HashMap<(String, &'static str), EventSubscription>,
    /// Per-row click subscriptions for entity-keyed lists, keyed by the list
    /// control's `x:Name`. One `MouseLeftButtonUp` handler is installed on each
    /// bound `ItemsControl`; its callback walks the clicked element's
    /// `DataContext` to the row's hidden `__entity` field and pushes a
    /// row-targeted [`UiClicked`](crate::events::UiClicked). Installed by
    /// [`NoesisRenderState::apply_list_for`] when the `ItemsSource` binds; drops
    /// with the scene, same orphan-safety rules as [`Self::event_subs`].
    row_click_subs: HashMap<String, EventSubscription>,
    /// Last text snapshot per name in [`crate::text::NoesisTextReadWatch`].
    /// Used to dedupe `NoesisTextChanged` emissions: only push when the
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
    /// `CompositeTransform3D` handles assigned as elements' `Transform3D` by
    /// [`crate::transforms3d::NoesisTransform3D`], keyed by `x:Name`. Held at +1
    /// (the same object Noesis stores) so the poll can read it back; same
    /// lifetime/identity rules as [`Self::transform_handles`].
    transform3d_handles: HashMap<String, CompositeTransform3D>,
    /// Last [`Transform3DSpec`](crate::transforms3d::Transform3DSpec) snapshot
    /// per `x:Name`, to dedupe
    /// [`crate::transforms3d::NoesisTransform3DChanged`] emissions. Resets on
    /// scene rebuild.
    transform3d_snapshots: HashMap<String, crate::transforms3d::Transform3DSpec>,
    /// `MatrixTransform3D` handles assigned as elements' `Transform3D` by
    /// [`crate::transforms3d::NoesisTransform3D`]'s matrix writes, keyed by
    /// `x:Name`. Held at +1 (the same object Noesis stores) so the poll can read
    /// it back; same lifetime/identity rules as [`Self::transform3d_handles`].
    /// Distinct from [`Self::transform3d_handles`] because the two transform
    /// kinds carry different read-back payloads; a name should use one or the
    /// other (both write the single `Transform3D` DP, so the later apply wins).
    matrix_transform3d_handles: HashMap<String, MatrixTransform3D>,
    /// Last 12-float `Transform3` matrix snapshot per `x:Name`, to dedupe
    /// [`crate::transforms3d::NoesisMatrixTransform3DChanged`] emissions. Resets
    /// on scene rebuild.
    matrix_transform3d_snapshots: HashMap<String, [f32; 12]>,
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
    /// Diff-reconciled: a spec dropped from the component detaches its binding
    /// via `KeyBinding::remove_from`. Remaining entries drop with the scene.
    input_bindings: HashMap<(String, i32, i32), InstalledKeyBinding>,
    /// Last `(candidate, predicted_name, matches_expected)` per
    /// [`crate::focus_input::FocusPredict`] ident, to dedupe
    /// [`crate::focus_input::NoesisFocusPredicted`] emissions. Resets on scene
    /// rebuild.
    predict_snapshots: HashMap<(String, i32, Option<String>), (bool, Option<String>, bool)>,
    /// Last image read-back per `x:Name` watched by
    /// [`crate::imaging::NoesisImaging`]. Dedupes
    /// [`crate::imaging::NoesisImageChanged`] emissions; resets on scene rebuild.
    image_snapshots: HashMap<String, crate::imaging::ImageReadback>,
    /// Live inline handle trees built by [`crate::inlines::NoesisInlines`], keyed
    /// by `x:Name`. Held so the read-back can re-read live `Run` text / `Hyperlink`
    /// URIs; the `TextBlock`'s collection also owns these (`AddRef`'d on add), so
    /// they stay valid while it keeps them. Drops with the scene.
    inline_handles: HashMap<String, Vec<crate::inlines::BuiltInline>>,
    /// Last inline read-back per `x:Name` watched by `NoesisInlines`. Dedupes
    /// [`crate::inlines::NoesisInlinesChanged`] emissions; resets on scene rebuild.
    inlines_snapshots: HashMap<String, crate::inlines::InlinesReadback>,
}

/// One installed `KeyBinding`. Holds the `Command` *and* the `KeyBinding` at +1
/// so neither is released while the binding lives in the element's
/// `InputBindings`. On reconcile a dropped spec calls `binding.remove_from` to
/// detach it; scene teardown drops both, releasing our references. `command` is
/// held only to keep its +1 alive for the binding's lifetime.
struct InstalledKeyBinding {
    #[allow(dead_code)] // held only to keep the command's +1 alive.
    command: Command,
    binding: KeyBinding,
}

/// Process-global monotonic sequence for synthesizing a unique Noesis class name
/// per mounted panel. Noesis registers reflected classes globally by name, so two
/// panels (even of the same component set) get distinct classes (hence distinct
/// `DataContext`s and scopes). One counter for the process is enough; it only ever
/// increments on the main thread inside [`PanelEntry::build`].
static PANEL_CLASS_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// One live mounted panel (see [`NoesisRenderState::panels`]). Field order is the
/// drop order Noesis requires: the click/keydown subscription maps first (each drop
/// fires the C++ unsubscribe on a fragment element), then `fragment` (held by the
/// host panel and holding a `DataContext` ref to `instance`), then `instance`, then
/// `_class` (whose drop unregisters the class, so it must outlive every live
/// instance of it).
struct PanelEntry {
    /// `BaseButton::Click` subscriptions on fragment-internal elements, keyed by
    /// `x:Name`, for a [`crate::events::NoesisClickWatch`] placed on this panel
    /// entity. Resolved against the fragment's private namescope (a host-view
    /// `FindName` can't see inside it). Drops before `fragment`; same orphan-safety
    /// as the scene's `click_subs`.
    click_subs: HashMap<String, ClickSubscription>,
    /// `UIElement::KeyDown` subscriptions on fragment-internal elements, keyed by
    /// `x:Name`. Same rules as [`Self::click_subs`].
    keydown_subs: HashMap<String, KeyDownSubscription>,
    /// The loaded sub-XAML, its own namescope. `DataContext` set once at build.
    fragment: FrameworkElement,
    /// Aggregated plain-VM instance: one synthetic class whose properties are the
    /// union of the panel entity's bound components.
    instance: PlainInstance,
    _class: PlainVmClass,
    /// Aggregated property names in global index order, for `set_and_notify`.
    prop_names: Vec<String>,
    /// UI→Rust writeback sink the `on_set` hook pushes `(global_index, value)`
    /// onto; drained each frame and routed back to the originating component by
    /// the per-type [`crate::panel`] writeback systems.
    set_sink: SetSink,
    /// Host [`NoesisView`] entity whose scene contains the mount target.
    host: Entity,
    /// `x:Name` of the `Panel` in the host scene to mount the fragment into.
    host_name: String,
    /// `built_for_uri` stamp of the host scene we are mounted into; `None` until
    /// mounted (and reset on a host-scene rebuild, forcing a re-mount).
    mounted_for_uri: Option<String>,
    /// Last text per watched fragment-scope `x:Name`, to dedupe
    /// [`crate::panel::NoesisPanelTextChanged`] emissions. Resolved against the
    /// fragment's *own* namescope (mounted fragments are private to the host).
    text_snapshots: HashMap<String, String>,
}

impl PanelEntry {
    /// Load the sub-XAML, register a uniquely-named synthetic class for the
    /// aggregated `props`, instantiate, set it as the fragment's `DataContext`.
    /// `None` if the XAML doesn't resolve or registration/instantiation fails.
    fn build(
        uri: &str,
        host: Entity,
        host_name: &str,
        props: &[(String, PlainType)],
    ) -> Option<Self> {
        let mut fragment = FrameworkElement::load(uri)?;
        let seq = PANEL_CLASS_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let class_name = format!("DmPanel.{seq}");
        let prop_names: Vec<String> = props.iter().map(|(n, _)| n.clone()).collect();
        let kinds: Vec<PlainType> = props.iter().map(|(_, k)| *k).collect();
        let set_sink: SetSink = Arc::new(std::sync::Mutex::new(Vec::new()));

        let mut builder = PlainVmBuilder::new(&class_name);
        for (name, kind) in props {
            builder.add_property(name, *kind);
        }
        let sink_for_handler = Arc::clone(&set_sink);
        let class = builder
            .on_set(move |idx: u32, value: &PlainValueRef| {
                let kind = kinds
                    .get(idx as usize)
                    .copied()
                    .unwrap_or(PlainType::BaseComponent);
                let owned = unbox(kind, value);
                if let Ok(mut queue) = sink_for_handler.lock() {
                    queue.push((idx, owned));
                }
            })
            .register()?;
        let instance = class.create_instance()?;
        if !instance.set_data_context(&mut fragment) {
            warn!("UiPanel: set_data_context returned false for fragment {uri:?}");
        }
        Some(Self {
            click_subs: HashMap::new(),
            keydown_subs: HashMap::new(),
            fragment,
            instance,
            _class: class,
            prop_names,
            set_sink,
            host,
            host_name: host_name.to_owned(),
            mounted_for_uri: None,
            text_snapshots: HashMap::new(),
        })
    }

    /// Push changed aggregated properties into the instance (Rust→UI). `pushes`
    /// carries `(global_index, value)` for only the components that changed.
    fn apply_pushes(&self, pushes: &[(u32, PlainValue)]) {
        for (gi, value) in pushes {
            if let Some(name) = self.prop_names.get(*gi as usize) {
                let _ = self.instance.set_and_notify(*gi, name, value.clone());
            }
        }
    }

    /// Take pending UI→Rust writebacks (drained each frame, routed to components).
    fn drain_writebacks(&self) -> Vec<(u32, PlainValue)> {
        let mut guard = self.set_sink.lock().expect("panel set sink poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }

    /// Best-effort removal of our fragment from the host panel, so the host
    /// releases its child ref before this entry drops. No-op if the host scene is
    /// already gone (its tree drop already released the fragment). Matches by
    /// pointer identity since sibling panels share the collection and indices
    /// shift as children come and go.
    fn unmount(&self, scenes: &HashMap<Entity, SceneInstance>) {
        let Some(scene) = scenes.get(&self.host) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        let Some(host_el) = resolve_named(&content, &self.host_name) else {
            return;
        };
        let Some(mut children) = panel_children(&host_el) else {
            return;
        };
        let target = self.fragment.raw();
        for i in 0..children.count() {
            if std::ptr::eq(children.get_raw(i), target) {
                let _ = children.remove_at(i);
                break;
            }
        }
    }
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

/// Resolve an element by its (optionally scope-qualified) `x:Name`, starting
/// from a scene's root element.
///
/// A plain name like `"PlayButton"` is a single `FindName` against `root`'s
/// namescope — the long-standing behavior, unchanged.
///
/// A name may also be *scope-qualified* with `/` to reach into a composed
/// control. Noesis (like WPF) gives every control loaded from its own XAML —
/// a `UserControl`, a templated part — a **private namescope**: the names
/// declared inside it are invisible to a `FindName` on the outer root. So if
/// `main_menu.xaml` is hosted as `<local:MainMenu x:Name="MainMenu"/>`, its
/// inner `PlayButton` is *not* reachable as `"PlayButton"` from the view root.
/// Write `"MainMenu/PlayButton"` instead: each segment but the last names a
/// host control to descend into, and the final segment is resolved inside that
/// host's own namescope. Nesting goes as deep as you like
/// (`"MainMenu/Footer/PlayButton"`).
///
/// Returns [`None`] if any segment along the path fails to resolve.
fn resolve_named(root: &FrameworkElement, path: &str) -> Option<FrameworkElement> {
    // The single most-travelled FFI choke point: nearly every per-element apply
    // resolves a name first, so counting here captures the bulk of engine traffic.
    record_ffi_hop();
    resolve_scope_path(root, path, |host, name| host.find_name(name))
}

/// Walk a `/`-separated scope `path`, descending one namescope per segment via
/// `find`: each segment but the last names a host to step into, the last names
/// the target. Generic over the node type so the traversal is unit-testable
/// without a live Noesis scene; [`resolve_named`] specializes it to
/// [`FrameworkElement::find_name`].
fn resolve_scope_path<T>(
    root: &T,
    path: &str,
    find: impl Fn(&T, &str) -> Option<T> + Copy,
) -> Option<T> {
    match path.split_once('/') {
        None => find(root, path),
        Some((host, rest)) => resolve_scope_path(&find(root, host)?, rest, find),
    }
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
            published_intermediates: HashSet::new(),
            scenes_built_this_frame: HashSet::new(),
            panels_mounted_this_frame: HashSet::new(),
            pointer_over_ui: false,
            fallbacks_installed: false,
            registered_faces: HashSet::new(),
            installed_app_resources: None,
            clock_origin: std::time::Instant::now(),
            last_keydown_swallow: HashMap::new(),
            last_click_target: HashMap::new(),
            last_event_config: HashMap::new(),
            bake_rig: None,
            view_models: HashMap::new(),
            items_sources: HashMap::new(),
            plain_vms: HashMap::new(),
            command_hosts: HashMap::new(),
            warned_host_build_failures: HashSet::new(),
            warned_dc_collisions: HashSet::new(),
            binding_entries: HashMap::new(),
            panels: HashMap::new(),
            failed_fragments: HashSet::new(),
            lists: HashMap::new(),
            integration_guards: Vec::new(),
        }
    }

    /// Take ownership of the process-global integration callback guards so they
    /// drop before `shutdown()` (see [`Self::integration_guards`]). The caller
    /// registers exactly once; guards already present are replaced. Main-thread
    /// only.
    pub(crate) fn own_integration_guards(&mut self, guards: Vec<Box<dyn std::any::Any + Send>>) {
        self.integration_guards = guards;
    }

    /// Build view `entity`'s [`VmEntry`] on first sight (register the Noesis
    /// class, instantiate, wire the entity-tagged change forwarder). When a
    /// re-inserted [`NoesisVm`](crate::viewmodel::NoesisVm) carries a changed def
    /// (class, props, or target), the stale entry is reaped — detached off the
    /// live scene, then dropped so its class unregisters — before rebuilding, or
    /// the fresh registration would collide with the old one under the same name.
    /// No-op when the def is unchanged. Main-thread only.
    pub(crate) fn ensure_view_model(
        &mut self,
        entity: Entity,
        def: &ViewModelDef,
        changed: &SharedVmChangedQueue,
    ) {
        if let Some(existing) = self.view_models.get(&entity) {
            if existing.matches(def) {
                return;
            }
            self.reap_view_model_for(entity);
        }
        match VmEntry::build(entity, def, changed) {
            Some(entry) => {
                self.view_models.insert(entity, entry);
                self.warned_host_build_failures
                    .remove(&(entity, "NoesisVm"));
                self.warn_datacontext_collisions(entity);
            }
            None => {
                if self.warned_host_build_failures.insert((entity, "NoesisVm")) {
                    warn!(
                        "NoesisViewModel: failed to register/instantiate class {:?} (duplicate name?)",
                        def.class_name(),
                    );
                }
            }
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
                AttachTarget::Named(name) => resolve_named(&content, name),
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
    /// with `entity` and pushing to `queue`). When a re-inserted
    /// [`NoesisCommands`](crate::commands::NoesisCommands) carries a changed def
    /// (class, commands, or target), the stale host is reaped — detached off the
    /// live scene, then dropped so its class unregisters — before rebuilding, or
    /// the fresh registration would collide with the old one under the same name.
    /// No-op when the def is unchanged. Main-thread only.
    pub(crate) fn ensure_commands(
        &mut self,
        entity: Entity,
        def: &CommandsDef,
        queue: &SharedCommandQueue,
    ) {
        if let Some(existing) = self.command_hosts.get(&entity) {
            if existing.matches(def) {
                return;
            }
            self.reap_commands_for(entity);
        }
        match CommandEntry::build(entity, def, queue) {
            Some(entry) => {
                self.command_hosts.insert(entity, entry);
                self.warned_host_build_failures
                    .remove(&(entity, "NoesisCommands"));
                self.warn_datacontext_collisions(entity);
            }
            None => {
                if self
                    .warned_host_build_failures
                    .insert((entity, "NoesisCommands"))
                {
                    warn!(
                        "NoesisCommands: failed to register/instantiate class {:?} (duplicate name?)",
                        def.class_name(),
                    );
                }
            }
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
                AttachTarget::Named(name) => resolve_named(&content, name),
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

    /// Warn (once per clash) when more than one Rust-owned host on view `entity`
    /// would attach its instance as the `DataContext` of the same target element.
    /// A `NoesisVm`, a `NoesisCommands`, and plain view models each call
    /// `set_data_context` on their target, and the last attach wins — so two of
    /// them defaulting to [`AttachTarget::Root`] leaves the loser silently inert.
    /// This surfaces the misconfiguration; merging colliding hosts into one is
    /// future work. Called after each host is registered on first sight.
    fn warn_datacontext_collisions(&mut self, entity: Entity) {
        let mut by_target: HashMap<&AttachTarget, Vec<String>> = HashMap::new();
        if let Some(entry) = self.view_models.get(&entity) {
            by_target
                .entry(entry.target())
                .or_default()
                .push("NoesisVm".to_owned());
        }
        if let Some(entry) = self.command_hosts.get(&entity) {
            by_target
                .entry(entry.target())
                .or_default()
                .push("NoesisCommands".to_owned());
        }
        for ((ent, _), entry) in &self.plain_vms {
            if *ent == entity {
                by_target
                    .entry(entry.target())
                    .or_default()
                    .push(format!("plain view model {:?}", entry.type_name()));
            }
        }

        for (target, mut sources) in by_target {
            if sources.len() < 2 {
                continue;
            }
            if !self.warned_dc_collisions.insert((entity, target.clone())) {
                continue;
            }
            sources.sort();
            warn!(
                "DataContext collision on view {entity:?} {}: {} all attach as the same \
                 element's DataContext, so only the last one applied wins and the rest are \
                 silently inert — give them distinct attach targets (attach_to(x_name))",
                target.describe(),
                sources.join(", "),
            );
        }
    }

    /// Reconcile view `entity`'s [`NoesisItems`] component. When `changed`, set
    /// each named element's collection to the desired typed item list and its
    /// desired selection (creating a collection per `(entity, name)` on first
    /// use, pruning names no longer present). Every frame, bind any unbound
    /// collection to its element's `ItemsSource` (handles first resolution and
    /// re-binding after a rebuild), then drive any pending selection and
    /// collection-view navigation.
    pub(crate) fn apply_items_for(
        &mut self,
        entity: Entity,
        sources: &HashMap<String, Vec<ItemValue>>,
        objects: &HashMap<String, ObjectSource>,
        select: &HashMap<String, i32>,
        navigate: &HashMap<String, CollectionViewOp>,
        changed: bool,
    ) {
        if changed {
            // Prune this view's collections whose name was removed.
            self.items_sources.retain(|(ent, name), _| {
                *ent != entity || sources.contains_key(name) || objects.contains_key(name)
            });
            for (name, items) in sources {
                // Object items take precedence over a primitive source for the
                // same name (see `NoesisItems::objects`); skip the source so the
                // control is not applied twice, non-deterministically.
                if objects.contains_key(name) {
                    warn!(
                        "NoesisItems: x:Name {name:?} is in both sources and objects; \
                         using the object items and ignoring the primitive source",
                    );
                    continue;
                }
                let binding = self
                    .items_sources
                    .entry((entity, name.clone()))
                    .or_default();
                binding.set_typed(items);
                binding.set_desired_select(select.get(name).copied());
                binding.set_desired_nav(navigate.get(name).copied());
            }
            for (name, source) in objects {
                let binding = self
                    .items_sources
                    .entry((entity, name.clone()))
                    .or_default();
                binding.set_objects(source);
                binding.set_desired_select(select.get(name).copied());
                binding.set_desired_nav(navigate.get(name).copied());
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
            if *ent != entity {
                continue;
            }
            let Some(mut element) = resolve_named(&content, name) else {
                if binding.needs_bind(&uri) {
                    warn!(
                        "NoesisItems: x:Name {:?} not found in scene {:?}",
                        name, scene.built_for_uri,
                    );
                }
                continue;
            };
            if binding.needs_bind(&uri) {
                record_ffi_hop();
                if element.set_items_source(binding.collection()) {
                    binding.mark_bound(&uri);
                } else {
                    warn!("NoesisItems: element {name:?} is not an ItemsControl; skipped");
                    continue;
                }
            }
            binding.drive_selection(&mut element);
            // Navigation runs last so it wins when both select and navigate are
            // set for the same control (both move the shared current item).
            binding.drive_navigation();
        }
    }

    /// Poll each of view `entity`'s bound list controls, returning
    /// `(x:Name, count, selected_index, current_position, current-typed-value)`
    /// for every control whose snapshot changed since the last poll. Drives the
    /// [`NoesisItemsCurrent`](crate::items::NoesisItemsCurrent) read-back.
    pub(crate) fn poll_items_for(
        &mut self,
        entity: Entity,
    ) -> Vec<(String, usize, i32, i32, Option<ItemValue>)> {
        let mut out = Vec::new();
        let Some(scene) = self.scenes.get(&entity) else {
            return out;
        };
        let Some(content) = scene.view.content() else {
            return out;
        };
        for ((ent, name), binding) in &mut self.items_sources {
            if *ent != entity {
                continue;
            }
            let Some(element) = resolve_named(&content, name) else {
                continue;
            };
            if let Some((count, selected_index, current_position, current)) =
                binding.read_changed(&element)
            {
                out.push((
                    name.clone(),
                    count,
                    selected_index,
                    current_position,
                    current,
                ));
            }
        }
        out
    }

    /// Reconcile view `entity`'s entity-keyed list `name` (Primitive 2): ensure
    /// the row class, diff the live collection to `desired` (minimal
    /// Add/Remove/Update/Move, never a clear), bind the collection to the named
    /// `ItemsControl` once the scene + element exist (re-binding after a rebuild),
    /// and reconcile selection (currency). Returns the op tally and any UI-driven
    /// selection change for the caller to mirror onto a [`Selected`](crate::list::Selected)
    /// marker. See [`crate::list`].
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_list_for(
        &mut self,
        entity: Entity,
        name: &str,
        class: &str,
        schema: &[(&'static str, crate::plain_vm::PlainType)],
        desired: &[crate::list::DesiredRow],
        desired_selected: Option<Entity>,
        click_queue: &SharedClickQueue,
    ) -> (crate::list::ListOps, crate::list::SelectionOutcome) {
        let key = (entity, name.to_owned());
        let ops = {
            let binding = self.lists.entry(key.clone()).or_default();
            binding.reconcile_into(class, schema, desired)
        };

        // Bind the ItemsSource once the scene + named control exist; re-bind after
        // a rebuild. Resolve the scene content first so the `scenes` borrow ends
        // before the `lists` mutable borrow (disjoint fields).
        let mount = self
            .scenes
            .get(&entity)
            .and_then(|s| s.view.content().map(|c| (c, s.built_for_uri.clone())));
        if let Some((content, uri)) = mount
            && self.lists.get(&key).is_some_and(|b| b.needs_bind(&uri))
        {
            match resolve_named(&content, name) {
                Some(mut element) => {
                    record_ffi_hop();
                    let bound = {
                        let binding = self.lists.get_mut(&key).expect("checked above");
                        let ok = element.set_items_source(binding.collection());
                        if ok {
                            binding.mark_bound(&uri);
                            // Stash a handle on the control so selection is read /
                            // driven on the live control itself (its own selection
                            // is the source of truth; no separate CollectionView).
                            binding.set_control(element.clone_ref());
                        }
                        ok
                    };
                    if bound {
                        self.install_row_click_sub(entity, name, &element, click_queue);
                    } else {
                        warn!("UiList: element {name:?} is not an ItemsControl; skipped");
                    }
                }
                None => warn!("UiList: x:Name {name:?} not found in scene {uri:?}"),
            }
        }

        let selection = self
            .lists
            .get_mut(&key)
            .map_or(crate::list::SelectionOutcome::Unchanged, |b| {
                b.poll_selection(desired_selected)
            });
        (ops, selection)
    }

    /// Install the one per-row `MouseLeftButtonUp` handler on the `ItemsControl`
    /// named `name` (Primitive 3, per-row events). Templated rows carry no
    /// `x:Name`, so instead of subscribing each row we subscribe the control once
    /// and recover the clicked row from the event: the callback walks the event
    /// source's `DataContext` to the hidden `__entity` `u64` field (stashed per
    /// row by [`crate::list`]) and pushes a row-targeted [`UiClicked`] onto the
    /// shared click queue. Stored in the scene so it drops with the view; re-run
    /// only when the list (re-)binds.
    fn install_row_click_sub(
        &mut self,
        entity: Entity,
        name: &str,
        element: &FrameworkElement,
        click_queue: &SharedClickQueue,
    ) {
        let queue_handle = click_queue.clone();
        let list_name = name.to_owned();
        let Some(sub) = subscribe_event(
            element,
            RoutedEvent::MouseLeftButtonUp,
            // Observe only: never consume the click, so the control's own
            // selection / currency handling still runs.
            false,
            move |args: &EventArgs| {
                if let Some(bits) = args.source_data_context_u64(crate::list::ENTITY_FIELD)
                    && let Some(row) = Entity::try_from_bits(bits)
                {
                    queue_handle.push(entity, row, list_name.clone());
                }
                false
            },
        ) else {
            warn!("UiList: element {name:?} is not a UIElement; per-row clicks disabled");
            return;
        };
        if let Some(scene) = self.scenes.get_mut(&entity) {
            scene.row_click_subs.insert(name.to_owned(), sub);
        }
    }

    /// Number of live entity-keyed list bindings. Mirrors `self.lists.len()`.
    #[must_use]
    pub(crate) fn live_list_count(&self) -> usize {
        self.lists.len()
    }

    /// Whether view `entity` already has a built binding for `(element,
    /// property)`. The bridge builds each target's runtime binding once.
    pub(crate) fn has_binding(&self, entity: Entity, element: &str, property: &str) -> bool {
        self.binding_entries
            .contains_key(&(entity, element.to_owned(), property.to_owned()))
    }

    /// Store a freshly built binding for view `entity`'s `(element, property)`
    /// target, replacing any prior entry for that key. Bound to its element by
    /// the next [`Self::bind_pending_for`] pass.
    pub(crate) fn insert_binding(
        &mut self,
        entity: Entity,
        element: String,
        property: String,
        built: BuiltBinding,
    ) {
        self.binding_entries
            .insert((entity, element, property), BindingEntry::new(built));
    }

    /// Reap view `entity`'s single [`NoesisBinding`](crate::binding::NoesisBinding)
    /// target `(element, property)`: clear the live binding off the element's DP
    /// (`ClearValue`, so it stops driving the property) before dropping the owning
    /// entry. Mirrors [`Self::reap_items_for`]'s detach-then-drop order; the clear
    /// is a no-op when the scene or element is gone or the binding never attached.
    pub(crate) fn reap_binding_for(&mut self, entity: Entity, element: &str, property: &str) {
        let key = (entity, element.to_owned(), property.to_owned());
        if let Some(entry) = self.binding_entries.get(&key)
            && let Some(scene) = self.scenes.get(&entity)
            && !entry.needs_bind(&scene.built_for_uri)
            && let Some(content) = scene.view.content()
            && let Some(mut target) = resolve_named(&content, element)
        {
            target.clear_value(property);
        }
        self.binding_entries.remove(&key);
    }

    /// Drop (and unbind) any of view `entity`'s binding targets no longer named
    /// by its [`NoesisBinding`]'s current `keep` set. A target removed from a
    /// re-inserted component is unbound off its element via
    /// [`Self::reap_binding_for`] and released here; without this a dropped
    /// binding keeps driving its property forever.
    pub(crate) fn prune_bindings_for(&mut self, entity: Entity, keep: &[(String, String)]) {
        let stale: Vec<(String, String)> = self
            .binding_entries
            .keys()
            .filter(|(ent, element, property)| {
                *ent == entity && !keep.iter().any(|(ke, kp)| ke == element && kp == property)
            })
            .map(|(_, element, property)| (element.clone(), property.clone()))
            .collect();
        for (element, property) in stale {
            self.reap_binding_for(entity, &element, &property);
        }
    }

    /// Attach any of view `entity`'s not-yet-bound bindings onto their named
    /// element's DP. No-op until the view (and named element) exists; retries each
    /// frame, and re-attaches after a scene rebuild. Mirrors
    /// [`Self::apply_items_for`]'s bind pass.
    pub(crate) fn bind_pending_for(&mut self, entity: Entity) {
        let Some(scene) = self.scenes.get(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        let uri = scene.built_for_uri.clone();
        for ((ent, element, property), entry) in &mut self.binding_entries {
            if *ent != entity || !entry.needs_bind(&uri) {
                continue;
            }
            let Some(target) = resolve_named(&content, element) else {
                warn!(
                    "NoesisBinding: x:Name {:?} not found in scene {:?}",
                    element, scene.built_for_uri,
                );
                continue;
            };
            if entry.bind_onto(&target, property) {
                entry.mark_bound(&uri);
            } else {
                warn!(
                    "NoesisBinding: binding {element:?}.{property:?} failed \
                     (unknown property or type mismatch)",
                );
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
            let Some(entry) = PlainVmEntry::build(type_name, entity, props, target.clone()) else {
                warn!("NoesisViewModel: failed to register plain VM {type_name:?}");
                return Vec::new();
            };
            slot.insert(entry);
            self.warn_datacontext_collisions(entity);
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
                    AttachTarget::Named(name) => resolve_named(&content, name),
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

    /// Reconcile the mounted panel for `entity`: build it on first sight (load
    /// `uri`, register the aggregated class from `props`, set `DataContext`), push
    /// the changed aggregated properties (Rust→UI), mount the fragment into the
    /// host scene's named `Panel` once that scene exists (re-mounting after a
    /// rebuild), and return any queued two-way edits (UI→Rust) keyed by global
    /// property index for the caller to route back to the originating components.
    /// `props` is the frozen aggregated layout; `pushes` are the changed-only
    /// `(global_index, value)` pairs. See [`crate::panel`].
    pub(crate) fn sync_panel(
        &mut self,
        entity: Entity,
        uri: &str,
        host: Entity,
        host_name: &str,
        props: &[(String, PlainType)],
        pushes: &[(u32, PlainValue)],
    ) -> Vec<(u32, PlainValue)> {
        if let std::collections::hash_map::Entry::Vacant(slot) = self.panels.entry(entity) {
            // F5b: a malformed-but-loadable fragment (e.g. a tag mismatch) loads as a
            // partial tree and only warns through Noesis's parser, so capture any error
            // raised on this (render) thread during the load and surface it as a Bevy
            // error! rather than leaving a silent half-render.
            let sink = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
            let captured = std::sync::Arc::clone(&sink);
            let built = {
                let _guard = noesis_runtime::diagnostics::set_thread_error_handler(
                    move |_file, _line, message, _fatal, ctx| {
                        let at = ctx
                            .filter(|c| c.line != 0)
                            .map(|c| format!(" (line {}, col {})", c.line, c.column))
                            .unwrap_or_default();
                        captured.lock().unwrap().push(format!("{message}{at}"));
                    },
                );
                PanelEntry::build(uri, host, host_name, props)
                // guard drops here, restoring the prior handler before any ECS access
            };
            let warnings = std::mem::take(&mut *sink.lock().unwrap());
            match built {
                Some(built) => {
                    slot.insert(built);
                    self.failed_fragments.remove(&(entity, uri.to_owned()));
                    if !warnings.is_empty() {
                        error!(
                            "UiPanel {entity:?}: fragment {uri:?} loaded with parser warning(s): {}",
                            warnings.join("; "),
                        );
                    }
                }
                None => {
                    // Vacant slot retries every frame; dedupe the log to once per (entity, uri).
                    if self.failed_fragments.insert((entity, uri.to_owned())) {
                        let why = if warnings.is_empty() {
                            "unregistered/typo'd URI, or XAML Noesis rejected outright".to_owned()
                        } else {
                            warnings.join("; ")
                        };
                        error!(
                            "UiPanel {entity:?}: fragment {uri:?} failed to load: {why}. \
                             The panel will not mount until it resolves.",
                        );
                    }
                    return Vec::new();
                }
            }
        }

        if let Some(entry) = self.panels.get(&entity)
            && !pushes.is_empty()
        {
            entry.apply_pushes(pushes);
        }

        // Mount into the host scene's named Panel once it exists; re-mount after a
        // host rebuild (the stamp was reset by `teardown_scene`). Resolve the host
        // content + uri first so the `scenes` borrow ends before the `panels`
        // mutable borrow (disjoint fields).
        let mount = self
            .scenes
            .get(&host)
            .and_then(|s| s.view.content().map(|c| (c, s.built_for_uri.clone())));
        if let Some((content, host_uri)) = mount
            && let Some(entry) = self.panels.get_mut(&entity)
            && entry.mounted_for_uri.as_deref() != Some(host_uri.as_str())
        {
            match resolve_named(&content, &entry.host_name) {
                Some(host_el) => match panel_children(&host_el) {
                    Some(mut children) => {
                        record_ffi_hop();
                        if children.add(&entry.fragment).is_some() {
                            entry.mounted_for_uri = Some(host_uri);
                            self.panels_mounted_this_frame.insert(entity);
                        } else {
                            warn!(
                                "UiPanel: failed to add fragment to host {:?} for panel {entity:?}",
                                entry.host_name,
                            );
                        }
                    }
                    None => warn!(
                        "UiPanel: host {:?} is not a Panel (panel {entity:?})",
                        entry.host_name,
                    ),
                },
                None => warn!(
                    "UiPanel: host {:?} not found in scene {host_uri:?} (panel {entity:?})",
                    entry.host_name,
                ),
            }
        }

        self.panels
            .get(&entity)
            .map(PanelEntry::drain_writebacks)
            .unwrap_or_default()
    }

    /// Terminal teardown of a despawned panel entity: unmount its fragment from
    /// the host panel (so the host releases its child ref) then drop the entry in
    /// Noesis drop order (fragment → instance → class). Driven by
    /// [`teardown_removed_panels`] off `RemovedComponents<UiPanel>`. No-op for an
    /// entity that owns no panel.
    pub(crate) fn teardown_panel_for(&mut self, entity: Entity) {
        if let Some(entry) = self.panels.remove(&entity) {
            entry.unmount(&self.scenes);
            drop(entry);
        }
        self.failed_fragments.retain(|(ent, _)| *ent != entity);
        // Panels are also valid targets for the click/keydown watches (both reap
        // paths key into `self.panels`), so this terminal teardown must prune
        // their dedupe snapshots just as [`Self::teardown_for`] does for views —
        // otherwise a despawned watched panel leaks its entries forever.
        self.last_click_target.retain(|(ent, _), _| *ent != entity);
        self.last_keydown_swallow
            .retain(|(ent, _), _| *ent != entity);
    }

    /// Poll panel `entity`'s watched fragment-scope names, returning `(name,
    /// text)` for each whose text changed since the last poll. Resolves against
    /// the fragment's *own* namescope (a mounted fragment's inner names are
    /// invisible to a host-root lookup) so the watch names are fragment-local
    /// (optionally `/`-qualified within the fragment). Drives the
    /// [`crate::panel::NoesisPanelText`] read-back. No-op until the panel is built.
    pub(crate) fn poll_panel_text_for(
        &mut self,
        entity: Entity,
        watched: &[String],
    ) -> Vec<(String, String)> {
        let mut changed = Vec::new();
        let Some(entry) = self.panels.get_mut(&entity) else {
            return changed;
        };
        entry
            .text_snapshots
            .retain(|k, _| watched.iter().any(|w| w == k));
        if watched.is_empty() {
            return changed;
        }
        for name in watched {
            let Some(element) = resolve_named(&entry.fragment, name) else {
                continue;
            };
            let current = element.text().unwrap_or_default();
            if entry.text_snapshots.get(name) == Some(&current) {
                continue;
            }
            entry.text_snapshots.insert(name.clone(), current.clone());
            changed.push((name.clone(), current));
        }
        changed
    }

    /// Eagerly register every `(folder, filename)` pair currently in
    /// [`SharedFontMap`] that we haven't already handed to the C++
    /// `CachedFontProvider`. Called both before scene build (so the
    /// initial population is in place when XAML's first font lookup
    /// runs) and on every render-app sync (so fonts that arrive after
    /// scene build are picked up before they're ever requested).
    ///
    /// This bypasses Noesis's lazy `ScanFolder` model, which only fires
    /// once per folder and then caches its result, making any face that
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

        // A zero-width or -height view (minimized window, transient 0-height
        // resize) would create a zero-extent wgpu texture and abort the process
        // on validation. Skip build and resize while degenerate; any existing
        // scene stays at its last good size (invisible anyway on a 0×0 window)
        // and the resize-in-place branch below restores it once the window
        // reports a non-zero size again.
        if config.size.x == 0 || config.size.y == 0 {
            return;
        }

        // Current bytes for this URI in the shared map (`None` until the XAML
        // first lands). Cloning the `Arc` is cheap and lets us both detect a
        // hot-reload (by pointer identity) and stamp the rebuilt scene.
        let current_bytes = {
            let guard = self.shared_map.0.lock().expect("SharedXamlMap poisoned");
            guard.get(&config.xaml_uri).cloned()
        };

        // Hot-reload: the URI is unchanged but its bytes were replaced. Both
        // `update_xaml_registry` (asset `Modified`) and `XamlRegistry::insert`
        // allocate a fresh `Arc` for replaced bytes, so an `Arc::ptr_eq`
        // mismatch means the markup changed; force a full rebuild (not a
        // resize) against the new bytes. The bridges (view-models, items,
        // bindings, commands) re-attach after the rebuild via `teardown_scene`.
        let bytes_changed = matches!(
            (self.scenes.get(&entity), &current_bytes),
            (Some(scene), Some(bytes))
                if scene.built_for_uri == config.xaml_uri
                    && !Arc::ptr_eq(&scene.built_bytes, bytes)
        );

        // Same URI + bytes, different size: resize in place without tearing
        // down the View. Rebuild just the intermediate texture; `View::set_size`
        // informs Noesis without invalidating the renderer. Important for
        // desktop window drags, which fire `WindowResized` at every pixel.
        if !bytes_changed
            && let Some(scene) = self.scenes.get_mut(&entity)
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

        let up_to_date = !bytes_changed
            && self
                .scenes
                .get(&entity)
                .is_some_and(|s| s.built_for_uri == config.xaml_uri && s.size == config.size);
        if up_to_date {
            return;
        }

        // Evaluate every readiness gate BEFORE tearing down the live scene. A
        // hot-reload (`bytes_changed`) or a resize that also needs a rebuild
        // must not destroy the current scene only to bail on an unmet gate and
        // leave the view blank — P0.8 strips the ghost intermediate, so an early
        // teardown blanks rather than freezing on the last frame. Teardown is
        // deferred until all gates pass, just before the rebuild below. (The
        // explicit `xaml_uri = ""` case above still tears down eagerly.)

        // Confirm the XAML is currently present; skip if not.
        let Some(current_bytes) = current_bytes else {
            return;
        };
        // Defer scene creation until `wait_for_fonts` is satisfied (or
        // never set). Noesis's `CachedFontProvider` caches the result of
        // `ScanFolder` the first time it's called; if we build the View
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
        // wait for every URI in the chain to reach the provider;
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

        // All readiness gates passed: this is the first point at which the
        // rebuild is guaranteed to proceed, so tear down the stale scene now.
        self.teardown_scene(entity);

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

        // Application resources (theme chain + code-built entries) are installed
        // by `reconcile_app_resources` in the `Sync` phase, which runs before
        // this `Ensure` build and merges every view's `application_resources`
        // with the `NoesisResources` bridge into one dictionary. The chain-URI
        // readiness gate above kept us from reaching here until those URIs
        // resolved, and `Sync` runs after the provider map is populated, so the
        // resources are installed by now.

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
                built_bytes: current_bytes,
                applied_flags: initial_flags,
                applied_scale: config.scale,
                click_subs: HashMap::new(),
                keydown_subs: HashMap::new(),
                event_subs: HashMap::new(),
                row_click_subs: HashMap::new(),
                text_snapshots: HashMap::new(),
                dp_snapshots: HashMap::new(),
                transform_handles: HashMap::new(),
                transform_snapshots: HashMap::new(),
                transform3d_handles: HashMap::new(),
                transform3d_snapshots: HashMap::new(),
                matrix_transform3d_handles: HashMap::new(),
                matrix_transform3d_snapshots: HashMap::new(),
                brush_snapshots: HashMap::new(),
                typo_snapshots: HashMap::new(),
                input_bindings: HashMap::new(),
                predict_snapshots: HashMap::new(),
                image_snapshots: HashMap::new(),
                inline_handles: HashMap::new(),
                inlines_snapshots: HashMap::new(),
            },
        );
        self.scenes_built_this_frame.insert(entity);
    }

    /// Whether `entity`'s scene was (re)built this frame. Apply-set bridges OR
    /// this with their own change detection so a write that was set before the
    /// scene existed still lands once it does, and so a hot-reload re-seeds the
    /// current component state. See [`Self::scenes_built_this_frame`].
    pub(crate) fn scene_rebuilt_this_frame(&self, entity: Entity) -> bool {
        self.scenes_built_this_frame.contains(&entity)
    }

    /// Whether `entity`'s panel fragment first mounted this frame. The focus bridge
    /// ORs this so a once-set panel `NoesisFocus` re-applies once the fragment
    /// exists. See [`Self::panels_mounted_this_frame`].
    pub(crate) fn panel_mounted_this_frame(&self, entity: Entity) -> bool {
        self.panels_mounted_this_frame.contains(&entity)
    }

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
            let Some(mut element) = resolve_named(&content, name) else {
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
            // Panel entity: resolve in the fragment's private namescope.
            if let Some(panel) = self.panels.get(&entity) {
                for (name, &[left, top, right, bottom]) in desired {
                    let Some(mut element) = resolve_named(&panel.fragment, name) else {
                        warn!("NoesisLayout: x:Name {name:?} not found in panel fragment");
                        continue;
                    };
                    element.set_margin(left, top, right, bottom);
                }
            }
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, &[left, top, right, bottom]) in desired {
            let Some(mut element) = resolve_named(&content, name) else {
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
            let Some(element) = resolve_named(&content, name) else {
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
            let Some(element) = resolve_named(&content, name) else {
                warn!(
                    "NoesisAnimation: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let mut anim = DoubleAnimation::new();
            // From/To/Duration return false only on a type/read-only mismatch,
            // impossible on a freshly-created DoubleAnimation; ignore them.
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
        entries: &[crate::events::ClickWatchEntry],
        queue: &SharedClickQueue,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
            // Not a view: a NoesisClickWatch may sit on a panel whose fragment
            // owns a private namescope the host scene can't see.
            if self.panels.contains_key(&entity) {
                self.sync_click_subs_panel(entity, entries, queue);
            }
            return;
        };

        // Drop subscriptions that are no longer requested. `retain` runs
        // each entry's drop in place, which fires the C++ unsubscribe.
        scene
            .click_subs
            .retain(|k, _| entries.iter().any(|e| &e.name == k));

        // Re-bind any entry whose target changed (the callback captures the
        // target by value), and add brand-new ones. The sibling map keyed by
        // (view, name) records the captured target.
        let needs_change = entries.iter().any(|e| {
            let target = e.target.unwrap_or(entity);
            !scene.click_subs.contains_key(&e.name)
                || self
                    .last_click_target
                    .get(&(entity, e.name.clone()))
                    .is_none_or(|prev| *prev != target)
        });
        if !needs_change {
            return;
        }
        let Some(content) = scene.view.content() else {
            return;
        };
        for entry in entries {
            let target = entry.target.unwrap_or(entity);
            if scene.click_subs.contains_key(&entry.name)
                && self
                    .last_click_target
                    .get(&(entity, entry.name.clone()))
                    .is_some_and(|prev| *prev == target)
            {
                continue;
            }
            let Some(element) = resolve_named(&content, &entry.name) else {
                warn!(
                    "NoesisClickWatch: x:Name {:?} not found in scene {:?}",
                    entry.name, scene.built_for_uri,
                );
                continue;
            };
            let queue_handle = queue.clone();
            let captured_name = entry.name.clone();
            let Some(sub) = subscribe_click(&element, move || {
                queue_handle.push(entity, target, captured_name.clone());
            }) else {
                warn!(
                    "NoesisClickWatch: element {:?} is not a BaseButton; skipping",
                    entry.name
                );
                continue;
            };
            scene.click_subs.insert(entry.name.clone(), sub);
            self.last_click_target
                .insert((entity, entry.name.clone()), target);
        }

        // Prune this view's target snapshots whose name is no longer watched.
        self.last_click_target
            .retain(|(ent, name), _| *ent != entity || entries.iter().any(|e| &e.name == name));
    }

    /// Panel branch of [`Self::sync_click_subscriptions_for`]: a
    /// [`crate::events::NoesisClickWatch`] on a mounted `UiPanel` entity. Resolves
    /// names against the fragment's private namescope, reports the host view as the
    /// click's `view`, and defaults the target to the panel entity.
    fn sync_click_subs_panel(
        &mut self,
        entity: Entity,
        entries: &[crate::events::ClickWatchEntry],
        queue: &SharedClickQueue,
    ) {
        let Some(panel) = self.panels.get_mut(&entity) else {
            return;
        };
        let host = panel.host;
        panel
            .click_subs
            .retain(|k, _| entries.iter().any(|e| &e.name == k));
        let needs_change = entries.iter().any(|e| {
            let target = e.target.unwrap_or(entity);
            !panel.click_subs.contains_key(&e.name)
                || self
                    .last_click_target
                    .get(&(entity, e.name.clone()))
                    .is_none_or(|prev| *prev != target)
        });
        if !needs_change {
            return;
        }
        for entry in entries {
            let target = entry.target.unwrap_or(entity);
            if panel.click_subs.contains_key(&entry.name)
                && self
                    .last_click_target
                    .get(&(entity, entry.name.clone()))
                    .is_some_and(|prev| *prev == target)
            {
                continue;
            }
            let Some(element) = resolve_named(&panel.fragment, &entry.name) else {
                warn!(
                    "NoesisClickWatch: x:Name {:?} not found in panel fragment (host {host:?})",
                    entry.name,
                );
                continue;
            };
            let queue_handle = queue.clone();
            let captured_name = entry.name.clone();
            let Some(sub) = subscribe_click(&element, move || {
                queue_handle.push(host, target, captured_name.clone());
            }) else {
                warn!(
                    "NoesisClickWatch: element {:?} is not a BaseButton; skipping",
                    entry.name
                );
                continue;
            };
            panel.click_subs.insert(entry.name.clone(), sub);
            self.last_click_target
                .insert((entity, entry.name.clone()), target);
        }
        self.last_click_target
            .retain(|(ent, name), _| *ent != entity || entries.iter().any(|e| &e.name == name));
    }

    /// Reconcile the active `UIElement::KeyDown` subscription set against
    /// `entries`. Mirrors [`Self::sync_click_subscriptions`]: adds /
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
            // Panel entity: resolve in the fragment's private namescope, not the host scene.
            if self.panels.contains_key(&entity) {
                self.sync_keydown_subs_panel(entity, entries, queue);
            }
            return;
        };

        scene
            .keydown_subs
            .retain(|k, _| entries.iter().any(|e| e.name == *k));

        // Always re-bind every entry. Swallow lists may change between
        // frames and the C++-side handler captured them at subscription
        // time, so we can't update in place; we drop and re-create.
        // Cheap: a single FFI ref-bump + delegate add per entry, only on
        // the frames the watch actually changes.
        for entry in entries {
            let target = entry.target.unwrap_or(entity);
            // If the existing subscription's swallow set + target match the
            // requested ones, leave it alone. We track this on the Bevy side via
            // a sibling map keyed by name (the closure captures both by value).
            if scene.keydown_subs.contains_key(&entry.name)
                && self
                    .last_keydown_swallow
                    .get(&(entity, entry.name.clone()))
                    .is_some_and(|(swallow, prev_target)| {
                        swallow == &entry.swallow && *prev_target == target
                    })
            {
                continue;
            }

            let Some(content) = scene.view.content() else {
                return;
            };
            let Some(element) = resolve_named(&content, &entry.name) else {
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
                queue_handle.push(entity, target, captured_name.clone(), key);
                swallow.contains(&key)
            }) else {
                warn!(
                    "NoesisKeyDownWatch: element {:?} is not a UIElement; skipping",
                    entry.name
                );
                continue;
            };
            scene.keydown_subs.insert(entry.name.clone(), sub);
            self.last_keydown_swallow.insert(
                (entity, entry.name.clone()),
                (entry.swallow.clone(), target),
            );
        }

        // Prune this view's swallow snapshots whose name is no longer watched
        // (leave other views' entries intact).
        self.last_keydown_swallow
            .retain(|(ent, name), _| *ent != entity || entries.iter().any(|e| &e.name == name));
    }

    /// Panel branch of [`Self::sync_keydown_subscriptions_for`]: a
    /// [`crate::events::NoesisKeyDownWatch`] on a mounted `UiPanel` entity, resolved
    /// against the fragment's private namescope. Reports the host view; default
    /// target is the panel entity.
    fn sync_keydown_subs_panel(
        &mut self,
        entity: Entity,
        entries: &[crate::events::KeyDownWatchEntry],
        queue: &SharedKeyDownQueue,
    ) {
        let Some(panel) = self.panels.get_mut(&entity) else {
            return;
        };
        let host = panel.host;
        panel
            .keydown_subs
            .retain(|k, _| entries.iter().any(|e| e.name == *k));
        for entry in entries {
            let target = entry.target.unwrap_or(entity);
            if panel.keydown_subs.contains_key(&entry.name)
                && self
                    .last_keydown_swallow
                    .get(&(entity, entry.name.clone()))
                    .is_some_and(|(swallow, prev_target)| {
                        swallow == &entry.swallow && *prev_target == target
                    })
            {
                continue;
            }
            let Some(element) = resolve_named(&panel.fragment, &entry.name) else {
                warn!(
                    "NoesisKeyDownWatch: x:Name {:?} not found in panel fragment (host {host:?})",
                    entry.name,
                );
                continue;
            };
            let queue_handle = queue.clone();
            let captured_name = entry.name.clone();
            let swallow = entry.swallow.clone();
            let Some(sub) = subscribe_keydown(&element, move |key: Key| {
                queue_handle.push(host, target, captured_name.clone(), key);
                swallow.contains(&key)
            }) else {
                warn!(
                    "NoesisKeyDownWatch: element {:?} is not a UIElement; skipping",
                    entry.name
                );
                continue;
            };
            panel.keydown_subs.insert(entry.name.clone(), sub);
            self.last_keydown_swallow.insert(
                (entity, entry.name.clone()),
                (entry.swallow.clone(), target),
            );
        }
        self.last_keydown_swallow
            .retain(|(ent, name), _| *ent != entity || entries.iter().any(|e| &e.name == name));
    }

    /// Reconcile view `entity`'s generic `RoutedEvent` subscriptions against
    /// `entries`. Mirrors [`Self::sync_keydown_subscriptions_for`]: adds /
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

            let target = entry.target.unwrap_or(entity);
            // Leave an existing subscription alone iff its captured flags + target
            // still match the requested ones (sibling map keyed by view + name +
            // event; the callback captures all three by value).
            if scene.event_subs.contains_key(&key)
                && self
                    .last_event_config
                    .get(&(entity, entry.name.clone(), evname))
                    .is_some_and(|prev| *prev == (entry.mark_handled, entry.handled_too, target))
            {
                continue;
            }

            // Pull the content tree per change. `find_name` is cheap but the FFI
            // hop isn't free, so we only reach here on the frames the watch moved.
            let Some(content) = scene.view.content() else {
                return;
            };
            let Some(element) = resolve_named(&content, &entry.name) else {
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
                    queue_handle.push(
                        entity,
                        target,
                        captured_name.clone(),
                        captured_event,
                        snapshot,
                    );
                    mark_handled
                },
            ) else {
                warn!(
                    "NoesisEventWatch: element {:?} not a UIElement / event {:?} unknown; skipping",
                    entry.name, evname,
                );
                continue;
            };

            // Replace any stale sub (flag/target change); drop runs the C++ unsubscribe.
            scene.event_subs.insert(key, sub);
            self.last_event_config.insert(
                (entity, entry.name.clone(), evname),
                (entry.mark_handled, entry.handled_too, target),
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
            let Some(mut element) = resolve_named(&content, name) else {
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
            let Some(element) = resolve_named(&content, name) else {
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
            let Some(element) = resolve_named(&content, &watch.name) else {
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

    /// Populate each named `TextBlock`'s `Inlines` with the desired inline tree
    /// from view `entity`'s [`crate::inlines::NoesisInlines`] component. Builds
    /// the live Noesis inlines and stores their handles in the scene for the
    /// read-back. Re-apply is full replacement: any existing `Inlines` content
    /// (from an earlier apply or authored in XAML) is cleared
    /// (`InlineCollection::clear`) and rebuilt from the spec. No-op until the
    /// scene exists.
    pub(crate) fn apply_inlines_for(
        &mut self,
        entity: Entity,
        set: &HashMap<String, Vec<crate::inlines::InlineSpec>>,
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
        for (name, specs) in set {
            if specs.is_empty() {
                continue;
            }
            let Some(element) = resolve_named(&content, name) else {
                warn!(
                    "NoesisInlines: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let Some(mut collection) = noesis_runtime::text_inlines::text_block_inlines(&element)
            else {
                warn!("NoesisInlines: element {name:?} is not a TextBlock; skipped");
                continue;
            };
            // Clear any existing content, then drop our previously-built handles
            // (releasing their refs) before building the replacement so the live
            // collection holds only the new inlines.
            if collection.count() != 0 {
                collection.clear();
            }
            scene.inline_handles.remove(name);
            let built = crate::inlines::build_into(&mut collection, specs);
            scene.inline_handles.insert(name.clone(), built);
        }
    }

    /// Poll view `entity`'s watched `TextBlock`s, returning `(x:Name, readback)`
    /// for each whose live inline structure changed since last frame (deduped
    /// against the per-scene snapshot). The read re-reads the *live* collection
    /// count and pointer identity, plus the live `Run` text / `Hyperlink` URIs of
    /// the handles the bridge built. First poll after a watch is added reports.
    pub(crate) fn poll_inlines_reads_for(
        &mut self,
        entity: Entity,
        watched: &[String],
    ) -> Vec<(String, crate::inlines::InlinesReadback)> {
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene
            .inlines_snapshots
            .retain(|name, _| watched.iter().any(|w| w == name));
        if watched.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for name in watched {
            let Some(element) = resolve_named(&content, name) else {
                continue;
            };
            let Some(collection) = noesis_runtime::text_inlines::text_block_inlines(&element)
            else {
                continue;
            };
            let empty = Vec::new();
            let tree = scene.inline_handles.get(name).unwrap_or(&empty);
            let current = crate::inlines::readback(tree, &collection);
            if scene.inlines_snapshots.get(name) == Some(&current) {
                continue;
            }
            scene
                .inlines_snapshots
                .insert(name.clone(), current.clone());
            changed.push((name.clone(), current));
        }
        changed
    }

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
            // Panel entity: resolve in the fragment's private namescope (host
            // FindName can't see inside).
            if let Some(panel) = self.panels.get(&entity) {
                for (name, points) in desired {
                    let Some(mut element) = resolve_named(&panel.fragment, name) else {
                        warn!("NoesisGeometry: x:Name {name:?} not found in panel fragment");
                        continue;
                    };
                    if !element.set_path_points(points) {
                        warn!(
                            "NoesisGeometry: element {name:?} is not a Path (or < 2 points); skipped"
                        );
                    }
                }
            }
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, points) in desired {
            let Some(mut element) = resolve_named(&content, name) else {
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

    /// Build view `entity`'s desired code-built shapes (`x:Name → spec`) and
    /// assign each to its named container element. Each spec becomes a fresh
    /// Noesis `Rectangle`/`Ellipse`/`Line` (size, corner radii, optional solid
    /// fill/stroke + thickness); the container adopts it as its `Content`
    /// (`ContentControl`) or, failing that, its decorator `Child`
    /// (`Border`/`Decorator`). Noesis takes its own reference, so the Rust shape
    /// handle drops right after. Missing names (and containers that accept
    /// neither) warn. Called when the view's `NoesisShapes` component changes.
    pub(crate) fn apply_shapes_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, crate::shapes::ShapeSpec>,
    ) {
        use crate::shapes::ShapeKind;
        use noesis_runtime::brushes::SolidColorBrush;
        use noesis_runtime::shapes::{Ellipse, Line, Rectangle, Shape};

        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };

        // Apply the shared fill/stroke/thickness paint of `spec` to any built
        // `Shape`, then return it as an owning element handle ready for the tree.
        fn finish<S: Shape>(
            mut shape: S,
            spec: &crate::shapes::ShapeSpec,
        ) -> noesis_runtime::view::FrameworkElement {
            if let Some(rgba) = spec.fill {
                shape.set_fill(&SolidColorBrush::new(rgba));
            }
            if let Some(rgba) = spec.stroke {
                shape.set_stroke(&SolidColorBrush::new(rgba));
            }
            if let Some(t) = spec.stroke_thickness {
                shape.set_stroke_thickness(t);
            }
            shape.as_element()
        }

        for (name, spec) in desired {
            let Some(mut element) = resolve_named(&content, name) else {
                warn!(
                    "NoesisShapes: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let shape_el = match spec.kind {
                ShapeKind::Rectangle {
                    width,
                    height,
                    radius_x,
                    radius_y,
                } => {
                    let mut r = Rectangle::new();
                    r.set_width(width);
                    r.set_height(height);
                    r.set_radius_x(radius_x);
                    r.set_radius_y(radius_y);
                    finish(r, spec)
                }
                ShapeKind::Ellipse { width, height } => {
                    let mut e = Ellipse::new();
                    e.set_width(width);
                    e.set_height(height);
                    finish(e, spec)
                }
                ShapeKind::Line { x1, y1, x2, y2 } => {
                    let mut l = Line::new();
                    l.set_points(x1, y1, x2, y2);
                    finish(l, spec)
                }
            };
            // ContentControl `Content` first; fall back to a Decorator/Border
            // `Child`. Either takes its own reference to the shape.
            if !element.set_content(&shape_el) && !element.set_decorator_child(&shape_el) {
                warn!(
                    "NoesisShapes: container {name:?} accepts neither Content nor a decorator Child; skipped",
                );
            }
        }
    }

    /// Parse view `entity`'s [`NoesisSvg`](crate::svg::NoesisSvg) sources and
    /// size each named element to the parsed outline's measured bounds. Returns
    /// `(name, bounds)` (`bounds` = `[x, y, width, height]`) for every source
    /// that resolved to a live element and parsed: the read-back the SVG bridge
    /// turns into [`NoesisSvgChanged`](crate::svg::NoesisSvgChanged). Missing
    /// names and unparseable sources warn and are skipped (no entry).
    pub(crate) fn apply_svg_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, String>,
    ) -> Vec<(String, [f32; 4])> {
        use noesis_runtime::svg::SvgPath;

        let mut applied = Vec::new();
        if desired.is_empty() {
            return applied;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return applied;
        };
        let Some(content) = scene.view.content() else {
            return applied;
        };
        for (name, source) in desired {
            let Some(mut element) = resolve_named(&content, name) else {
                warn!(
                    "NoesisSvg: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let Some(path) = SvgPath::parse(source) else {
                warn!("NoesisSvg: source for {name:?} failed to parse; skipped");
                continue;
            };
            let bounds = path.bounds();
            // Size the element to the SVG's measured extent; a non-sizable target
            // (no Width/Height DP) still reports its bounds but warns on the set.
            if !element.set_width(bounds[2]) || !element.set_height(bounds[3]) {
                warn!(
                    "NoesisSvg: element {name:?} did not accept Width/Height; bounds still reported"
                );
            }
            applied.push((name.clone(), bounds));
        }
        applied
    }

    /// Move keyboard focus to `target` (an `x:Name`) in view `entity`, if set.
    /// Called when the view's `NoesisFocus` component changes.
    pub(crate) fn apply_focus_for(&mut self, entity: Entity, target: Option<&str>) {
        let Some(name) = target else {
            return;
        };
        let Some(scene) = self.scenes.get_mut(&entity) else {
            // Panel entity: resolve in the fragment's private namescope.
            if let Some(panel) = self.panels.get(&entity) {
                match resolve_named(&panel.fragment, name) {
                    Some(mut element) => {
                        if !element.focus() {
                            warn!("NoesisFocus: element {name:?} refused focus (non-focusable?)");
                        }
                    }
                    None => warn!("NoesisFocus: x:Name {name:?} not found in panel fragment"),
                }
            }
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        let Some(mut element) = resolve_named(&content, name) else {
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
    /// focus warns (non-traversable direction / no neighbour). Returns `false`
    /// when the target root wasn't ready to receive the actions (scene not yet
    /// built / no content, or panel fragment not yet mounted), so the caller can
    /// keep the one-shots queued until the mount/build frame instead of dropping
    /// them; an empty `moves` slice reports `true` (nothing to do).
    pub(crate) fn apply_focus_moves_for(
        &mut self,
        entity: Entity,
        moves: &[crate::focus_input::FocusMove],
    ) -> bool {
        if moves.is_empty() {
            return true;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            // Panel entity: resolve in the fragment's private namescope.
            let Some(panel) = self.panels.get(&entity) else {
                return false;
            };
            if panel.mounted_for_uri.is_none() {
                // Fragment exists but isn't in the visual tree yet; focus can't
                // move on a detached element — retry once it mounts.
                return false;
            }
            for m in moves {
                let Some(mut element) = resolve_named(&panel.fragment, &m.from) else {
                    warn!(
                        "NoesisFocusControl: move-from x:Name {:?} not found in panel fragment",
                        m.from,
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
            return true;
        };
        let Some(content) = scene.view.content() else {
            return false;
        };
        for m in moves {
            let Some(mut element) = resolve_named(&content, &m.from) else {
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
        true
    }

    /// Apply view `entity`'s one-shot focus-engagement actions
    /// (`UIElement::Focus(engage)`). Returns `false` when the target root wasn't
    /// ready (see [`Self::apply_focus_moves_for`]); an empty slice reports `true`.
    pub(crate) fn apply_focus_engages_for(
        &mut self,
        entity: Entity,
        engages: &[crate::focus_input::FocusEngage],
    ) -> bool {
        if engages.is_empty() {
            return true;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            // Panel entity: resolve in the fragment's private namescope.
            let Some(panel) = self.panels.get(&entity) else {
                return false;
            };
            if panel.mounted_for_uri.is_none() {
                // Not in the visual tree yet; retry once it mounts.
                return false;
            }
            for e in engages {
                let Some(mut element) = resolve_named(&panel.fragment, &e.name) else {
                    warn!(
                        "NoesisFocusControl: engage x:Name {:?} not found in panel fragment",
                        e.name,
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
            return true;
        };
        let Some(content) = scene.view.content() else {
            return false;
        };
        for e in engages {
            let Some(mut element) = resolve_named(&content, &e.name) else {
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
        true
    }

    /// Reconcile view `entity`'s `KeyBinding`s against `specs`. Each binding's
    /// command callback pushes `(entity, name, key, modifiers)` onto `queue`, so
    /// the emitted `NoesisFocusBindingFired` carries the originating view.
    /// Bindings already installed are left alone; bindings dropped from `specs`
    /// are detached from their element via `KeyBinding::remove_from` and then
    /// forgotten (releasing our `+1` references). Mirrors
    /// `sync_click_subscriptions_for`.
    pub(crate) fn sync_key_bindings_for(
        &mut self,
        entity: Entity,
        specs: &[crate::focus_input::KeyBindingSpec],
        queue: &crate::focus_input::SharedFocusBindingQueue,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };

        // Idents installed but no longer requested: detach + forget these.
        let dropped: Vec<(String, i32, i32)> = scene
            .input_bindings
            .keys()
            .filter(|k| !specs.iter().any(|s| &s.ident() == *k))
            .cloned()
            .collect();
        let needs_new = specs
            .iter()
            .any(|s| !scene.input_bindings.contains_key(&s.ident()));
        if dropped.is_empty() && !needs_new {
            return;
        }
        let Some(content) = scene.view.content() else {
            return;
        };

        // Detach dropped bindings from their element, then drop our refs. The
        // element name is the ident's first field; `remove_from` is a no-op if
        // the element vanished. Detaching before drop ensures the chord stops
        // firing immediately, not just when the scene tears down.
        for ident in dropped {
            if let Some(installed) = scene.input_bindings.remove(&ident)
                && let Some(element) = resolve_named(&content, &ident.0)
            {
                installed.binding.remove_from(&element);
            }
        }

        for spec in specs {
            let ident = spec.ident();
            if scene.input_bindings.contains_key(&ident) {
                continue;
            }
            let Some(element) = resolve_named(&content, &spec.name) else {
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
    /// `(from, direction, candidate, predicted_name, matches_expected)` when the
    /// answer changed since last frame (deduped against the per-scene snapshot).
    /// First poll after a watch is added always reports. `predicted_name` is the
    /// predicted element's actual `x:Name` (via
    /// `FrameworkElement::predict_focus_name`); `matches_expected` is `true` when
    /// that name equals the watch's `expect`. Mirrors `poll_dp_reads_for`.
    pub(crate) fn poll_focus_predictions_for(
        &mut self,
        entity: Entity,
        predicts: &[crate::focus_input::FocusPredict],
    ) -> Vec<(
        String,
        crate::focus_input::FocusNavigationDirection,
        bool,
        Option<String>,
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
            let Some(from) = resolve_named(&content, &p.from) else {
                continue;
            };
            let candidate = from.predict_focus(p.direction).is_some();
            let predicted_name = from.predict_focus_name(p.direction);
            let matches_expected = match &p.expect {
                Some(expect) => predicted_name.as_deref() == Some(expect.as_str()),
                None => false,
            };
            let ident = p.ident();
            let snapshot = (candidate, predicted_name.clone(), matches_expected);
            if scene.predict_snapshots.get(&ident) == Some(&snapshot) {
                continue;
            }
            scene.predict_snapshots.insert(ident, snapshot);
            changed.push((
                p.from.clone(),
                p.direction,
                candidate,
                predicted_name,
                matches_expected,
            ));
        }
        changed
    }

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
            let Some(element) = resolve_named(&content, name) else {
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
            let Some(mut element) = resolve_named(&content, name) else {
                warn!(
                    "NoesisDp: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            record_ffi_hop();
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
            let Some(element) = resolve_named(&content, &watch.name) else {
                continue;
            };
            record_ffi_hop();
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
            // Panel entity: resolve in the fragment's private namescope. Write-only:
            // the +1 RenderTransform handle isn't retained (panels have no transform
            // poll), but Noesis keeps its own ref.
            if let Some(panel) = self.panels.get(&entity) {
                for (name, spec) in desired {
                    let Some(mut element) = resolve_named(&panel.fragment, name) else {
                        warn!("NoesisTransform: x:Name {name:?} not found in panel fragment");
                        continue;
                    };
                    let transform = CompositeTransform::new(spec.to_fields());
                    if !element.set_render_transform(&transform) {
                        warn!(
                            "NoesisTransform: {name:?} has no RenderTransform (not a UIElement?) in panel fragment"
                        );
                    }
                }
            }
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
            let Some(mut element) = resolve_named(&content, name) else {
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

    /// Assign view `entity`'s desired `Transform3D`s (`x:Name → spec`). Each
    /// spec becomes a `CompositeTransform3D` held at +1 in
    /// [`Self::transform3d_handles`] (the same object Noesis stores), so the
    /// poll can read it back. Missing names / non-`UIElement` targets warn.
    /// Mirror of [`Self::apply_transforms_for`], but for `UIElement::Transform3D`
    /// rather than `RenderTransform`.
    pub(crate) fn apply_transforms3d_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, crate::transforms3d::Transform3DSpec>,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        // Drop handles for names no longer requested; releasing each handle's +1
        // (Noesis still holds its own ref until the DP is overwritten / cleared).
        scene
            .transform3d_handles
            .retain(|k, _| desired.contains_key(k));
        if desired.is_empty() {
            return;
        }
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, spec) in desired {
            let Some(mut element) = resolve_named(&content, name) else {
                warn!(
                    "NoesisTransform3D: x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let transform = CompositeTransform3D::new(spec.to_fields());
            if element.set_transform3d(&transform) {
                scene.transform3d_handles.insert(name.clone(), transform);
            } else {
                warn!(
                    "NoesisTransform3D: {name:?} has no Transform3D (not a UIElement?) \
                     in scene {:?}",
                    scene.built_for_uri,
                );
            }
        }
    }

    /// Poll view `entity`'s named elements' live `Transform3D`s, returning
    /// `(name, spec)` for each that changed since last frame (deduped against the
    /// per-scene snapshot). A name only reports while the element's current
    /// `Transform3D` is the exact object we assigned (pointer identity), so the
    /// read-back is element-sourced proof the assignment took, not an echo of
    /// the component. First poll after assignment always reports. Mirror of
    /// [`Self::poll_transforms_for`].
    pub(crate) fn poll_transforms3d_for(
        &mut self,
        entity: Entity,
        names: &[&str],
    ) -> Vec<(String, crate::transforms3d::Transform3DSpec)> {
        use crate::transforms3d::Transform3DSpec;
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene
            .transform3d_snapshots
            .retain(|name, _| names.contains(&name.as_str()));
        if names.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for &name in names {
            let Some(handle) = scene.transform3d_handles.get(name) else {
                continue;
            };
            let Some(element) = resolve_named(&content, name) else {
                continue;
            };
            // Read the element's live Transform3D; only trust it when it is the
            // very object we assigned (Noesis stores our pointer, no clone).
            let Some(live) = element.transform3d() else {
                continue;
            };
            if live.raw() != handle.raw() {
                continue;
            }
            let current = Transform3DSpec::from_fields(handle.get());
            if scene.transform3d_snapshots.get(name) == Some(&current) {
                continue;
            }
            scene
                .transform3d_snapshots
                .insert(name.to_string(), current);
            changed.push((name.to_string(), current));
        }
        changed
    }

    /// Assign view `entity`'s desired raw 3D matrix transforms (`x:Name → 12
    /// `Transform3` floats`). Each becomes a `MatrixTransform3D` held at +1 in
    /// [`Self::matrix_transform3d_handles`] (the same object Noesis stores), so
    /// the poll can read it back. Missing names / non-`UIElement` targets warn.
    /// Matrix analogue of [`Self::apply_transforms3d_for`]; both set the single
    /// `UIElement::Transform3D` DP, so a name given both kinds keeps whichever
    /// applied last.
    pub(crate) fn apply_matrix_transforms3d_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, crate::transforms3d::Matrix3DSpec>,
    ) {
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        // Drop handles for names no longer requested; releasing each handle's +1.
        scene
            .matrix_transform3d_handles
            .retain(|k, _| desired.contains_key(k));
        if desired.is_empty() {
            return;
        }
        let Some(content) = scene.view.content() else {
            return;
        };
        for (name, spec) in desired {
            let Some(mut element) = resolve_named(&content, name) else {
                warn!(
                    "NoesisTransform3D(matrix): x:Name {:?} not found in scene {:?}",
                    name, scene.built_for_uri,
                );
                continue;
            };
            let transform = MatrixTransform3D::new(spec.rows);
            if element.set_transform3d(&transform) {
                scene
                    .matrix_transform3d_handles
                    .insert(name.clone(), transform);
            } else {
                warn!(
                    "NoesisTransform3D(matrix): {name:?} has no Transform3D (not a UIElement?) \
                     in scene {:?}",
                    scene.built_for_uri,
                );
            }
        }
    }

    /// Poll view `entity`'s named elements' live raw 3D matrix transforms,
    /// returning `(name, matrix)` for each that changed since last frame (deduped
    /// against the per-scene snapshot). A name only reports while the element's
    /// current `Transform3D` is the exact `MatrixTransform3D` we assigned (pointer
    /// identity), so the read-back is element-sourced proof the assignment took.
    /// First poll after assignment always reports. Matrix analogue of
    /// [`Self::poll_transforms3d_for`].
    pub(crate) fn poll_matrix_transforms3d_for(
        &mut self,
        entity: Entity,
        names: &[&str],
    ) -> Vec<(String, [f32; 12])> {
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene
            .matrix_transform3d_snapshots
            .retain(|name, _| names.contains(&name.as_str()));
        if names.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for &name in names {
            let Some(handle) = scene.matrix_transform3d_handles.get(name) else {
                continue;
            };
            let Some(element) = resolve_named(&content, name) else {
                continue;
            };
            // Trust the value only when the element's live Transform3D is the very
            // object we assigned (Noesis stores our pointer, no clone).
            let Some(live) = element.transform3d() else {
                continue;
            };
            if live.raw() != handle.raw() {
                continue;
            }
            let current = handle.get();
            if scene.matrix_transform3d_snapshots.get(name) == Some(&current) {
                continue;
            }
            scene
                .matrix_transform3d_snapshots
                .insert(name.to_string(), current);
            changed.push((name.to_string(), current));
        }
        changed
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
            let Some(mut element) = resolve_named(&content, name) else {
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

    /// Build view `entity`'s desired code-built styles (`x:Name → spec`) and
    /// assign each to its named element via `FrameworkElement::set_style`. Each
    /// spec becomes a fresh `Noesis::Style` (target type + setters + property
    /// triggers); Noesis takes its own reference, so the Rust handle is dropped
    /// right after. A `Style` is sealed on first apply, so rebuilding per change
    /// is correct. Missing names / unknown target types / unresolvable
    /// properties warn. Called when the view's `NoesisStyles` component changes.
    pub(crate) fn apply_styles_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, crate::styles::StyleSpec>,
    ) {
        if desired.is_empty() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return;
        };
        let uri = scene.built_for_uri.clone();
        let Some(content) = scene.view.content() else {
            return;
        };

        for (name, spec) in desired {
            let Some(mut element) = resolve_named(&content, name) else {
                warn!("NoesisStyles: x:Name {name:?} not found in scene {uri:?}");
                continue;
            };
            let Some(style) = build_noesis_style(spec, name, &uri) else {
                continue;
            };
            if !element.set_style(&style) {
                warn!("NoesisStyles: {name:?} is not a FrameworkElement in scene {uri:?}");
            }
        }
    }

    /// Reconcile the single process-global application resources dictionary from
    /// every source that feeds it: the code-built `entries` and `merged_xaml` of
    /// the [`NoesisResources`](crate::resources::NoesisResources) bridge, plus
    /// the `chain_uris` collected from the views' `application_resources`. All
    /// three feed one process-global dictionary, so opting into a theme (a URI
    /// chain) no longer clobbers code-built brushes/values (and vice versa) — the
    /// old two-installer design let whichever ran last in the frame win.
    ///
    /// Two install paths: a pure-chain config (no `entries`/`merged_xaml`) goes
    /// through `install_app_resources_chain` so cross-leaf `{StaticResource}`s
    /// resolve in dependency order; anything with code-built inputs is merged into
    /// one `ResourceDictionary` (base `entries` win over merged, per WPF) and
    /// installed with `GUI::SetApplicationResources`.
    ///
    /// Returns `Some(present)` — the declared `entries` keys confirmed resolvable
    /// through the live application resources, sorted — only when this call
    /// actually (re)installed; `None` when the merged inputs are unchanged since
    /// the last install or when a `chain_uris` entry hasn't reached the XAML
    /// provider yet (retried next frame). Called from the `Sync` phase (before
    /// scene build) so a scene's `{StaticResource}` resolves at parse time.
    pub(crate) fn reconcile_app_resources(
        &mut self,
        entries: &HashMap<String, crate::resources::ResourceEntry>,
        merged_xaml: &[String],
        chain_uris: &[String],
    ) -> Option<Vec<String>> {
        use crate::brushes::BrushSpec;
        use crate::resources::ResourceEntry;
        use noesis_runtime::brushes::{GradientStop, LinearGradientBrush, SolidColorBrush};
        use noesis_runtime::resources::{
            ResourceDictionary, application_resources_contains, set_application_resources,
        };

        if entries.is_empty() && merged_xaml.is_empty() && chain_uris.is_empty() {
            return None;
        }

        // Cheap unchanged-check first: this runs every frame, so bail before
        // locking the provider map or cloning the spec when nothing changed.
        // (Keyed on the URI *list*, like the previous chain installer — an
        // in-place hot-reload of a chain dictionary's bytes isn't reinstalled.)
        if self.installed_app_resources.as_ref().is_some_and(|s| {
            s.entries == *entries && s.merged_xaml == merged_xaml && s.chain_uris == chain_uris
        }) {
            return None;
        }

        // Snapshot the chain bytes up front and drop the map lock before parsing:
        // `ResourceDictionary::parse` re-enters our XAML provider to resolve each
        // dictionary's nested `Source="..."`, which locks the same map. Defer the
        // whole install until every chain URI has reached the provider so the
        // merged theme installs atomically (the scene build gates on the same
        // URIs, so no scene parses against a half-installed chain).
        let chain_sources: Vec<(String, String)> = {
            let guard = self.shared_map.0.lock().expect("SharedXamlMap poisoned");
            let mut sources = Vec::with_capacity(chain_uris.len());
            for uri in chain_uris {
                let bytes = guard.get(uri)?;
                sources.push((uri.clone(), String::from_utf8_lossy(bytes).into_owned()));
            }
            sources
        };

        // Pure-chain config (no code-built base entries or `merged_xaml`): install
        // via the runtime chain installer, which wires each leaf into the parent's
        // `MergedDictionaries` *before* `SetSource` so a `{StaticResource}` in a
        // later leaf that references an earlier leaf's key resolves at parse time.
        // Re-parsing each leaf standalone (the merge path below) can't do that —
        // a leaf parses with no sibling scope, null-resolving cross-leaf refs. The
        // installer re-resolves each URI through the same provider; the byte
        // snapshot above already gated on every URI being present, so this can't
        // half-install. (Bytes dropped: the installer re-reads them by URI.)
        if entries.is_empty() && merged_xaml.is_empty() {
            if !noesis_runtime::gui::install_app_resources_chain(chain_uris) {
                warn!(
                    "NoesisResources: failed to install application-resources chain {chain_uris:?}"
                );
            }
            self.installed_app_resources = Some(AppResourcesSnapshot {
                entries: entries.clone(),
                merged_xaml: merged_xaml.to_vec(),
                chain_uris: chain_uris.to_vec(),
            });
            info!(
                "Installed Noesis application resources: {} chain dicts (dependency-ordered)",
                chain_uris.len(),
            );
            return Some(Vec::new());
        }

        let mut dict = ResourceDictionary::new();

        // Merged dictionaries first (URI chain, then the bridge's `merged_xaml`);
        // code-built `entries` are added as base entries afterwards, so they win
        // on a key collision (base takes precedence over merged, per WPF). Among
        // merged dictionaries the later-added wins, so `merged_xaml` overrides the
        // theme chain. NOTE: with code-built `entries`/`merged_xaml` present each
        // chain leaf is re-parsed standalone here, so a `{StaticResource}` that
        // crosses two chain leaves won't resolve (unlike the pure-chain path
        // above). Fixing that for mixed configs needs a runtime FFI that returns
        // the chain parent so base entries can be injected into it.
        for (uri, xaml) in &chain_sources {
            match ResourceDictionary::parse(xaml) {
                Some(leaf) => {
                    if !dict.add_merged(&leaf) {
                        warn!("NoesisResources: failed to merge chain dictionary {uri:?}");
                    }
                }
                None => warn!(
                    "NoesisResources: chain URI {uri:?} did not parse as a ResourceDictionary"
                ),
            }
        }
        for xaml in merged_xaml {
            match ResourceDictionary::parse(xaml) {
                Some(merged) => {
                    if !dict.add_merged(&merged) {
                        warn!("NoesisResources: failed to merge a parsed ResourceDictionary");
                    }
                }
                None => warn!(
                    "NoesisResources: a merged_xaml fragment did not parse as a <ResourceDictionary>",
                ),
            }
        }

        for (key, entry) in entries {
            let ok = match entry {
                ResourceEntry::Value(value) => dict.add_boxed(key, &value.to_boxed()),
                ResourceEntry::Brush(BrushSpec::Solid(rgba)) => {
                    dict.add_brush(key, &SolidColorBrush::new(*rgba))
                }
                ResourceEntry::Brush(BrushSpec::LinearGradient { start, end, stops }) => {
                    let mut brush = LinearGradientBrush::new();
                    brush.set_start_point(start[0], start[1]);
                    brush.set_end_point(end[0], end[1]);
                    for stop in stops {
                        brush.add_stop(GradientStop::new(stop.offset, stop.color));
                    }
                    dict.add_brush(key, &brush)
                }
            };
            if !ok {
                warn!("NoesisResources: failed to add resource {key:?}");
            }
        }

        set_application_resources(&dict);
        self.installed_app_resources = Some(AppResourcesSnapshot {
            entries: entries.clone(),
            merged_xaml: merged_xaml.to_vec(),
            chain_uris: chain_uris.to_vec(),
        });

        // Confirm against the live global (now our dict) so the read-back proves
        // the install took, not just that we built the spec.
        let mut present: Vec<String> = entries
            .keys()
            .filter(|key| application_resources_contains(key))
            .cloned()
            .collect();
        present.sort();
        info!(
            "Installed Noesis application resources: {} entries, {} merged dicts, {} chain dicts, {} present",
            entries.len(),
            merged_xaml.len(),
            chain_sources.len(),
            present.len(),
        );
        Some(present)
    }

    /// Poll view `entity`'s named elements' live `RenderTransform`s, returning
    /// `(name, spec)` for each that changed since last frame (deduped against the
    /// per-scene snapshot). A name only reports while the element's current
    /// `RenderTransform` is the exact object we assigned (pointer identity), so
    /// the read-back is element-sourced proof the assignment took, not an echo
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
            let Some(element) = resolve_named(&content, name) else {
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
            let Some(element) = resolve_named(&content, name) else {
                continue;
            };
            let property = target.property();
            // Solid: read the exact color. Otherwise, if a brush is present at
            // all (non-null DP), it's a non-solid brush (e.g. a gradient), the
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

    /// Poll view `entity`'s watched `<Image>` elements, returning `(name,
    /// readback)` for each whose resolved size / source presence changed since
    /// last frame (deduped against the per-scene snapshot). The read-back is
    /// element-sourced: `ActualWidth`/`ActualHeight` come from the live layout,
    /// which Noesis derives from the source's pixel size via our texture
    /// provider's `GetTextureInfo`. So once a staged bitmap resolves, a
    /// `Stretch="None"` element reports the bitmap's exact dimensions; an
    /// unresolvable source reports `[0.0, 0.0]`. First poll after a name is
    /// watched always reports.
    pub(crate) fn poll_image_reads_for(
        &mut self,
        entity: Entity,
        desired: &HashMap<String, crate::imaging::ImageBitmap>,
    ) -> Vec<(String, crate::imaging::ImageReadback)> {
        use crate::imaging::ImageReadback;
        let mut changed = Vec::new();
        let Some(scene) = self.scenes.get_mut(&entity) else {
            return changed;
        };
        scene
            .image_snapshots
            .retain(|name, _| desired.contains_key(name));
        if desired.is_empty() {
            return changed;
        }
        let Some(content) = scene.view.content() else {
            return changed;
        };
        for name in desired.keys() {
            let Some(element) = resolve_named(&content, name) else {
                continue;
            };
            let current = ImageReadback {
                has_source: element.image_source().is_some(),
                actual_size: [
                    element.actual_width().unwrap_or(0.0),
                    element.actual_height().unwrap_or(0.0),
                ],
            };
            if scene.image_snapshots.get(name) == Some(&current) {
                continue;
            }
            scene.image_snapshots.insert(name.clone(), current);
            changed.push((name.clone(), current));
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

    /// Apply a batch of queued input events onto their target View. Each event
    /// carries the view its coordinates were converted against (or `None` for
    /// the primary view); routing to that same view keeps hit-testing consistent
    /// when several views coexist. No-op when the target scene hasn't been built
    /// yet; such events are dropped, which is fine: pre-scene input targets
    /// nothing.
    fn apply_input(&mut self, events: &[crate::input::TargetedInput]) {
        use crate::input::NoesisInputEvent as E;
        // The primary view is the deterministic fallback for untargeted events
        // (keyboard, focus, programmatic pushes): the lowest-`Entity` live scene.
        // `values_mut().next()` is HashMap order and unstable across insertions,
        // so pick by `Entity` — the same rule the coordinate forwarders use.
        let Some(primary) = self.scenes.keys().min().copied() else {
            // No live scenes: nothing can be under the pointer, so drop any stale
            // "over UI" state (a view despawned while the pointer was over it must
            // not keep suppressing 3D interaction). See also `apply_noesis_input`,
            // which resets even on frames with no queued events.
            self.pointer_over_ui = false;
            return;
        };
        if self.scenes.len() > 1 {
            bevy::log::warn_once!(
                "Noesis input routes to the primary view (lowest Entity); per-view \
                 pointer routing across {} views is not implemented yet",
                self.scenes.len()
            );
        }
        // Last pointer hit-test wins; seed from prior so a still cursor persists.
        let mut over_ui = self.pointer_over_ui;
        for targeted in events {
            let Some(scene) = self.scenes.get_mut(&targeted.target.unwrap_or(primary)) else {
                continue;
            };
            match targeted.event {
                E::MouseMove { x, y } => {
                    over_ui = scene.view.mouse_move(x, y);
                }
                E::MouseButton {
                    down: true,
                    x,
                    y,
                    button,
                } => {
                    over_ui = scene.view.mouse_button_down(x, y, button);
                }
                E::MouseButton {
                    down: false,
                    x,
                    y,
                    button,
                } => {
                    over_ui = scene.view.mouse_button_up(x, y, button);
                }
                E::MouseWheel { x, y, delta } => {
                    over_ui = scene.view.mouse_wheel(x, y, delta);
                }
                E::MouseHWheel { x, y, delta } => {
                    over_ui = scene.view.mouse_hwheel(x, y, delta);
                }
                E::Scroll {
                    x,
                    y,
                    value,
                    horizontal: false,
                } => {
                    over_ui = scene.view.scroll(x, y, value);
                }
                E::Scroll {
                    x,
                    y,
                    value,
                    horizontal: true,
                } => {
                    over_ui = scene.view.hscroll(x, y, value);
                }
                E::TouchDown { x, y, id } => {
                    over_ui = scene.view.touch_down(x, y, id);
                }
                E::TouchMove { x, y, id } => {
                    over_ui = scene.view.touch_move(x, y, id);
                }
                E::TouchUp { x, y, id } => {
                    over_ui = scene.view.touch_up(x, y, id);
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
        self.pointer_over_ui = over_ui;
    }

    /// Drive one Noesis frame into the intermediate. Call during the
    /// `Render` schedule (before `NoesisNode::run`) so the intermediate
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
            // Lazy renderer init: needs both the live View and the registered
            // device, so it happens on first frame here (not at scene creation).
            if !scene.renderer_initialized {
                let mut renderer = scene.view.renderer();
                renderer.init(registered_device);
                // Don't call renderer.shutdown() here; keep the init live.
                scene.renderer_initialized = true;
            }

            // Paint into this frame's back buffer (the one the render thread is
            // not currently blitting). `publish_intermediates` flips the index.
            registered_device
                .device_mut::<WgpuRenderDevice>()
                .set_onscreen_target(
                    scene.intermediates[scene.write_index].view.clone(),
                    scene.size.x,
                    scene.size.y,
                );

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
    /// as a [`NoesisIntermediate`] component (the main→render handoff; only
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
            self.published_intermediates.insert(entity);
        }
        // Strip the intermediate off entities whose scene was torn down but that
        // survive as entities (xaml_uri cleared, uri swapped to not-yet-loaded
        // bytes, readiness gate re-blocked). Without this the render world keeps
        // extracting and blitting the last-painted frame — a frozen UI ghost.
        // `teardown_for` already drops the component for despawned entities via
        // Bevy's own reaping, but a *surviving* entity needs it removed here.
        let scenes = &self.scenes;
        self.published_intermediates.retain(|&entity| {
            if scenes.contains_key(&entity) {
                return true;
            }
            commands.entity(entity).remove::<NoesisIntermediate>();
            false
        });
    }

    /// Render label template `xaml_uri` (with `fields` applied to named
    /// `TextBlock`/`TextBox` elements) into `target` at `size`, reusing one
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
                if let Some(mut element) = resolve_named(&content, name) {
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
            .set_onscreen_target(target.clone(), size.x, size.y);

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
        for ((ent, _), binding) in &mut self.lists {
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
        for ((ent, _, _), entry) in &mut self.binding_entries {
            if *ent == entity {
                entry.reset_bind();
            }
        }
        // Panels mounted into *this* view's scene lose their host child on the
        // rebuild; reset their stamp so the next `sync_panel` re-mounts them into
        // the fresh tree. (Keyed by host, not by the scene entity.)
        for entry in self.panels.values_mut() {
            if entry.host == entity {
                entry.mounted_for_uri = None;
            }
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

    /// Number of live scene instances. Mirrors `self.scenes.len()`; surfaced
    /// through [`NoesisDiagnostics`](crate::diagnostics::NoesisDiagnostics::live_scenes)
    /// so a despawn-leak test can assert the side table drains back to baseline.
    #[must_use]
    pub(crate) fn live_scene_count(&self) -> usize {
        self.scenes.len()
    }

    /// Number of live mounted panels. Mirrors `self.panels.len()`; surfaced
    /// through [`NoesisDiagnostics`](crate::diagnostics::NoesisDiagnostics::live_panels)
    /// so a despawn-reap test can assert the panel table drains back to baseline.
    #[must_use]
    pub(crate) fn live_panel_count(&self) -> usize {
        self.panels.len()
    }

    /// Fully reap every Noesis resource owned on behalf of `entity` (its scene
    /// *and* every per-entity side-table entry) when the view (or, later, panel)
    /// is despawned or loses its [`NoesisView`]. Unlike [`Self::teardown_scene`]
    /// (a *rebuild* step that drops only the scene and leaves the view models /
    /// collections / bindings parked for re-attach), this is the *terminal*
    /// teardown: it drops the owners too, so nothing leaks once the entity is gone.
    ///
    /// Drop order mirrors [`Drop`]: the scene (`View`) first, because it holds
    /// refs to the VM `ClassInstance`s and `ObservableCollection`s set as its
    /// `DataContext`/`ItemsSource`; only once those refs release is it safe to
    /// drop the owners (or a live View-held instance would outlive the
    /// `ClassRegistration` that unregisters its class → use-after-free). The
    /// scratch dedupe tables are keyed by entity and just dropped. No-op for an
    /// entity we never tracked.
    pub(crate) fn teardown_for(&mut self, entity: Entity) {
        self.teardown_scene(entity);
        self.view_models.remove(&entity);
        self.items_sources.retain(|(ent, _), _| *ent != entity);
        self.lists.retain(|(ent, _), _| *ent != entity);
        self.plain_vms.retain(|(ent, _), _| *ent != entity);
        self.command_hosts.remove(&entity);
        self.warned_host_build_failures
            .retain(|(ent, _)| *ent != entity);
        self.warned_dc_collisions.retain(|(ent, _)| *ent != entity);
        self.binding_entries.retain(|(ent, _, _), _| *ent != entity);
        self.last_keydown_swallow
            .retain(|(ent, _), _| *ent != entity);
        self.last_click_target.retain(|(ent, _), _| *ent != entity);
        self.last_event_config
            .retain(|(ent, _, _), _| *ent != entity);
        self.scenes_built_this_frame.remove(&entity);
        // Drop the publish record so a since-despawned entity is never targeted
        // by `publish_intermediates`' stale-ghost sweep (Bevy already reaps the
        // component off the dead entity; a `remove` command on it would panic).
        self.published_intermediates.remove(&entity);
    }

    // ─────────────────────────────────────────────────────────────────────────
    // Per-bridge component-removal reaps
    //
    // [`Self::teardown_for`] / [`Self::teardown_panel_for`] cover a whole entity
    // being despawned (or losing its `NoesisView` / `UiPanel`). These cover the
    // narrower case an audit flagged as leaking everywhere: a *bridge* component
    // dropped off an entity whose view stays live. Each is invoked from that
    // bridge's `RemovedComponents<C>` reap system (see [`ReapOnRemove`] /
    // [`add_bridge_reap`]) and MUST be idempotent with the terminal teardown: on
    // a full despawn both fire in the same `NoesisSet::Ensure` head, and every
    // one below is a plain `remove`/`retain`/`clear` that no-ops once the scene
    // (and its side-table entries) are already gone.
    // ─────────────────────────────────────────────────────────────────────────

    /// Detach `target`'s live `DataContext` in view `entity`'s scene (set it to
    /// null), releasing the View's ref to whatever host instance we attached
    /// there. Shared by the VM / commands / plain-VM removal reaps: unlike a
    /// despawn (where [`Self::teardown_scene`] drops the whole `View` first, so
    /// its refs release on their own), a component-removal reap runs while the
    /// scene is still live — so the host `ClassInstance` must be released off the
    /// View *before* its owning entry (and thus that entry's `ClassRegistration`)
    /// drops, or the class unregisters under a live instance (use-after-free, the
    /// exact hazard [`Self::teardown_for`]'s drop order avoids). No-op when the
    /// scene or the target element is gone.
    fn clear_host_data_context(&self, entity: Entity, target: &AttachTarget) {
        let Some(scene) = self.scenes.get(&entity) else {
            return;
        };
        let Some(content) = scene.view.content() else {
            return;
        };
        let element = match target {
            AttachTarget::Root => Some(content),
            AttachTarget::Named(name) => resolve_named(&content, name),
        };
        if let Some(mut element) = element {
            element.clear_data_context();
        }
    }

    /// Reap view `entity`'s [`NoesisVm`](crate::viewmodel::NoesisVm): detach its
    /// `DataContext` off the live scene, then drop the owning entry.
    pub(crate) fn reap_view_model_for(&mut self, entity: Entity) {
        if let Some(entry) = self.view_models.get(&entity)
            && self
                .scenes
                .get(&entity)
                .is_some_and(|s| !entry.needs_attach(&s.built_for_uri))
        {
            self.clear_host_data_context(entity, entry.target());
        }
        self.view_models.remove(&entity);
        self.warned_host_build_failures
            .remove(&(entity, "NoesisVm"));
        self.warned_dc_collisions.retain(|(ent, _)| *ent != entity);
    }

    /// Reap view `entity`'s [`NoesisCommands`](crate::commands::NoesisCommands):
    /// detach its `DataContext` off the live scene, then drop the command host.
    pub(crate) fn reap_commands_for(&mut self, entity: Entity) {
        if let Some(entry) = self.command_hosts.get(&entity)
            && self
                .scenes
                .get(&entity)
                .is_some_and(|s| !entry.needs_attach(&s.built_for_uri))
        {
            self.clear_host_data_context(entity, entry.target());
        }
        self.command_hosts.remove(&entity);
        self.warned_host_build_failures
            .remove(&(entity, "NoesisCommands"));
        self.warned_dc_collisions.retain(|(ent, _)| *ent != entity);
    }

    /// Reap view `entity`'s plain-struct view model of type `type_id`: detach its
    /// `DataContext` off the live scene, then drop the entry (whose sink was
    /// otherwise left accumulating UI writebacks nobody drains).
    pub(crate) fn reap_plain_vm_for(&mut self, entity: Entity, type_id: std::any::TypeId) {
        let key = (entity, type_id);
        if let Some(entry) = self.plain_vms.get(&key)
            && self
                .scenes
                .get(&entity)
                .is_some_and(|s| !entry.needs_attach(&s.built_for_uri))
        {
            self.clear_host_data_context(entity, entry.target());
        }
        self.plain_vms.remove(&key);
        self.warned_dc_collisions.retain(|(ent, _)| *ent != entity);
    }

    /// Reap view/panel `entity`'s [`NoesisClickWatch`](crate::events::NoesisClickWatch):
    /// drop its live click subscriptions (each drop fires the C++ unsubscribe, so
    /// they stop pushing onto the shared queue) and forget its target snapshots.
    pub(crate) fn reap_click_watch_for(&mut self, entity: Entity) {
        if let Some(scene) = self.scenes.get_mut(&entity) {
            scene.click_subs.clear();
        }
        if let Some(panel) = self.panels.get_mut(&entity) {
            panel.click_subs.clear();
        }
        self.last_click_target.retain(|(ent, _), _| *ent != entity);
    }

    /// Reap view/panel `entity`'s [`NoesisKeyDownWatch`](crate::events::NoesisKeyDownWatch):
    /// drop its live keydown subscriptions and forget its swallow snapshots.
    pub(crate) fn reap_keydown_watch_for(&mut self, entity: Entity) {
        if let Some(scene) = self.scenes.get_mut(&entity) {
            scene.keydown_subs.clear();
        }
        if let Some(panel) = self.panels.get_mut(&entity) {
            panel.keydown_subs.clear();
        }
        self.last_keydown_swallow
            .retain(|(ent, _), _| *ent != entity);
    }

    /// Reap view `entity`'s [`NoesisEventWatch`](crate::routed_events::NoesisEventWatch):
    /// drop its live routed-event subscriptions and forget its config snapshots.
    pub(crate) fn reap_event_watch_for(&mut self, entity: Entity) {
        if let Some(scene) = self.scenes.get_mut(&entity) {
            scene.event_subs.clear();
        }
        self.last_event_config
            .retain(|(ent, _, _), _| *ent != entity);
    }

    /// Reap view `entity`'s [`NoesisItems`](crate::items::NoesisItems): detach
    /// each bound `ItemsSource` off its live control before dropping the backing
    /// collections. Detaching first releases the control's ref to a collection,
    /// so an object-source binding's rows drop before their `ClassRegistration`
    /// (same use-after-free rule as [`Self::clear_host_data_context`]).
    pub(crate) fn reap_items_for(&mut self, entity: Entity) {
        if let Some(scene) = self.scenes.get(&entity)
            && let Some(content) = scene.view.content()
        {
            let uri = &scene.built_for_uri;
            for ((ent, name), binding) in &self.items_sources {
                if *ent != entity || binding.needs_bind(uri) {
                    continue;
                }
                if let Some(mut element) = resolve_named(&content, name) {
                    element.clear_items_source();
                }
            }
        }
        self.items_sources.retain(|(ent, _), _| *ent != entity);
    }

    /// Reap view `entity`'s [`UiList`](crate::list::UiList) bindings: detach each
    /// list's `ItemsSource` off its cached control (same use-after-free rule as
    /// [`Self::reap_items_for`]), drop the bindings, and drop the per-row click
    /// subscriptions parked in the still-live scene.
    pub(crate) fn reap_list_for(&mut self, entity: Entity) {
        for ((ent, _), binding) in &mut self.lists {
            if *ent == entity {
                binding.detach();
            }
        }
        self.lists.retain(|(ent, _), _| *ent != entity);
        if let Some(scene) = self.scenes.get_mut(&entity) {
            scene.row_click_subs.clear();
        }
    }

    /// Reap view `entity`'s [`NoesisImaging`](crate::imaging::NoesisImaging)
    /// read-back snapshots. The staged registry buffers are reclaimed separately
    /// (they live in the `ImageRegistry` resource, not here); see
    /// `crate::imaging::reap_removed_imaging`.
    pub(crate) fn reap_imaging_snapshots_for(&mut self, entity: Entity) {
        if let Some(scene) = self.scenes.get_mut(&entity) {
            scene.image_snapshots.clear();
        }
    }
}

impl Drop for NoesisRenderState {
    fn drop(&mut self) {
        // Strict teardown order (Noesis demands it):
        //   1. scenes (Renderer::shutdown + View drop) FIRST: a `View` holds
        //      refs to the VM `ClassInstance`s / plain-VM instances it was given
        //      as `DataContext` and to the `ObservableCollection`s set as
        //      `ItemsSource`. Those refs must release before we drop the owners,
        //      or the owner's `ClassRegistration` unregisters the class while a
        //      live (View-held) instance still references it → use-after-free.
        //   2. view-models / items / plain-vms: now the last refs; safe to drop.
        //   3. bake rig, registered device + providers, then global `shutdown()`.
        // This `Drop` owns `shutdown()` (rather than a separate guard) so the
        // ordering is guaranteed: Bevy gives no drop order between two main-world
        // resources, and calling `shutdown()` early deadlocks/crashes.
        self.teardown_all_scenes();
        self.view_models.clear();
        self.items_sources.clear();
        // Lists after the scenes (the View held ItemsSource refs to each
        // collection); each binding drops collection → instances → registration.
        self.lists.clear();
        self.plain_vms.clear();
        // Panels after the scenes: the host views are now torn down, so each host
        // has already released its child ref; clearing just drops our handles in
        // field order (fragment → instance → class).
        self.panels.clear();
        self.command_hosts.clear();
        self.binding_entries.clear();
        self.teardown_bake_rig();
        drop(self.registered_device.take());
        drop(self.registered_provider.take());
        drop(self.registered_fonts.take());
        drop(self.registered_textures.take());
        // Process-global integration callbacks: unregister (their `Drop`) while
        // the engine is still up; after `shutdown()` it would crash.
        self.integration_guards.clear();
        noesis_runtime::shutdown();
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Blit pipeline: stretch the intermediate onto ViewTarget
// ─────────────────────────────────────────────────────────────────────────────

pub(crate) struct BlitPipeline {
    pipeline: wgpu::RenderPipeline,
    bind_group_layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
}

/// Premultiplied-alpha "over": `result = src.rgb + dst.rgb * (1 - src.a)`. Used
/// by *both* compositing nodes to composite the UI directly onto the camera's
/// cleared/finished `ViewTarget` (`LoadOp::Load`): transparent intermediate
/// texels (a == 0) leave the target intact, fully-opaque texels (a == 1)
/// overwrite it, and the fractional-alpha edges Noesis emits with
/// `RenderFlag::Ppaa` enabled blend correctly.
///
/// Noesis writes *premultiplied* alpha into the intermediate (its own
/// `BlendMode::SrcOver` is `One, OneMinusSrcAlpha`). Compositing premultiplied
/// here keeps the `ViewTarget` correct and independent of the clear colour, and
/// stays identical to a 1:1 overwrite whenever the target was cleared
/// transparent (dst == 0 ⇒ result == src). A plain overwrite (`blend = None`)
/// would discard the clear colour and let it bleed through PPAA edges.
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
    /// Build the compositing pipeline for `target_format`. Both render-graph
    /// nodes use the same [`PREMULTIPLIED_OVER`] blend so the UI composites
    /// correctly over whatever the camera left in its `ViewTarget`.
    fn new(device: &wgpu::Device, target_format: wgpu::TextureFormat) -> Self {
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
                    blend: Some(PREMULTIPLIED_OVER),
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

/// Test-only: run the production compositing blit (same [`BlitPipeline`] +
/// [`PREMULTIPLIED_OVER`] blend + `LoadOp::Load` the render-graph nodes use) of
/// `src` onto `target`. Lets integration tests exercise the real premultiplied
/// composite without standing up a render world. Not part of the public API.
#[doc(hidden)]
pub fn blit_composite_for_test(
    device: &wgpu::Device,
    encoder: &mut wgpu::CommandEncoder,
    src: &wgpu::TextureView,
    target: &wgpu::TextureView,
    target_format: wgpu::TextureFormat,
) {
    let blit = BlitPipeline::new(device, target_format);
    let bg = blit.bind_group(device, src);
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("blit_composite_for_test"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: target,
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
}

#[derive(Resource, Default)]
pub(crate) struct BlitPipelineCache {
    /// Premultiplied-alpha "over" pipeline, one per encountered target format.
    /// Both the Core2d and Core3d nodes share it (see [`PREMULTIPLIED_OVER`]).
    over: HashMap<wgpu::TextureFormat, BlitPipeline>,
}

impl BlitPipelineCache {
    fn get(&self, format: wgpu::TextureFormat) -> Option<&BlitPipeline> {
        self.over.get(&format)
    }

    fn ensure(&mut self, device: &wgpu::Device, format: wgpu::TextureFormat) {
        self.over
            .entry(format)
            .or_insert_with(|| BlitPipeline::new(device, format));
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems (render app)
// ─────────────────────────────────────────────────────────────────────────────

/// Copy the [`XamlRegistry`] into the [`SharedXamlMap`] backing
/// [`BevyXamlProvider`]. Runs on the main thread (alongside the rest of the
/// Noesis driving pipeline) directly against the main-world registry.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_xaml_provider_map(
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
/// step: fonts that finish loading after scene build get picked up here
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
    // Reset the per-frame rebuild set before this pass repopulates it; the
    // Apply systems that follow read it within the same frame.
    state.scenes_built_this_frame.clear();
    state.panels_mounted_this_frame.clear();
    for (entity, config) in &views {
        state.ensure_scene(entity, config);
    }
}

/// A bridge component whose per-entity render-side state must be reaped when the
/// component is removed from a live entity — the case entity-despawn teardown
/// ([`NoesisRenderState::teardown_for`]) never sees, because a reconcile system
/// only visits entities that still *have* the component. Implementors name the
/// [`NoesisRenderState`] method that drops exactly what this bridge owns for the
/// entity; that method MUST be idempotent with the terminal teardown (see the
/// per-bridge reaps' shared note). Wire one up with [`add_bridge_reap`].
pub(crate) trait ReapOnRemove: Component {
    /// Drop every per-entity render resource this bridge owns for `entity`.
    fn reap(state: &mut NoesisRenderState, entity: Entity);
}

/// Generic reap system for any [`ReapOnRemove`] bridge `C`: on the main thread
/// (the `NonSendMut` pins it there, where Noesis lives), for each entity whose
/// `C` was removed since last frame. One instance per bridge, scheduled by
/// [`add_bridge_reap`].
#[allow(clippy::needless_pass_by_value)]
fn reap_removed_bridge<C: ReapOnRemove>(
    mut removed: RemovedComponents<C>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for entity in removed.read() {
        C::reap(&mut state, entity);
    }
}

/// Schedule a component-removal reap `system` at the head of
/// [`NoesisSet::Ensure`] — before [`ensure_noesis_scene`] (re)builds the
/// survivors and before the Apply-phase bridges run — so removal teardown is
/// symmetric with the entity-despawn teardown that already runs there
/// ([`teardown_removed_views`] / [`teardown_removed_panels`]). The single home
/// for the reap-ordering contract: [`add_bridge_reap`] routes the trait-driven
/// bridges through it, and the generic plain-VM reap wires through it directly.
pub(crate) fn add_reap_system<M>(
    app: &mut App,
    system: impl IntoScheduleConfigs<ScheduleSystem, M>,
) {
    app.add_systems(
        PostUpdate,
        system.in_set(NoesisSet::Ensure).before(ensure_noesis_scene),
    );
}

/// Wire the [`ReapOnRemove`] reap for bridge component `C`. Mechanical per
/// bridge: implement [`ReapOnRemove`] for `C`, then call this from its plugin.
pub(crate) fn add_bridge_reap<C: ReapOnRemove>(app: &mut App) {
    add_reap_system(app, reap_removed_bridge::<C>);
}

/// Reap the Noesis state of any view whose [`NoesisView`] was removed since last
/// frame, whether the whole entity was despawned or just the component dropped.
/// Without it a despawned view leaks its (`!Send`) scene + side-table entries for
/// the life of the process. Runs on the main thread (the `NonSendMut` forces it
/// there, where Noesis lives) at the head of [`NoesisSet::Ensure`], before
/// [`ensure_noesis_scene`] rebuilds the survivors. Per-bridge component removals
/// are reaped alongside it by the [`ReapOnRemove`] systems.
#[allow(clippy::needless_pass_by_value)]
fn teardown_removed_views(
    mut removed: RemovedComponents<NoesisView>,
    alive: Query<Entity>,
    mut commands: Commands,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for entity in removed.read() {
        state.teardown_for(entity);
        // `RemovedComponents<NoesisView>` fires both when the whole entity is
        // despawned and when only the component is dropped off a surviving
        // entity (a game toggling its UI off while keeping Camera2d/NoesisCamera).
        // A despawn reaps every component (NoesisIntermediate included); a
        // survivor keeps its last-published intermediate, and `teardown_for` has
        // just pruned it out of `publish_intermediates`' sweep — so unless we
        // strip it here the render world blits the last-painted frame forever
        // (the P0.8 ghost). Guard on liveness: a `remove` command on a despawned
        // entity would panic at flush.
        if alive.contains(entity) {
            commands.entity(entity).remove::<NoesisIntermediate>();
        }
    }
}

/// Reap the Noesis state of any panel whose [`UiPanel`](crate::panel::UiPanel)
/// was removed (entity despawned or component dropped). Panels are distinct
/// entities from their host [`NoesisView`], so the view removal hook does not see
/// them; this is their dedicated reap. Runs main-thread (the `NonSendMut` pins it)
/// at the head of [`NoesisSet::Ensure`], before the survivors are reconciled.
#[allow(clippy::needless_pass_by_value)]
fn teardown_removed_panels(
    mut removed: RemovedComponents<crate::panel::UiPanel>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for entity in removed.read() {
        state.teardown_panel_for(entity);
    }
}

/// Stamp the start of the [`NoesisSet::Apply`] phase. Pairs with
/// [`apply_timer_end`]; see [`NoesisApplyTimer`].
fn apply_timer_start(mut timer: ResMut<NoesisApplyTimer>) {
    timer.started = Some(std::time::Instant::now());
}

/// Close the [`NoesisSet::Apply`] timing window opened by [`apply_timer_start`],
/// recording the elapsed wall-time into [`NoesisApplyTimer::last`].
fn apply_timer_end(mut timer: ResMut<NoesisApplyTimer>) {
    if let Some(start) = timer.started.take() {
        timer.last = start.elapsed();
    }
}

/// Re-apply each view's per-frame live settings (PPAA + DPI scale). Cheap:
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
/// [`ensure_noesis_scene`] (so the scene exists) and before
/// [`drive_noesis_frame`] (so `View::Update` picks up the state these
/// events produced: hover highlights, button presses, etc.).
#[allow(clippy::needless_pass_by_value)]
fn apply_noesis_input(
    queue: Option<Res<crate::input::NoesisInputQueue>>,
    state: Option<NonSendMut<NoesisRenderState>>,
    over_ui: Option<ResMut<crate::input::NoesisPointerOverUi>>,
) {
    let (Some(queue), Some(mut state)) = (queue, state) else {
        return;
    };
    if !queue.events.is_empty() {
        state.apply_input(&queue.events);
    } else if state.scenes.is_empty() {
        // No events and no scenes: a view despawned while the pointer was over
        // it would otherwise leave `pointer_over_ui` stuck true forever, wrongly
        // suppressing 3D interaction. `apply_input` clears it when it runs, but
        // teardown frames carry no events, so clear it here too.
        state.pointer_over_ui = false;
    } else {
        return;
    }
    // Publish on change only, so idle pointer frames don't churn change detection.
    if let Some(mut over_ui) = over_ui
        && over_ui.over != state.pointer_over_ui
    {
        over_ui.over = state.pointer_over_ui;
    }
}

/// Drive Noesis for the frame (layout, update render tree, render into each
/// view's intermediate), then publish each intermediate onto its camera entity
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
// NoesisNode: blits the intermediate into ViewTarget
// ─────────────────────────────────────────────────────────────────────────────

/// Render-graph label for the Core2d compositing node ([`NoesisNode`]). Used to
/// position the node between [`Node2d::MainTransparentPass`] and
/// [`Node2d::EndMainPass`] in [`Core2d`].
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct NoesisNodeLabel;

/// Marks the camera Noesis composites its UI onto. Add this to the camera
/// (`Camera2d` or `Camera3d`) whose final image the UI should overlay.
///
/// The blit runs *inside that camera's* render graph (Core2d or Core3d), after
/// its post-processing, so it composes cleanly with whatever the camera does
/// (HDR, image-based lighting, bloom, DOF, …). It does **not** rely on
/// a second window-targeting camera sharing the 3D camera's `ViewTarget`, which
/// breaks the moment the host adds standard 3D features. Tag exactly the
/// camera(s) you want the UI on; untagged cameras (e.g. offscreen effect passes)
/// are skipped.
#[derive(Component, ExtractComponent, Clone, Copy, Default, Debug)]
pub struct NoesisCamera;

/// The painted intermediate for a view, published onto the camera entity by the
/// main-world driving systems and `ExtractComponent`'d to the render world for
/// the blit. This is the **only** Noesis data that crosses to the render world;
/// `View`/`Renderer` stay pinned to the main thread (see `NoesisRenderState`).
/// Both fields are `wgpu::TextureView` (Arc-backed, `Send + Sync`), so the
/// cross-world hand-off is a cheap clone.
#[derive(Component, ExtractComponent, Clone)]
pub struct NoesisIntermediate {
    /// `Rgba8Unorm` raw view, sampled when the target is plain `Rgba8Unorm`.
    view: wgpu::TextureView,
    /// `Rgba8UnormSrgb` alias, sampled when the target is sRGB/HDR so the
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

/// Shared blit body for both nodes. Premultiplied-alpha composites the UI over
/// whatever the camera left in its `ViewTarget` (`LoadOp::Load`): the Core2d
/// node runs on every 2D view, the Core3d node only on views tagged
/// [`NoesisCamera`]. Both use the same [`PREMULTIPLIED_OVER`] blend so PPAA's
/// fractional-alpha edges composite correctly instead of overwriting the clear
/// colour (see [`PREMULTIPLIED_OVER`]).
fn blit_noesis_ui(
    render_context: &mut RenderContext<'_>,
    intermediate: &NoesisIntermediate,
    view_target: &ViewTarget,
    world: &World,
) -> Result<(), NodeRunError> {
    let Some(cache) = world.get_resource::<BlitPipelineCache>() else {
        return Ok(());
    };
    let target_format = view_target.main_texture_format();
    let Some(blit) = cache.get(target_format) else {
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
    //   must be decoded to linear on read; otherwise they're treated as
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

/// Core2d compositing node: runs on **every** Core2d view. Premultiplied-alpha
/// composites the UI over the view's `ViewTarget`; when that target was cleared
/// transparent (a UI camera layered over a lower camera) the result is identical
/// to a 1:1 overwrite, and Bevy's multi-camera step folds it over the
/// lower camera.
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
        blit_noesis_ui(render_context, intermediate, view_target, world)
    }
}

/// Render-graph label for the Core3d overlay node ([`NoesisOverlayNode`]). Used
/// to position the node late in [`Core3d`] so the UI composites over the
/// camera's finished scene.
#[derive(Debug, Hash, PartialEq, Eq, Clone, RenderLabel)]
pub struct NoesisOverlayNodeLabel;

/// Core3d overlay node: runs only on views tagged [`NoesisCamera`], compositing
/// the UI premultiplied-alpha over the camera's finished scene. This is the
/// single-camera path that keeps working when the host adds IBL/bloom/DOF.
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
        blit_noesis_ui(render_context, intermediate, view_target, world)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Sub-plugin that wires Noesis into [`RenderApp`]: it registers the wgpu-backed
/// `RenderDevice`, installs the XAML/font/image providers, runs the main-world
/// driving pipeline ([`NoesisSet`]), and adds the blit nodes to [`Core2d`] and
/// [`Core3d`]. Added for you by the top-level `NoesisPlugin`; you don't add this
/// one directly.
pub struct NoesisRenderPlugin;

impl Plugin for NoesisRenderPlugin {
    fn build(&self, app: &mut App) {
        // The painted intermediate is the only Noesis data the render world sees;
        // it rides each camera entity. (`NoesisView` + the bridges stay
        // main-world; Noesis is thread-affine and lives on the main thread.)
        app.add_plugins((
            ExtractComponentPlugin::<NoesisIntermediate>::default(),
            ExtractComponentPlugin::<NoesisCamera>::default(),
        ));

        // Main-world driving pipeline: all on the main thread (the one thread
        // Bevy pins reliably), satisfying Noesis's thread-affinity contract.
        // Bridge plugins slot their per-view apply systems into `NoesisSet::Apply`.
        app.init_resource::<NoesisApplyTimer>();
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
                // Reap before rebuild; timer_start at the tail of Ensure brackets
                // the whole Apply set (the four phases are `.chain()`-ed above).
                teardown_removed_views
                    .in_set(NoesisSet::Ensure)
                    .before(ensure_noesis_scene),
                teardown_removed_panels
                    .in_set(NoesisSet::Ensure)
                    .before(ensure_noesis_scene),
                ensure_noesis_scene.in_set(NoesisSet::Ensure),
                apply_timer_start
                    .in_set(NoesisSet::Ensure)
                    .after(ensure_noesis_scene),
                (apply_live_scene_flags, apply_noesis_input).in_set(NoesisSet::Apply),
                // Timer end leads Drive, closing the window after every Apply system.
                apply_timer_end
                    .in_set(NoesisSet::Drive)
                    .before(drive_noesis_frame),
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
            // Core2d: premultiplied composite on any 2D view that has published
            // an intermediate (the `ViewQuery` gates on `NoesisIntermediate`).
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
            // passes) and before upscaling, so it survives IBL/bloom/DOF and
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

    /// Create `NoesisRenderState` as a **main-world non-send resource**. Runs
    /// on the main thread (so the resource, and every Noesis handle it owns,
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
    use super::{is_linear_float, resolve_scope_path};
    use wgpu::TextureFormat;

    // A toy namescope tree: each node owns named children, mirroring how a
    // composed control nests a private namescope inside its host. `find` only
    // looks one level down, exactly like Noesis `FindName` against one scope.
    struct Scope {
        name: &'static str,
        children: Vec<Scope>,
    }
    impl Scope {
        fn child(&self, name: &str) -> Option<&Scope> {
            self.children.iter().find(|c| c.name == name)
        }
    }

    fn resolve<'a>(root: &'a Scope, path: &str) -> Option<&'a str> {
        // `find` returns owned references; resolve_scope_path threads them by value.
        resolve_scope_path(&root, path, |node, seg| node.child(seg)).map(|n| n.name)
    }

    #[test]
    fn scope_path_walks_into_nested_namescopes() {
        let tree = Scope {
            name: "Root",
            children: vec![Scope {
                name: "MainMenu",
                children: vec![Scope {
                    name: "Footer",
                    children: vec![Scope {
                        name: "PlayButton",
                        children: vec![],
                    }],
                }],
            }],
        };

        // Plain name: one hop in the root scope (unchanged classic behavior).
        assert_eq!(resolve(&tree, "MainMenu"), Some("MainMenu"));
        // Qualified: descend into the host's private scope to reach its leaf.
        assert_eq!(resolve(&tree, "MainMenu/Footer"), Some("Footer"));
        assert_eq!(
            resolve(&tree, "MainMenu/Footer/PlayButton"),
            Some("PlayButton")
        );

        // A leaf that only exists *inside* the host is unreachable as a plain
        // name from the root — the whole reason qualified names exist.
        assert_eq!(resolve(&tree, "PlayButton"), None);
        // Any unresolved segment fails the whole path.
        assert_eq!(resolve(&tree, "MainMenu/Nope"), None);
        assert_eq!(resolve(&tree, "Nope/Footer"), None);
    }

    #[test]
    fn hdr_float_targets_decode_srgb() {
        // HDR camera ViewTargets store linear values (encoded downstream), so the
        // blit must treat them like sRGB targets and decode on sample.
        assert!(is_linear_float(TextureFormat::Rgba16Float));
        assert!(is_linear_float(TextureFormat::Rgba32Float));
        assert!(is_linear_float(TextureFormat::Rg11b10Ufloat));
    }

    #[test]
    fn noesis_view_requires_the_bridge_components() {
        use bevy::prelude::*;

        // Spawning a bare NoesisView must auto-attach every per-view bridge, so a
        // write set before the scene exists has somewhere to land. No Noesis
        // runtime needed: required components are pure ECS composition.
        let mut world = World::new();
        let view = world
            .spawn(super::NoesisView {
                xaml_uri: "x.xaml".to_string(),
                ..default()
            })
            .id();
        let e = world.entity(view);
        assert!(e.contains::<crate::text::NoesisText>());
        assert!(e.contains::<crate::visibility::NoesisVisibility>());
        assert!(e.contains::<crate::dp::NoesisDp>());
        assert!(e.contains::<crate::items::NoesisItems>());
        assert!(e.contains::<crate::focus::NoesisFocus>());
        assert!(e.contains::<crate::svg::NoesisSvg>());
        assert!(e.contains::<crate::events::NoesisClickWatch>());
        // The explicitly-constructed binding bridges are not auto-attached.
        assert!(!e.contains::<crate::viewmodel::NoesisVm>());
        assert!(!e.contains::<crate::commands::NoesisCommands>());
    }

    #[test]
    fn ldr_unorm_targets_sample_raw() {
        // Plain 8-bit targets are written/displayed raw; no gamma decode. (sRGB
        // targets are handled separately via TextureFormat::is_srgb.)
        assert!(!is_linear_float(TextureFormat::Rgba8Unorm));
        assert!(!is_linear_float(TextureFormat::Bgra8Unorm));
        assert!(!is_linear_float(TextureFormat::Rgba8UnormSrgb));
    }
}

//! Per-view value-converter / multi-binding bridge: install a code-built
//! `{Binding}` — driven by a **Rust** [`ValueConverter`] / [`MultiValueConverter`]
//! — onto a named element's dependency property.
//!
//! Where [`crate::viewmodel`] supplies the *source* data a binding resolves
//! against, this bridge installs the *binding itself*: it wires a target
//! element's DP to a source (its inherited `DataContext`, another element by
//! `x:Name`, or the target itself), running each value through Rust conversion
//! logic on the way. It's the code-built equivalent of authoring
//! `Text="{Binding Path, Converter={StaticResource …}}"` or a `<MultiBinding>`
//! in XAML, but with the converter living in Rust.
//!
//! Add a [`NoesisBinding`] component to the view's camera entity. Each entry
//! names a target `(x:Name, property)`, a [`SourceSpec`] (path + where to read
//! it), and the converter. The bridge builds the Noesis [`Binding`] /
//! [`MultiBinding`] + [`Converter`] / [`MultiConverter`] once (Rust → Noesis),
//! then attaches it to the element when the scene exists — re-attaching after a
//! scene rebuild, exactly like [`crate::items`].
//!
//! ```ignore
//! use noesis_bevy::binding::{NoesisBinding, SourceSpec};
//! use noesis_bevy::binding::{ConvertArg, Converted};
//!
//! commands.entity(view).insert(
//!     NoesisBinding::new()
//!         // Upper.Text <- {Binding Text, ElementName=Source}, uppercased.
//!         .converted("Upper", "Text", SourceSpec::element("Source", "Text"),
//!             |v: &ConvertArg, _p: &ConvertArg| {
//!                 Some(Converted::String(v.as_str()?.to_uppercase()))
//!             })
//!         // Full.Text <- "{First} {Last}" combined from two elements.
//!         .multi("Full", "Text",
//!             [SourceSpec::element("First", "Text"), SourceSpec::element("Last", "Text")],
//!             |vals: &[ConvertArg], _p: &ConvertArg| {
//!                 let a = vals.first().and_then(ConvertArg::as_str)?;
//!                 let b = vals.get(1).and_then(ConvertArg::as_str)?;
//!                 Some(Converted::String(format!("{a} {b}")))
//!             }),
//! );
//! ```
//!
//! # Converter bounds
//!
//! The runtime's [`ValueConverter`] is `Send`-only (it runs on the single
//! view-thread). A Bevy [`Component`] must be `Send + Sync`, so this bridge
//! additionally requires the supplied converter to be `Sync`. Closures that
//! capture only `Sync` data (plain values, `Arc<Atomic…>`, …) satisfy this; it's
//! the common case.
//!
//! # Scope
//!
//! This slice installs **one-way** bindings (source → target) by default — the
//! natural shape for a Rust converter that formats / maps a source value for
//! display. The binding [`mode`](NoesisBinding::mode) is overridable, but
//! `convert_back` for `TwoWay` is reachable only through the [`ValueConverter`]
//! trait's default (it returns `None`); a richer two-way ergonomic is deferred.
//!
//! # Lifetime & threading
//!
//! The built [`Binding`]/[`MultiBinding`] + converter handles are owned per
//! `(view, x:Name, property)` in [`NoesisRenderState`](crate::render) (Noesis
//! objects are thread-affine to the `View`) and released before
//! `noesis_runtime::shutdown`, after the scenes that reference them tear down.

use bevy::prelude::*;
use noesis_runtime::binding::{Binding, set_binding};
use noesis_runtime::converters::Converter;
use noesis_runtime::multi_binding::{MultiBinding, MultiConverter};
use noesis_runtime::view::FrameworkElement;

use crate::render::{NoesisRenderState, NoesisSet};

pub use noesis_runtime::binding::BindingMode;
pub use noesis_runtime::converters::{ConvertArg, Converted, ValueConverter};
pub use noesis_runtime::multi_binding::MultiValueConverter;

// ─────────────────────────────────────────────────────────────────────────────
// Source description
// ─────────────────────────────────────────────────────────────────────────────

/// Where a (child) binding reads its source value from.
#[derive(Clone, Debug)]
enum BindingSource {
    /// The target element's inherited `DataContext` (a plain `{Binding Path}`).
    DataContext,
    /// Another element resolved by its `x:Name` (`{Binding Path,
    /// ElementName=name}`).
    ElementName(String),
    /// The target element itself (`{Binding Path, RelativeSource Self}`).
    Own,
}

/// A binding's source: the property `path` plus where to resolve it
/// ([`BindingSource`]). Build with [`Self::data_context`], [`Self::element`], or
/// [`Self::own`].
#[derive(Clone, Debug)]
pub struct SourceSpec {
    path: String,
    source: BindingSource,
}

impl SourceSpec {
    /// Read `path` off the target's inherited `DataContext` — a plain
    /// `{Binding path}`.
    #[must_use]
    pub fn data_context(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source: BindingSource::DataContext,
        }
    }

    /// Read `path` off the element named `name` — `{Binding path,
    /// ElementName=name}`. Resolves within the loaded scene's namescope.
    #[must_use]
    pub fn element(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source: BindingSource::ElementName(name.into()),
        }
    }

    /// Read `path` off the target element itself — `{Binding path, RelativeSource
    /// Self}`.
    #[must_use]
    pub fn own(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source: BindingSource::Own,
        }
    }

    /// Build the Noesis [`Binding`] this source describes (path + source knob).
    fn build(&self) -> Binding {
        let binding = Binding::new(&self.path);
        match &self.source {
            BindingSource::DataContext => binding,
            BindingSource::ElementName(name) => binding.element_name(name),
            BindingSource::Own => binding.relative_source_self(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Converter newtypes — adapt boxed trait objects to the runtime's by-value API
// ─────────────────────────────────────────────────────────────────────────────

/// A `Sync` boxed [`ValueConverter`] (the component-storable form). `Sync` is
/// the extra bound this bridge layers on top of the runtime's `Send`-only trait
/// (see the module docs).
type BoxedConverter = Box<dyn ValueConverter + Sync>;

/// A `Sync` boxed [`MultiValueConverter`].
type BoxedMultiConverter = Box<dyn MultiValueConverter + Sync>;

/// Adapts a [`BoxedConverter`] back into a by-value [`ValueConverter`] so it can
/// be handed to [`Converter::new`] (which consumes its argument).
struct DynConverter(BoxedConverter);

impl ValueConverter for DynConverter {
    fn convert(&self, value: &ConvertArg, param: &ConvertArg) -> Option<Converted> {
        self.0.convert(value, param)
    }
    fn convert_back(&self, value: &ConvertArg, param: &ConvertArg) -> Option<Converted> {
        self.0.convert_back(value, param)
    }
}

/// Adapts a [`BoxedMultiConverter`] back into a by-value [`MultiValueConverter`].
struct DynMultiConverter(BoxedMultiConverter);

impl MultiValueConverter for DynMultiConverter {
    fn convert(&self, values: &[ConvertArg], param: &ConvertArg) -> Option<Converted> {
        self.0.convert(values, param)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Component spec
// ─────────────────────────────────────────────────────────────────────────────

/// One target's binding recipe. The converter is taken out (`Option::take`) the
/// first time the bridge builds the runtime objects, so a recipe builds exactly
/// once per `(view, element, property)`.
enum BindSpec {
    /// A single converted [`Binding`].
    Converted {
        source: SourceSpec,
        converter: Option<BoxedConverter>,
        mode: BindingMode,
    },
    /// A [`MultiBinding`] combining several child sources.
    Multi {
        sources: Vec<SourceSpec>,
        converter: Option<BoxedMultiConverter>,
        mode: BindingMode,
    },
}

impl BindSpec {
    fn mode_mut(&mut self) -> &mut BindingMode {
        match self {
            BindSpec::Converted { mode, .. } | BindSpec::Multi { mode, .. } => mode,
        }
    }

    /// Consume the converter and build the live Noesis binding + converter. Once
    /// taken, returns `None` (the binding is already built / owned render-side).
    fn take_built(&mut self) -> Option<BuiltBinding> {
        match self {
            BindSpec::Converted {
                source,
                converter,
                mode,
            } => {
                let boxed = converter.take()?;
                let conv = Converter::new(DynConverter(boxed));
                let binding = source.build().mode(*mode).converter(&conv);
                Some(BuiltBinding::Single {
                    binding,
                    _converter: conv,
                })
            }
            BindSpec::Multi {
                sources,
                converter,
                mode,
            } => {
                let boxed = converter.take()?;
                let conv = MultiConverter::new(DynMultiConverter(boxed));
                let mut binding = MultiBinding::new().converter(&conv).mode(*mode);
                for source in sources.iter() {
                    binding = binding.add_binding(source.build());
                }
                Some(BuiltBinding::Multi {
                    binding,
                    _converter: conv,
                })
            }
        }
    }
}

/// One `(x:Name, property)` target and its [`BindSpec`].
struct BindTarget {
    element: String,
    property: String,
    spec: BindSpec,
}

/// Per-view value-converter / multi-binding bridge. Attach to a
/// [`NoesisView`](crate::NoesisView) entity, then add targets with
/// [`converted`](Self::converted) / [`multi`](Self::multi).
#[derive(Component, Default)]
pub struct NoesisBinding {
    targets: Vec<BindTarget>,
}

impl NoesisBinding {
    /// An empty binding set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `element`'s `property` to a single `source` value run through
    /// `converter` (source → target). A bare
    /// `Fn(&ConvertArg, &ConvertArg) -> Option<Converted> + Send + Sync` closure
    /// is a converter. Defaults to [`BindingMode::OneWay`]; override with
    /// [`mode`](Self::mode).
    #[must_use]
    pub fn converted<C: ValueConverter + Sync>(
        mut self,
        element: impl Into<String>,
        property: impl Into<String>,
        source: SourceSpec,
        converter: C,
    ) -> Self {
        self.targets.push(BindTarget {
            element: element.into(),
            property: property.into(),
            spec: BindSpec::Converted {
                source,
                converter: Some(Box::new(converter)),
                mode: BindingMode::OneWay,
            },
        });
        self
    }

    /// Bind `element`'s `property` to several `sources` combined through a
    /// multi-value `converter`. The converter receives one boxed argument per
    /// source, in order. A bare
    /// `Fn(&[ConvertArg], &ConvertArg) -> Option<Converted> + Send + Sync`
    /// closure is a multi-converter.
    #[must_use]
    pub fn multi<C: MultiValueConverter + Sync>(
        mut self,
        element: impl Into<String>,
        property: impl Into<String>,
        sources: impl IntoIterator<Item = SourceSpec>,
        converter: C,
    ) -> Self {
        self.targets.push(BindTarget {
            element: element.into(),
            property: property.into(),
            spec: BindSpec::Multi {
                sources: sources.into_iter().collect(),
                converter: Some(Box::new(converter)),
                mode: BindingMode::OneWay,
            },
        });
        self
    }

    /// Override the [`BindingMode`] of the most recently added target. No-op if
    /// no target has been added yet.
    #[must_use]
    pub fn mode(mut self, mode: BindingMode) -> Self {
        if let Some(target) = self.targets.last_mut() {
            *target.spec.mode_mut() = mode;
        }
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-world entry — BindingEntry
// ─────────────────────────────────────────────────────────────────────────────

/// A built, live binding plus the converter it references — kept alive so the
/// binding keeps working. `_converter` outlives `binding` field-order-wise,
/// mirroring the rule that a converter releases after the binding that uses it.
pub(crate) enum BuiltBinding {
    Single {
        binding: Binding,
        _converter: Converter,
    },
    Multi {
        binding: MultiBinding,
        _converter: MultiConverter,
    },
}

/// One view's binding for a `(x:Name, property)` target: the built handles plus
/// the URI of the scene it's currently attached to. Owned by
/// [`NoesisRenderState`](crate::render), released before runtime shutdown.
///
/// `pub` so headless tests can observe the same build → attach translation the
/// render systems use; apps drive it through [`NoesisBinding`], never directly.
pub struct BindingEntry {
    built: BuiltBinding,
    bound_for_uri: Option<String>,
}

impl BindingEntry {
    pub(crate) fn new(built: BuiltBinding) -> Self {
        Self {
            built,
            bound_for_uri: None,
        }
    }

    pub(crate) fn needs_bind(&self, uri: &str) -> bool {
        self.bound_for_uri.as_deref() != Some(uri)
    }

    pub(crate) fn mark_bound(&mut self, uri: &str) {
        self.bound_for_uri = Some(uri.to_owned());
    }

    /// Detach (logically) so the next bind pass re-attaches against the rebuilt
    /// scene. Called from scene teardown.
    pub(crate) fn reset_bind(&mut self) {
        self.bound_for_uri = None;
    }

    /// Attach the binding onto `element`'s `property`. `false` on an unknown
    /// property / type mismatch (same contract as `set_binding` / `set_on`).
    pub(crate) fn bind_onto(&self, element: &FrameworkElement, property: &str) -> bool {
        match &self.built {
            BuiltBinding::Single { binding, .. } => set_binding(element, property, binding),
            BuiltBinding::Multi { binding, .. } => binding.set_on(element, property),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// System + plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisBinding`]: build any not-yet-built target's
/// runtime binding (taking its converter), then (re-)attach unbound bindings to
/// their elements each frame.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_binding_bridge(
    mut views: Query<(Entity, &mut NoesisBinding)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, mut comp) in &mut views {
        // Building only consumes the converter (Option::take) — it doesn't
        // logically change the component, so don't trip change detection.
        let comp = comp.bypass_change_detection();
        for target in &mut comp.targets {
            if state.has_binding(entity, &target.element, &target.property) {
                continue;
            }
            let Some(built) = target.spec.take_built() else {
                continue;
            };
            state.insert_binding(
                entity,
                target.element.clone(),
                target.property.clone(),
                built,
            );
        }
        state.bind_pending_for(entity);
    }
}

/// Wires the per-view value-converter / multi-binding bridge. Added transitively
/// by [`crate::NoesisPlugin`].
pub struct NoesisBindingPlugin;

impl Plugin for NoesisBindingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_binding_bridge.in_set(NoesisSet::Apply));
    }
}

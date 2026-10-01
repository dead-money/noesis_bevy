//! Code-built bindings with Rust converters: bind a named element's dependency
//! property to a source through a [`ValueConverter`] or [`MultiValueConverter`]
//! written in Rust.
//!
//! [`crate::viewmodel`] supplies the data a binding reads; this bridge installs
//! the binding itself. It is the code equivalent of
//! `Text="{Binding Path, Converter={StaticResource ...}}"` or a `<MultiBinding>`
//! in XAML, with the converter as a Rust closure or type.
//!
//! Add a [`NoesisBinding`] component to the [`NoesisView`](crate::NoesisView)
//! camera entity. Each target names an `(x:Name, property)`, one or more
//! [`SourceSpec`]s (a path plus where to read it), and the converter. The bridge
//! builds each binding once, attaches it when the scene and element exist
//! (retrying each frame and warning while the name is missing), and re-attaches
//! it after a scene rebuild. It acts on view entities only.
//!
//! Builders consume the component, so to change targets, insert a new
//! `NoesisBinding`: targets it lists are rebuilt and targets it drops are
//! unbound. Removing the component unbinds everything.
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
//! The runtime's converter traits only require `Send`, because converters run
//! on the main thread. A Bevy [`Component`] must also be `Sync`, so this bridge
//! requires `Sync` converters too. Closures that capture plain values or
//! `Arc<Atomic...>` qualify.
//!
//! # Scope
//!
//! Bindings default to [`BindingMode::OneWay`] (source to target). Override with
//! [`NoesisBinding::mode`]. A `TwoWay` binding calls
//! [`ValueConverter::convert_back`]; a closure converter uses the trait default,
//! which returns `None` (no write-back), so implement the trait on a type when
//! you need the reverse direction. [`MultiValueConverter`] has no reverse
//! conversion here.

use bevy::prelude::*;
use noesis_runtime::binding::{Binding, set_binding};
use noesis_runtime::converters::Converter;
use noesis_runtime::multi_binding::{MultiBinding, MultiConverter};
use noesis_runtime::view::FrameworkElement;

use crate::render::{NoesisRenderState, NoesisSet, ReapOnRemove, add_bridge_reap};

pub use noesis_runtime::binding::BindingMode;
pub use noesis_runtime::converters::{ConvertArg, Converted, ValueConverter};
pub use noesis_runtime::multi_binding::MultiValueConverter;

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

/// Where a binding reads its value: a property `path` on the target's
/// `DataContext`, on another named element, or on the target itself. Build with
/// [`Self::data_context`], [`Self::element`], or [`Self::own`].
#[derive(Clone, Debug)]
pub struct SourceSpec {
    path: String,
    source: BindingSource,
}

impl SourceSpec {
    /// Read `path` off the target's inherited `DataContext`, a plain
    /// `{Binding path}`.
    #[must_use]
    pub fn data_context(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source: BindingSource::DataContext,
        }
    }

    /// Read `path` off the element named `name`: `{Binding path,
    /// ElementName=name}`. Noesis resolves `name` in the target's namescope.
    #[must_use]
    pub fn element(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            source: BindingSource::ElementName(name.into()),
        }
    }

    /// Read `path` off the target element itself: `{Binding path, RelativeSource
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

/// `Sync` so it can live in a `Component`; the runtime trait is `Send`-only.
type BoxedConverter = Box<dyn ValueConverter + Sync>;

/// A `Sync` boxed [`MultiValueConverter`].
type BoxedMultiConverter = Box<dyn MultiValueConverter + Sync>;

/// Adapts a [`BoxedConverter`] for [`Converter::new`], which takes a sized value.
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

    /// Consume the converter and build the live Noesis binding and converter.
    /// `None` once taken: the built binding is owned by `NoesisRenderState`.
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

/// Per-view set of converted bindings. Build it with
/// [`converted`](Self::converted) and [`multi`](Self::multi), then insert it on a
/// [`NoesisView`](crate::NoesisView) entity. See the [module docs](self).
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

    /// Bind `element`'s `property` to `source`, passing each value through
    /// `converter`. A closure
    /// `Fn(&ConvertArg, &ConvertArg) -> Option<Converted> + Send + Sync` works as
    /// a converter. Its second argument is the converter parameter, which this
    /// bridge never sets. Returning `None` yields `UnsetValue`. Defaults to
    /// [`BindingMode::OneWay`]; change it with [`mode`](Self::mode).
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

    /// Bind `element`'s `property` to several `sources` combined by
    /// `converter`, which receives one value per source in order. A closure
    /// `Fn(&[ConvertArg], &ConvertArg) -> Option<Converted> + Send + Sync` works
    /// as a multi-converter. Defaults to [`BindingMode::OneWay`].
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

/// A built, live binding plus the converter it references, kept alive so the
/// binding keeps working. `_converter` is declared after `binding` so it drops
/// last: a converter must release after the binding that uses it.
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

/// One view's live binding for an `(x:Name, property)` target. Internal to the
/// bridge: apps use [`NoesisBinding`]; it is `pub` only for headless tests.
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

/// Build each view's new [`NoesisBinding`] targets, unbind dropped ones, then
/// attach any binding not yet bound to the current scene.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_binding_bridge(
    mut views: Query<(Entity, &mut NoesisBinding)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, mut comp) in &mut views {
        // Taking the converter isn't a logical change; keep change detection quiet.
        let comp = comp.bypass_change_detection();
        for target in &mut comp.targets {
            // Converter present = first sight or a re-inserted component.
            let Some(built) = target.spec.take_built() else {
                continue;
            };
            if state.has_binding(entity, &target.element, &target.property) {
                state.reap_binding_for(entity, &target.element, &target.property);
            }
            state.insert_binding(
                entity,
                target.element.clone(),
                target.property.clone(),
                built,
            );
        }
        let keep: Vec<(String, String)> = comp
            .targets
            .iter()
            .map(|t| (t.element.clone(), t.property.clone()))
            .collect();
        state.prune_bindings_for(entity, &keep);
        state.bind_pending_for(entity);
    }
}

impl ReapOnRemove for NoesisBinding {
    fn reap(state: &mut NoesisRenderState, entity: Entity) {
        state.reap_bindings_for(entity);
    }
}

/// Registers the [`NoesisBinding`] reconcile system. Added by
/// [`crate::NoesisPlugin`].
pub struct NoesisBindingPlugin;

impl Plugin for NoesisBindingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_binding_bridge.in_set(NoesisSet::Apply));
        add_bridge_reap::<NoesisBinding>(app);
    }
}

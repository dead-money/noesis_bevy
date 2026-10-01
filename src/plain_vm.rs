//! Bind a plain Rust struct, as a Bevy component, to a view's XAML
//! `{Binding field_name}`, two-way.
//!
//! Derive [`NoesisViewModel`] and `Component` on the struct, register it with
//! [`NoesisViewModelAppExt::add_noesis_view_model`], and insert it on a
//! [`NoesisView`](crate::NoesisView) entity. Each field becomes a property of a
//! reflected Noesis type that the view's binding engine resolves by name. For a
//! view model with explicitly declared dependency properties, use
//! [`crate::viewmodel`] instead.
//!
//! ```ignore
//! use bevy::prelude::*;
//! use noesis_bevy::{NoesisViewModel, NoesisViewModelAppExt};
//!
//! #[derive(Component, NoesisViewModel)]
//! struct SettingsVm {
//!     volume: f32,   // <Slider Value="{Binding volume, Mode=TwoWay}"/>
//!     muted: bool,   // <CheckBox IsChecked="{Binding muted}"/>
//!     quality: i32,  // <ComboBox SelectedIndex="{Binding quality, Mode=TwoWay}"/>
//! }
//!
//! fn main() {
//!     App::new()
//!         .add_plugins((DefaultPlugins, noesis_bevy::NoesisPlugin::default()))
//!         .add_noesis_view_model::<SettingsVm>()
//!         .run();
//! }
//!
//! // …then attach the component to a view entity:
//! // commands.entity(view).insert(SettingsVm { volume: 0.8, muted: false, quality: 2 });
//! ```
//!
//! # How the data flows
//!
//! - **Rust to UI.** When the component changes (Bevy change detection), the
//!   sync system in [`NoesisSet::Apply`] pushes every field to the view and
//!   raises property-changed for each, so bound controls update on the view's
//!   next update.
//! - **UI to Rust.** A `TwoWay` edit is queued and applied to the component by
//!   the same system on its next run, through
//!   [`NoesisViewModel::noesis_apply`]. The component is only marked changed
//!   when an edit lands.
//!
//! Each view gets its own instance per type, so several views can carry the
//! same `T`. The instance attaches once the view's scene exists and reattaches
//! after a rebuild. Removing the component detaches it and releases the
//! instance.
//!
//! A view's `DataContext` holds one object per element. If a [`NoesisVm`],
//! a [`NoesisCommands`] or another plain view model attaches to the same
//! element, the last attach wins and a warning is logged; register with
//! [`add_noesis_view_model_at`](NoesisViewModelAppExt::add_noesis_view_model_at)
//! to target a different element.
//!
//! [`NoesisVm`]: crate::NoesisVm
//! [`NoesisCommands`]: crate::NoesisCommands

use std::sync::{Arc, Mutex};

use bevy::ecs::component::Mutable;
use bevy::prelude::*;

pub use noesis_runtime::plain_vm::{
    PlainInstance, PlainSetHandler, PlainType, PlainValue, PlainValueRef, PlainVmBuilder,
    PlainVmClass,
};

use crate::render::{NoesisRenderState, NoesisSet, add_reap_system};
use crate::viewmodel::AttachTarget;

/// `(prop_index, value)` UI edits queued by the `on_set` hook and drained by
/// the sync system. One per instance.
pub(crate) type SetSink = Arc<Mutex<Vec<(u32, PlainValue)>>>;

/// Decodes a `TwoWay` edit as the property's declared [`PlainType`];
/// [`PlainValue::Null`] when it is null or doesn't decode.
pub(crate) fn unbox(kind: PlainType, value: &PlainValueRef) -> PlainValue {
    if value.is_none() {
        return PlainValue::Null;
    }
    let decoded = match kind {
        PlainType::Int32 => value.as_i32().map(PlainValue::Int32),
        PlainType::Double => value.as_f64().map(PlainValue::Double),
        PlainType::Bool => value.as_bool().map(PlainValue::Bool),
        PlainType::String => value.as_str().map(|s| PlainValue::String(s.to_owned())),
        PlainType::U64 => value.as_u64().map(PlainValue::U64),
        PlainType::BaseComponent => None,
    };
    decoded.unwrap_or(PlainValue::Null)
}

/// A struct whose fields bind to XAML as reflected Noesis properties. Used by
/// the plain view model bridge ([`NoesisViewModelAppExt`]), panels
/// ([`crate::panel`]) and lists ([`crate::list`]).
///
/// Derive it with `#[derive(NoesisViewModel)]`, re-exported from the crate
/// root; the `noesis_bevy_derive` crate docs list the supported field types
/// and attributes. Implement it by hand only for control the derive doesn't
/// offer. The methods work on owned [`PlainValue`]s and never touch Noesis.
pub trait NoesisViewModel: Send + Sync + 'static {
    /// Name for the reflected Noesis type, shown in diagnostics. The derive
    /// defaults it to the struct identifier. It need not be unique: the plain
    /// view model bridge registers a per-view name derived from it.
    fn noesis_type_name() -> &'static str
    where
        Self: Sized;

    /// `(property_name, type)` for each bound property. The index into this
    /// slice is the `prop_index` passed to [`Self::noesis_apply`].
    fn noesis_properties() -> &'static [(&'static str, PlainType)]
    where
        Self: Sized;

    /// Current values, one per [`Self::noesis_properties`] entry, in order.
    fn noesis_snapshot(&self) -> Vec<PlainValue>;

    /// Writes a UI edit into the property at `prop_index`. Ignore an
    /// out-of-range index or a value whose variant doesn't match the property.
    fn noesis_apply(&mut self, prop_index: u32, value: &PlainValue);
}

/// Field order matters: `instance` drops before `_class`.
pub(crate) struct PlainVmEntry {
    instance: PlainInstance,
    _class: PlainVmClass,
    /// Unmangled name, for diagnostics; the registered name is per-entity (see
    /// [`Self::build`]).
    type_name: String,
    prop_names: Vec<String>,
    target: AttachTarget,
    attached_for_uri: Option<String>,
    set_sink: SetSink,
}

impl PlainVmEntry {
    /// Registers the reflected type, wires `on_set` to this entry's sink, and
    /// instantiates. Main thread only. `None` if registration or instantiation
    /// fails.
    ///
    /// Registers as `"{type_name}#{entity bits}"`: Noesis's type registry is
    /// process-global, so a second view with the same `T` would otherwise fail
    /// to register. Bindings resolve properties on the instance, so XAML never
    /// sees the mangled name.
    pub(crate) fn build(
        type_name: &str,
        entity: Entity,
        props: &[(&'static str, PlainType)],
        target: AttachTarget,
    ) -> Option<Self> {
        let prop_names: Vec<String> = props.iter().map(|(n, _)| (*n).to_owned()).collect();
        let kinds: Vec<PlainType> = props.iter().map(|(_, k)| *k).collect();
        let set_sink: SetSink = Arc::new(Mutex::new(Vec::new()));

        let registered_name = format!("{type_name}#{}", entity.to_bits());
        let mut builder = PlainVmBuilder::new(&registered_name);
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
        Some(Self {
            instance,
            _class: class,
            type_name: type_name.to_owned(),
            prop_names,
            target,
            attached_for_uri: None,
            set_sink,
        })
    }

    /// Sets every property in `snapshot` and raises property-changed for each.
    pub(crate) fn apply_snapshot(&self, snapshot: &[PlainValue]) {
        for (idx, value) in snapshot.iter().enumerate() {
            if let Some(name) = self.prop_names.get(idx) {
                let _ = self
                    .instance
                    .set_and_notify(idx as u32, name, value.clone());
            }
        }
    }

    /// Takes the queued UI edits.
    pub(crate) fn drain_writebacks(&self) -> Vec<(u32, PlainValue)> {
        let mut guard = self.set_sink.lock().expect("plain VM set sink poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }

    pub(crate) fn reset_attach(&mut self) {
        self.attached_for_uri = None;
    }

    pub(crate) fn target(&self) -> &AttachTarget {
        &self.target
    }

    /// The unmangled type name, for diagnostics.
    pub(crate) fn type_name(&self) -> &str {
        &self.type_name
    }

    pub(crate) fn needs_attach(&self, uri: &str) -> bool {
        self.attached_for_uri.as_deref() != Some(uri)
    }

    /// Sets the instance as `element`'s `DataContext` and records `uri` on
    /// success.
    pub(crate) fn attach_to(
        &mut self,
        element: &mut noesis_runtime::view::FrameworkElement,
        uri: &str,
    ) -> bool {
        if self.instance.set_data_context(element) {
            self.attached_for_uri = Some(uri.to_owned());
            true
        } else {
            false
        }
    }
}

/// Where view model type `T` attaches as `DataContext` in every view that
/// carries it. Inserted by [`NoesisViewModelAppExt`].
#[derive(Resource)]
pub struct PlainVmConfig<T: Send + Sync + 'static> {
    target: AttachTarget,
    _marker: std::marker::PhantomData<fn() -> T>,
}

/// No-op until [`NoesisRenderState`] exists.
#[allow(clippy::needless_pass_by_value)]
fn sync_plain_vm_system<T: NoesisViewModel + Component<Mutability = Mutable>>(
    mut views: Query<(Entity, &mut T)>,
    config: Res<PlainVmConfig<T>>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, mut vm) in &mut views {
        // `is_changed` covers the initial insert.
        let snapshot = vm.is_changed().then(|| vm.noesis_snapshot());
        let writebacks = state.sync_plain_vm(
            entity,
            std::any::TypeId::of::<T>(),
            T::noesis_type_name(),
            T::noesis_properties(),
            &config.target,
            snapshot,
        );
        // Deref-mut only on a real edit; an idle frame must not mark it changed.
        for (index, value) in writebacks {
            vm.noesis_apply(index, &value);
        }
    }
}

/// Without this reap, a removed `T` leaves its instance attached and its sink
/// filling with UI edits nobody drains. Wired directly through
/// [`add_reap_system`] because `ReapOnRemove` is one impl per concrete
/// component, and here the component is the generic `T`.
#[allow(clippy::needless_pass_by_value)]
fn reap_plain_vm_system<T: NoesisViewModel + Component<Mutability = Mutable>>(
    mut removed: RemovedComponents<T>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for entity in removed.read() {
        state.reap_plain_vm_for(entity, std::any::TypeId::of::<T>());
    }
}

/// Registers plain view model types. Add [`crate::NoesisPlugin`] first, then
/// insert the `T` component on a [`NoesisView`](crate::NoesisView) entity to
/// bind it. Register each type once.
pub trait NoesisViewModelAppExt {
    /// Binds `T` as the root element's `DataContext` in every view that
    /// carries it.
    fn add_noesis_view_model<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
    ) -> &mut Self;

    /// Binds `T` as the `DataContext` of the element named `x_name` (may be
    /// scope-qualified, `"Host/Leaf"`) in every view that carries it. If the
    /// element isn't found, a warning is logged each frame.
    fn add_noesis_view_model_at<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
        x_name: impl Into<String>,
    ) -> &mut Self;
}

impl NoesisViewModelAppExt for App {
    fn add_noesis_view_model<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
    ) -> &mut Self {
        register_plain_vm::<T>(self, AttachTarget::Root)
    }

    fn add_noesis_view_model_at<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
        x_name: impl Into<String>,
    ) -> &mut Self {
        register_plain_vm::<T>(self, AttachTarget::Named(x_name.into()))
    }
}

fn register_plain_vm<T: NoesisViewModel + Component<Mutability = Mutable>>(
    app: &mut App,
    target: AttachTarget,
) -> &mut App {
    app.insert_resource(PlainVmConfig::<T> {
        target,
        _marker: std::marker::PhantomData,
    })
    .add_systems(
        PostUpdate,
        sync_plain_vm_system::<T>.in_set(NoesisSet::Apply),
    );
    add_reap_system(app, reap_plain_vm_system::<T>);
    app
}

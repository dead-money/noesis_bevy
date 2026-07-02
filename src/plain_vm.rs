//! Per-view plain-struct `ViewModel` bridge: bind a plain Bevy `Component` to a
//! view's XAML `{Binding field_name}` by field name, two-way.
//!
//! Where [`crate::viewmodel`] binds a `DependencyObject`-backed view model with
//! explicitly-declared dependency properties, this binds an *ordinary* Rust
//! struct: derive [`NoesisViewModel`] on it, make it a `Component`, register the
//! type, and add the component to a [`NoesisView`](crate::NoesisView) entity. The
//! runtime's reflected plain-VM (`noesis_runtime::plain_vm`) exposes each field
//! to that view's binding engine by name, including `TwoWay` writeback and
//! `INotifyPropertyChanged`.
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
//!         .add_noesis_view_model::<SettingsVm>() // register the type once
//!         .run();
//! }
//!
//! // …then attach the component to a view entity:
//! // commands.entity(view).insert(SettingsVm { volume: 0.8, muted: false, quality: 2 });
//! ```
//!
//! # How the data flows
//!
//! - **Rust → UI.** When the `Component` is mutated (Bevy change detection), the
//!   reconcile system snapshots its fields and pushes them into that view's
//!   plain-VM instance with `set_and_notify`; the bound controls update on the
//!   next `View::update`.
//! - **UI → Rust.** A `TwoWay` edit fires the runtime's `on_set` hook (on the
//!   main thread, where the `View` lives); the bridge converts the boxed value to
//!   an owned [`PlainValue`] and pushes it onto the entry's per-view writeback
//!   sink. The same reconcile system drains the sink and applies each edit back
//!   into the `Component` via [`NoesisViewModel::noesis_apply`].
//!
//! # Threading & lifetime
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there).
//! The plain-VM instance is created and owned per `(view entity, type)` in
//! [`NoesisRenderState`](crate::render), released before
//! `noesis_runtime::shutdown`. The `Component` stays in the ECS; only owned
//! [`PlainValue`]s cross into Noesis, so no Noesis handle is ever touched off the
//! main thread.

use std::sync::{Arc, Mutex};

use bevy::ecs::component::Mutable;
use bevy::prelude::*;

pub use noesis_runtime::plain_vm::{
    PlainInstance, PlainSetHandler, PlainType, PlainValue, PlainValueRef, PlainVmBuilder,
    PlainVmClass,
};

use crate::render::{NoesisRenderState, NoesisSet, add_reap_system};
use crate::viewmodel::AttachTarget;

/// The UI→Rust writeback sink shared between the (main-thread) `on_set` hook and
/// the reconcile drain. `(prop_index, value)` pairs. Owned per-entry so each
/// view's writebacks stay isolated.
pub(crate) type SetSink = Arc<Mutex<Vec<(u32, PlainValue)>>>;

/// Convert the boxed `TwoWay` writeback value to an owned [`PlainValue`],
/// decoding it as the property's declared [`PlainType`].
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

/// Implemented by `#[derive(NoesisViewModel)]` (re-exported from the crate
/// root). The derive maps each struct field to a reflected Noesis property and
/// generates the snapshot (Rust→UI) and writeback (UI→Rust) glue. Hand-impl
/// only if you need control the derive doesn't offer.
///
/// `noesis_snapshot` / `noesis_apply` work in owned [`PlainValue`]s and never
/// touch a Noesis handle, so they run main-world; the bridge does the
/// `set_and_notify` / `on_set` plumbing.
pub trait NoesisViewModel: Send + Sync + 'static {
    /// Unique Noesis type name for the reflected plain-VM (defaults to the
    /// struct identifier).
    fn noesis_type_name() -> &'static str
    where
        Self: Sized;

    /// Ordered `(field_name, type)` metadata. The index into this slice is the
    /// `prop_index` used by [`Self::noesis_apply`].
    fn noesis_properties() -> &'static [(&'static str, PlainType)]
    where
        Self: Sized;

    /// Current field values, one per [`Self::noesis_properties`] entry, in
    /// order. Pushed into the bound controls (Rust→UI).
    fn noesis_snapshot(&self) -> Vec<PlainValue>;

    /// Write a UI edit back into the field at `prop_index` (UI→Rust). A value
    /// whose variant doesn't match the field is ignored.
    fn noesis_apply(&mut self, prop_index: u32, value: &PlainValue);
}

/// One live plain view model owned per `(view entity, type)` by
/// [`NoesisRenderState`]. Field order matters: `instance` drops before `class`.
pub(crate) struct PlainVmEntry {
    instance: PlainInstance,
    _class: PlainVmClass,
    /// Registered Noesis type name, kept for `DataContext`-collision diagnostics.
    type_name: String,
    /// Property names in index order, for `set_and_notify`.
    prop_names: Vec<String>,
    target: AttachTarget,
    attached_for_uri: Option<String>,
    /// UI→Rust writeback sink: the `on_set` hook pushes `(prop_index, value)`
    /// here; [`Self::drain_writebacks`] empties it into the owning component.
    /// Owned by the entry so each view's writebacks are isolated.
    set_sink: SetSink,
}

impl PlainVmEntry {
    /// Register the reflected type, wire the `on_set` writeback to this entry's
    /// own sink, and instantiate. Main-thread only. `None` if registration is
    /// rejected (e.g. a duplicate type name).
    pub(crate) fn build(
        type_name: &str,
        props: &[(&'static str, PlainType)],
        target: AttachTarget,
    ) -> Option<Self> {
        let prop_names: Vec<String> = props.iter().map(|(n, _)| (*n).to_owned()).collect();
        let kinds: Vec<PlainType> = props.iter().map(|(_, k)| *k).collect();
        let set_sink: SetSink = Arc::new(Mutex::new(Vec::new()));

        let mut builder = PlainVmBuilder::new(type_name);
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

    /// Push a full field snapshot into the instance (Rust→UI).
    pub(crate) fn apply_snapshot(&self, snapshot: &[PlainValue]) {
        for (idx, value) in snapshot.iter().enumerate() {
            if let Some(name) = self.prop_names.get(idx) {
                let _ = self
                    .instance
                    .set_and_notify(idx as u32, name, value.clone());
            }
        }
    }

    /// Take the pending UI→Rust writebacks (drained each frame by the reconcile
    /// system into the owning component).
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

    /// Borrow the attach target for the render-side bind pass.
    pub(crate) fn target(&self) -> &AttachTarget {
        &self.target
    }

    /// The registered Noesis type name, for `DataContext`-collision diagnostics.
    pub(crate) fn type_name(&self) -> &str {
        &self.type_name
    }

    pub(crate) fn needs_attach(&self, uri: &str) -> bool {
        self.attached_for_uri.as_deref() != Some(uri)
    }

    /// Attach the instance to `element` as its `DataContext`; records the URI on
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

/// Per-type config: where to attach the VM as `DataContext`. Set once at
/// registration; applies to every view entity carrying a `T` component. Plain
/// main-world resource (no extraction; the whole bridge runs main-world).
#[derive(Resource)]
pub struct PlainVmConfig<T: Send + Sync + 'static> {
    target: AttachTarget,
    _marker: std::marker::PhantomData<fn() -> T>,
}

/// Reconcile every view's `T` plain view model: build/attach its render-side
/// entry, snapshot the component into the instance when it changed (Rust→UI),
/// and apply any queued two-way edits back into the component (UI→Rust). No-op
/// (state retained) until [`NoesisRenderState`] exists.
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
        // Snapshot only on a real change (covers the initial insert via
        // `is_added`); reading `&self` never trips change detection.
        let snapshot = vm.is_changed().then(|| vm.noesis_snapshot());
        let writebacks = state.sync_plain_vm(
            entity,
            std::any::TypeId::of::<T>(),
            T::noesis_type_name(),
            T::noesis_properties(),
            &config.target,
            snapshot,
        );
        // Only touch the component mutably when there's an actual edit, so an
        // idle frame doesn't falsely mark it changed (which would re-snapshot).
        for (index, value) in writebacks {
            vm.noesis_apply(index, &value);
        }
    }
}

/// Reap view `entity`'s `T` plain view model when the `T` component is removed
/// while its view stays live. Without it the entry's `set_sink` keeps
/// accumulating UI writebacks that nobody drains (unbounded growth as the user
/// interacts) and the instance stays attached as `DataContext`. Plain VMs are
/// keyed `(entity, TypeId)`, so this cannot ride the [`crate::render::ReapOnRemove`]
/// trait (one impl per component, but here the component *is* the generic `T`);
/// it wires through [`add_reap_system`] like the trait-driven bridges do.
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

/// `App` methods to register a plain-struct view model type. Add
/// [`crate::NoesisPlugin`] first, then register the type; attach the `T`
/// component to a [`NoesisView`](crate::NoesisView) entity to bind it.
pub trait NoesisViewModelAppExt {
    /// Bind `T` as each carrying view's **root** `DataContext`.
    fn add_noesis_view_model<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
    ) -> &mut Self;

    /// Bind `T` as the `DataContext` of the element named `x_name` within each
    /// carrying view.
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

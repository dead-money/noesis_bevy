//! Plain-struct `ViewModel` bridge (TODO §3 / §9, Phase C) — bind a plain Bevy
//! `Resource` to XAML `{Binding field_name}` by field name, two-way.
//!
//! Where [`crate::viewmodel`] binds a `DependencyObject`-backed view model with
//! explicitly-declared dependency properties, this binds an *ordinary* Rust
//! struct: derive [`NoesisViewModel`] on it, register it, and the runtime's
//! reflected plain-VM (`dm_noesis_runtime::plain_vm`) exposes each field to the
//! binding engine by name — including `TwoWay` writeback and
//! `INotifyPropertyChanged`.
//!
//! ```ignore
//! use bevy::prelude::*;
//! use dm_noesis_bevy::{NoesisViewModel, NoesisViewModelAppExt};
//!
//! #[derive(Resource, NoesisViewModel)]
//! struct SettingsVm {
//!     volume: f32,   // <Slider Value="{Binding volume, Mode=TwoWay}"/>
//!     muted: bool,   // <CheckBox IsChecked="{Binding muted}"/>
//!     quality: i32,  // <ComboBox SelectedIndex="{Binding quality, Mode=TwoWay}"/>
//! }
//!
//! fn main() {
//!     App::new()
//!         .add_plugins((DefaultPlugins, dm_noesis_bevy::NoesisPlugin::default()))
//!         .insert_resource(SettingsVm { volume: 0.8, muted: false, quality: 2 })
//!         .add_noesis_view_model::<SettingsVm>() // attach as the view-root DataContext
//!         .run();
//! }
//! ```
//!
//! # How the data flows
//!
//! - **Rust → UI.** When the `Resource` is mutated (Bevy change detection), a
//!   `PostUpdate` system snapshots its fields and the render world pushes them
//!   into the plain-VM instance with `set_and_notify` — the bound controls
//!   update on the next `View::update`.
//! - **UI → Rust.** A `TwoWay` edit fires the runtime's `on_set` hook on the
//!   render thread; the bridge converts the boxed value to an owned
//!   [`PlainValue`], queues it to the main thread, and applies it back into the
//!   `Resource` via [`NoesisViewModel::noesis_apply`].
//!
//! # Threading & lifetime
//!
//! The plain-VM instance is created render-side (Noesis objects are
//! thread-affine to the `View`) and owned in
//! [`NoesisRenderState`](crate::render), released before
//! `dm_noesis_runtime::shutdown`. The `Resource` stays main-world; only owned
//! [`PlainValue`]s cross the boundary, so no Noesis handle is ever touched off
//! the render thread.

use std::any::TypeId;
use std::marker::PhantomData;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};

pub use dm_noesis_runtime::plain_vm::{
    PlainInstance, PlainSetHandler, PlainType, PlainValue, PlainValueRef, PlainVmBuilder,
    PlainVmClass,
};

use crate::render::NoesisRenderState;
use crate::viewmodel::AttachTarget;

/// The UI→Rust writeback sink shared between the render-thread `on_set` hook and
/// the main-world drain. `(prop_index, value)` pairs.
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
        PlainType::BaseComponent => None,
    };
    decoded.unwrap_or(PlainValue::Null)
}

// ─────────────────────────────────────────────────────────────────────────────
// The derive target
// ─────────────────────────────────────────────────────────────────────────────

/// Implemented by `#[derive(NoesisViewModel)]` (re-exported from the crate
/// root). The derive maps each struct field to a reflected Noesis property and
/// generates the snapshot (Rust→UI) and writeback (UI→Rust) glue. Hand-impl
/// only if you need control the derive doesn't offer.
///
/// `noesis_snapshot` / `noesis_apply` work in owned [`PlainValue`]s and never
/// touch a Noesis handle, so they run main-world; the bridge does the
/// render-side `set_and_notify` / `on_set` plumbing.
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

// ─────────────────────────────────────────────────────────────────────────────
// Render-world entry
// ─────────────────────────────────────────────────────────────────────────────

/// One live plain view model owned by [`NoesisRenderState`]. Field order
/// matters: `instance` drops before `class`.
pub(crate) struct PlainVmEntry {
    instance: PlainInstance,
    _class: PlainVmClass,
    /// Property names in index order, for `set_and_notify`.
    prop_names: Vec<String>,
    target: AttachTarget,
    attached_for_uri: Option<String>,
}

impl PlainVmEntry {
    /// Register the reflected type, wire the `on_set` writeback to `set_sink`,
    /// and instantiate. Render-thread only. `None` if registration is rejected
    /// (e.g. a duplicate type name).
    pub(crate) fn build(
        type_name: &str,
        props: &[(&'static str, PlainType)],
        target: AttachTarget,
        set_sink: SetSink,
    ) -> Option<Self> {
        let prop_names: Vec<String> = props.iter().map(|(n, _)| (*n).to_owned()).collect();
        let kinds: Vec<PlainType> = props.iter().map(|(_, k)| *k).collect();

        let mut builder = PlainVmBuilder::new(type_name);
        for (name, kind) in props {
            builder.add_property(name, *kind);
        }
        let class = builder
            .on_set(move |idx: u32, value: &PlainValueRef| {
                let kind = kinds
                    .get(idx as usize)
                    .copied()
                    .unwrap_or(PlainType::BaseComponent);
                let owned = unbox(kind, value);
                if let Ok(mut queue) = set_sink.lock() {
                    queue.push((idx, owned));
                }
            })
            .register()?;
        let instance = class.create_instance()?;
        Some(Self {
            instance,
            _class: class,
            prop_names,
            target,
            attached_for_uri: None,
        })
    }

    /// Push a full field snapshot into the instance (Rust→UI).
    pub(crate) fn apply_snapshot(&self, snapshot: &[PlainValue]) {
        for (idx, value) in snapshot.iter().enumerate() {
            if let Some(name) = self.prop_names.get(idx) {
                self.instance
                    .set_and_notify(idx as u32, name, value.clone());
            }
        }
    }

    pub(crate) fn reset_attach(&mut self) {
        self.attached_for_uri = None;
    }

    /// Borrow the attach target for the render-side bind pass.
    pub(crate) fn target(&self) -> &AttachTarget {
        &self.target
    }

    pub(crate) fn needs_attach(&self, uri: &str) -> bool {
        self.attached_for_uri.as_deref() != Some(uri)
    }

    /// Attach the instance to `element` as its `DataContext`; records the URI on
    /// success.
    pub(crate) fn attach_to(
        &mut self,
        element: &mut dm_noesis_runtime::view::FrameworkElement,
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

// ─────────────────────────────────────────────────────────────────────────────
// Per-type main↔render channels (generic resources)
// ─────────────────────────────────────────────────────────────────────────────

/// Rust→UI: the latest field snapshot to push (latest-wins). `Arc`-aliased
/// across the world boundary by [`ExtractResource`].
#[derive(Resource)]
pub struct PlainVmInbox<T: Send + Sync + 'static> {
    inner: Arc<Mutex<Option<Vec<PlainValue>>>>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Send + Sync + 'static> Clone for PlainVmInbox<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            _marker: PhantomData,
        }
    }
}

impl<T: Send + Sync + 'static> Default for PlainVmInbox<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(None)),
            _marker: PhantomData,
        }
    }
}

impl<T: Send + Sync + 'static> ExtractResource for PlainVmInbox<T> {
    type Source = PlainVmInbox<T>;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

impl<T: Send + Sync + 'static> PlainVmInbox<T> {
    fn set(&self, snapshot: Vec<PlainValue>) {
        *self.inner.lock().expect("PlainVmInbox poisoned") = Some(snapshot);
    }

    fn take(&self) -> Option<Vec<PlainValue>> {
        self.inner.lock().expect("PlainVmInbox poisoned").take()
    }
}

/// UI→Rust: pending writebacks pushed by the render-thread `on_set` hook.
#[derive(Resource)]
pub struct PlainVmSetQueue<T: Send + Sync + 'static> {
    inner: SetSink,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Send + Sync + 'static> Clone for PlainVmSetQueue<T> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            _marker: PhantomData,
        }
    }
}

impl<T: Send + Sync + 'static> Default for PlainVmSetQueue<T> {
    fn default() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
            _marker: PhantomData,
        }
    }
}

impl<T: Send + Sync + 'static> ExtractResource for PlainVmSetQueue<T> {
    type Source = PlainVmSetQueue<T>;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

impl<T: Send + Sync + 'static> PlainVmSetQueue<T> {
    fn sink(&self) -> SetSink {
        Arc::clone(&self.inner)
    }

    fn drain(&self) -> Vec<(u32, PlainValue)> {
        let mut guard = self.inner.lock().expect("PlainVmSetQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Per-type config: where to attach the VM. Set once at registration.
#[derive(Resource)]
pub struct PlainVmConfig<T: Send + Sync + 'static> {
    target: AttachTarget,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Send + Sync + 'static> Clone for PlainVmConfig<T> {
    fn clone(&self) -> Self {
        Self {
            target: self.target.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T: Send + Sync + 'static> ExtractResource for PlainVmConfig<T> {
    type Source = PlainVmConfig<T>;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Main-app: snapshot the `Resource` into the inbox whenever it changes
/// (covers the initial insert via `is_added`).
fn push_plain_vm_snapshot<T: NoesisViewModel + Resource>(
    vm: Option<Res<T>>,
    inbox: Res<PlainVmInbox<T>>,
) {
    let Some(vm) = vm else {
        return;
    };
    if vm.is_changed() {
        inbox.set(vm.noesis_snapshot());
    }
}

/// Main-app: apply queued UI writebacks back into the `Resource`.
fn drain_plain_vm_set<T: NoesisViewModel + Resource>(
    queue: Res<PlainVmSetQueue<T>>,
    vm: Option<ResMut<T>>,
) {
    let pending = queue.drain();
    if pending.is_empty() {
        return;
    }
    let Some(mut vm) = vm else {
        return;
    };
    for (index, value) in pending {
        vm.noesis_apply(index, &value);
    }
}

/// Render-app: register / instantiate / attach the VM and apply the latest
/// snapshot. No-op (queues retained) until [`NoesisRenderState`] and the target
/// element exist.
fn sync_plain_vm_system<T: NoesisViewModel + Resource>(
    inbox: Option<Res<PlainVmInbox<T>>>,
    queue: Option<Res<PlainVmSetQueue<T>>>,
    config: Option<Res<PlainVmConfig<T>>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(inbox), Some(queue), Some(config), Some(mut state)) = (inbox, queue, config, state)
    else {
        return;
    };
    state.sync_plain_vm(
        TypeId::of::<T>(),
        T::noesis_type_name(),
        T::noesis_properties(),
        &config.target,
        &queue.sink(),
        inbox.take(),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// App extension
// ─────────────────────────────────────────────────────────────────────────────

/// `App` methods to register a plain-struct view model. Add [`crate::NoesisPlugin`]
/// first, insert the VM `Resource`, then call one of these.
pub trait NoesisViewModelAppExt {
    /// Bind `T` as the **view root**'s `DataContext`.
    fn add_noesis_view_model<T: NoesisViewModel + Resource>(&mut self) -> &mut Self;

    /// Bind `T` as the `DataContext` of the element named `x_name`.
    fn add_noesis_view_model_at<T: NoesisViewModel + Resource>(
        &mut self,
        x_name: impl Into<String>,
    ) -> &mut Self;
}

impl NoesisViewModelAppExt for App {
    fn add_noesis_view_model<T: NoesisViewModel + Resource>(&mut self) -> &mut Self {
        register_plain_vm::<T>(self, AttachTarget::Root)
    }

    fn add_noesis_view_model_at<T: NoesisViewModel + Resource>(
        &mut self,
        x_name: impl Into<String>,
    ) -> &mut Self {
        register_plain_vm::<T>(self, AttachTarget::Named(x_name.into()))
    }
}

fn register_plain_vm<T: NoesisViewModel + Resource>(
    app: &mut App,
    target: AttachTarget,
) -> &mut App {
    app.init_resource::<PlainVmInbox<T>>()
        .init_resource::<PlainVmSetQueue<T>>()
        .insert_resource(PlainVmConfig::<T> {
            target,
            _marker: PhantomData,
        })
        .add_plugins((
            ExtractResourcePlugin::<PlainVmInbox<T>>::default(),
            ExtractResourcePlugin::<PlainVmSetQueue<T>>::default(),
            ExtractResourcePlugin::<PlainVmConfig<T>>::default(),
        ))
        .add_systems(PostUpdate, push_plain_vm_snapshot::<T>)
        .add_systems(PreUpdate, drain_plain_vm_set::<T>);

    if let Some(render_app) = app.get_sub_app_mut(RenderApp) {
        render_app.add_systems(
            Render,
            sync_plain_vm_system::<T>.in_set(RenderSystems::Prepare),
        );
    }
    app
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy;

    #[test]
    fn inbox_latest_wins_and_takes_once() {
        let inbox = PlainVmInbox::<Dummy>::default();
        assert!(inbox.take().is_none());
        inbox.set(vec![PlainValue::Int32(1)]);
        inbox.set(vec![PlainValue::Int32(2)]);
        let taken = inbox.take().expect("snapshot present");
        assert!(matches!(taken.as_slice(), [PlainValue::Int32(2)]));
        assert!(inbox.take().is_none(), "snapshot consumed");
    }

    #[test]
    fn set_queue_drains_in_push_order() {
        let queue = PlainVmSetQueue::<Dummy>::default();
        let sink = queue.sink();
        sink.lock().unwrap().push((0, PlainValue::Bool(true)));
        sink.lock().unwrap().push((1, PlainValue::Double(0.5)));
        let drained = queue.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].0, 0);
        assert!(matches!(drained[0].1, PlainValue::Bool(true)));
        assert_eq!(drained[1].0, 1);
        assert!(queue.drain().is_empty());
    }
}

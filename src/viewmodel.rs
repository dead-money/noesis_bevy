//! Per-view Rust-owned `ViewModel` / `DataContext` bridge (TODO §3).
//!
//! Drive a XAML scene's `{Binding ...}` controls from a Rust-owned view model
//! without touching Noesis pointers. Add a [`NoesisVm`] component to the
//! view's camera entity: it declares the bindable dependency properties (a
//! [`ViewModelDef`]), the bridge registers the Noesis class + instance and
//! attaches it as the view's (or a named element's) `DataContext`. Writes are
//! queued by mutating the component; two-way edits flow back as
//! [`NoesisViewModelChanged`] messages carrying the originating `view` entity.
//!
//! ```ignore
//! use dm_noesis_bevy::viewmodel::{NoesisVm, ViewModelDef};
//! use dm_noesis_bevy::classes::PropType;
//!
//! commands.entity(view).insert(NoesisVm::new(
//!     ViewModelDef::new("Settings.ViewModel")
//!         .property("MasterVolume", PropType::Double)
//!         .property("Muted", PropType::Bool),
//! ));
//!
//! // write Rust -> UI:
//! fn set_volume(mut q: Query<&mut NoesisVm>) {
//!     q.single_mut().set_f64("MasterVolume", 0.8);
//! }
//! // observe UI -> Rust:
//! fn on_change(mut changed: MessageReader<NoesisViewModelChanged>) {
//!     for ev in changed.read() { /* ev.view, ev.prop, ev.value */ }
//! }
//! ```
//!
//! # Threading & lifetime
//!
//! The [`ClassInstance`] is created on the main thread (Noesis is thread-affine
//! to the `View`) and owned per-view in [`NoesisRenderState`](crate::render),
//! released before `noesis_runtime::shutdown`. `on_changed` also fires on the
//! main thread; the forwarder pushes onto a queue drained into messages.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_runtime::classes::{
    ClassBuilder, ClassInstance, ClassRegistration, Instance, PropertyChangeHandler, PropertyValue,
};
use noesis_runtime::ffi::{ClassBase, PropType};

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Public value type
// ─────────────────────────────────────────────────────────────────────────────

/// An owned dependency-property value crossing the bridge in either direction.
///
/// Covers the value types a settings UI needs: `Slider.Value` (`Double`),
/// `CheckBox.IsChecked` (`Bool`), `ComboBox.SelectedIndex` (`Int32`), text
/// (`Str`). `Float` properties arrive widened to [`VmValue::Double`].
#[derive(Debug, Clone, PartialEq)]
pub enum VmValue {
    Double(f64),
    Bool(bool),
    Int32(i32),
    Str(String),
}

impl VmValue {
    /// Decode the value handed to a [`PropertyChangeHandler`]. Returns `None`
    /// for property kinds this bridge doesn't surface.
    fn from_property(value: &PropertyValue<'_>) -> Option<Self> {
        match *value {
            PropertyValue::Double(d) => Some(Self::Double(d)),
            PropertyValue::Float(f) => Some(Self::Double(f64::from(f))),
            PropertyValue::Bool(b) => Some(Self::Bool(b)),
            PropertyValue::Int32(i) => Some(Self::Int32(i)),
            PropertyValue::String(s) => Some(Self::Str(s.unwrap_or_default().to_owned())),
            _ => None,
        }
    }

    /// Write this value into `instance`'s dependency property at `index`.
    fn apply_to(&self, instance: Instance, index: u32) {
        match self {
            Self::Double(v) => instance.set_double(index, *v),
            Self::Bool(v) => instance.set_bool(index, *v),
            Self::Int32(v) => instance.set_int32(index, *v),
            Self::Str(v) => instance.set_string(index, v),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// ViewModelDef — declarative recipe
// ─────────────────────────────────────────────────────────────────────────────

/// Where the bridge attaches a view model's instance as `DataContext`.
#[derive(Debug, Clone)]
pub(crate) enum AttachTarget {
    /// The view's root element (`View::content`).
    Root,
    /// The element resolved by `x:Name` via `FrameworkElement::find_name`.
    Named(String),
}

/// A declarative recipe for a view model: a Noesis class name, the ordered set
/// of bindable dependency properties, and where to attach the instance.
///
/// Build with the chained setters, then hand to [`NoesisVm::new`]. Each
/// property name must be unique within the def and match the `{Binding <name>}`
/// paths authored in the XAML.
#[derive(Debug, Clone)]
pub struct ViewModelDef {
    class_name: String,
    props: Vec<(String, PropType)>,
    target: AttachTarget,
}

impl ViewModelDef {
    /// Begin a def for the Noesis class `class_name`. Defaults to attaching at
    /// the view root — override with [`Self::attach_to`].
    #[must_use]
    pub fn new(class_name: impl Into<String>) -> Self {
        Self {
            class_name: class_name.into(),
            props: Vec::new(),
            target: AttachTarget::Root,
        }
    }

    /// Declare a bindable dependency property. `name` is the `{Binding name}`
    /// path; `kind` is its [`PropType`].
    #[must_use]
    pub fn property(mut self, name: impl Into<String>, kind: PropType) -> Self {
        self.props.push((name.into(), kind));
        self
    }

    /// Attach the instance as the view root's `DataContext` (the default).
    #[must_use]
    pub fn attach_to_root(mut self) -> Self {
        self.target = AttachTarget::Root;
        self
    }

    /// Attach the instance as the `DataContext` of the element named `x_name`.
    #[must_use]
    pub fn attach_to(mut self, x_name: impl Into<String>) -> Self {
        self.target = AttachTarget::Named(x_name.into());
        self
    }

    pub(crate) fn class_name(&self) -> &str {
        &self.class_name
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Per-view component
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view binding component. Attach to a [`NoesisView`](crate::NoesisView)
/// entity. Holds the [`ViewModelDef`] and a queue of pending Rust→UI writes;
/// mutate it (`set_f64`, …) to push values, which apply on the next frame.
#[derive(Component)]
pub struct NoesisVm {
    def: ViewModelDef,
    pending: Vec<(String, VmValue)>,
}

impl NoesisVm {
    /// Build a view model from its [`ViewModelDef`]. The class registration,
    /// instantiation, and `DataContext` attach happen on a later frame
    /// (retained until the view exists), so this is safe from `Startup`.
    #[must_use]
    pub fn new(def: ViewModelDef) -> Self {
        Self {
            def,
            pending: Vec::new(),
        }
    }

    /// Queue a `Double` write (e.g. `Slider.Value`).
    pub fn set_f64(&mut self, prop: impl Into<String>, value: f64) {
        self.pending.push((prop.into(), VmValue::Double(value)));
    }

    /// Queue a `Bool` write (e.g. `CheckBox.IsChecked`).
    pub fn set_bool(&mut self, prop: impl Into<String>, value: bool) {
        self.pending.push((prop.into(), VmValue::Bool(value)));
    }

    /// Queue an `Int32` write (e.g. `ComboBox.SelectedIndex`).
    pub fn set_i32(&mut self, prop: impl Into<String>, value: i32) {
        self.pending.push((prop.into(), VmValue::Int32(value)));
    }

    /// Queue a `String` write.
    pub fn set_string(&mut self, prop: impl Into<String>, value: impl Into<String>) {
        self.pending
            .push((prop.into(), VmValue::Str(value.into())));
    }

    pub(crate) fn def(&self) -> &ViewModelDef {
        &self.def
    }

    /// Take the queued writes (called by the reconcile system).
    pub(crate) fn take_pending(&mut self) -> Vec<(String, VmValue)> {
        std::mem::take(&mut self.pending)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Change side — shared queue + message + forwarding handler
// ─────────────────────────────────────────────────────────────────────────────

/// Queue between the (main-thread) [`ViewModelChangeForwarder`] callbacks and
/// the drain system. Entries carry the originating view entity.
#[derive(Resource, Clone, Default)]
pub struct SharedVmChangedQueue(Arc<Mutex<Vec<(Entity, String, VmValue)>>>);

impl SharedVmChangedQueue {
    /// Push a change from a forwarder.
    pub(crate) fn push(&self, view: Entity, prop: String, value: VmValue) {
        self.0
            .lock()
            .expect("SharedVmChangedQueue poisoned")
            .push((view, prop, value));
    }

    /// Take the pending changes. Drained into [`NoesisViewModelChanged`]; also
    /// exposed so headless tests can read the queue directly.
    #[must_use]
    pub fn drain(&self) -> Vec<(Entity, String, VmValue)> {
        let mut guard = self.0.lock().expect("SharedVmChangedQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Emitted when a view model's dependency property changes — from a two-way
/// bound control (a slider drag) or a Rust write that altered the value.
#[derive(Message, Debug, Clone)]
pub struct NoesisViewModelChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose view model changed.
    pub view: Entity,
    /// The bindable property's name, as declared in [`ViewModelDef::property`].
    pub prop: String,
    /// The new value.
    pub value: VmValue,
}

/// Main-thread [`PropertyChangeHandler`] that forwards a view model's
/// dependency-property changes onto a [`SharedVmChangedQueue`], tagged with the
/// owning view entity. `pub` so headless tests can wire the same forwarding.
pub struct ViewModelChangeForwarder {
    view: Entity,
    /// Property index → name (DP addition order). Shared (not cloned per call)
    /// because the callback fires on the hot path.
    prop_names: Arc<Vec<String>>,
    queue: SharedVmChangedQueue,
}

impl ViewModelChangeForwarder {
    /// Build a forwarder for the view model owned by `view`. `prop_names` must
    /// be indexed the same way the class's DPs were registered.
    #[must_use]
    pub fn new(view: Entity, prop_names: Arc<Vec<String>>, queue: SharedVmChangedQueue) -> Self {
        Self {
            view,
            prop_names,
            queue,
        }
    }
}

impl PropertyChangeHandler for ViewModelChangeForwarder {
    fn on_changed(&self, _instance: Instance, prop_index: u32, value: PropertyValue<'_>) {
        let Some(name) = self.prop_names.get(prop_index as usize) else {
            return;
        };
        let Some(value) = VmValue::from_property(&value) else {
            return;
        };
        self.queue.push(self.view, name.clone(), value);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-world entry — VmEntry
// ─────────────────────────────────────────────────────────────────────────────

/// One live view model, owned per-view by [`NoesisRenderState`]. Field order
/// matters: `instance` drops before `registration`, mirroring the C++ refcount
/// rule that a class's instances release before the class unregisters.
pub(crate) struct VmEntry {
    instance: ClassInstance,
    _registration: ClassRegistration,
    /// Property index → name (addition order), for name→index write lookups.
    prop_names: Vec<String>,
    target: AttachTarget,
    /// URI of the scene this VM is currently attached to, or `None` when not
    /// yet attached / detached by a scene rebuild.
    attached_for_uri: Option<String>,
}

impl VmEntry {
    /// Register the Noesis class, instantiate it, and wire its change forwarder
    /// (tagged with `view`) to `changed`. `None` if registration / instantiation
    /// is rejected (e.g. a duplicate class name). Main-thread only.
    pub(crate) fn build(
        view: Entity,
        def: &ViewModelDef,
        changed: &SharedVmChangedQueue,
    ) -> Option<Self> {
        let prop_names: Vec<String> = def.props.iter().map(|(n, _)| n.clone()).collect();
        let forwarder =
            ViewModelChangeForwarder::new(view, Arc::new(prop_names.clone()), changed.clone());
        let mut builder = ClassBuilder::new(&def.class_name, ClassBase::ContentControl, forwarder);
        for (name, kind) in &def.props {
            builder.add_property(name, *kind);
        }
        let registration = builder.register()?;
        let instance = registration.create_instance()?;
        Some(Self {
            instance,
            _registration: registration,
            prop_names,
            target: def.target.clone(),
            attached_for_uri: None,
        })
    }

    pub(crate) fn target(&self) -> &AttachTarget {
        &self.target
    }

    /// Borrow the instance for `set_data_context`. Lives as long as the entry.
    pub(crate) fn instance(&self) -> &ClassInstance {
        &self.instance
    }

    /// Apply a write by property name. `false` when the VM has no such property.
    pub(crate) fn write(&self, prop: &str, value: &VmValue) -> bool {
        let Some(index) = self.prop_names.iter().position(|n| n == prop) else {
            return false;
        };
        value.apply_to(self.instance.handle(), index as u32);
        true
    }

    pub(crate) fn needs_attach(&self, uri: &str) -> bool {
        self.attached_for_uri.as_deref() != Some(uri)
    }

    pub(crate) fn mark_attached(&mut self, uri: &str) {
        self.attached_for_uri = Some(uri.to_owned());
    }

    /// Detach (logically) so the next attach pass re-binds against the rebuilt
    /// scene. Called from scene teardown.
    pub(crate) fn reset_attach(&mut self) {
        self.attached_for_uri = None;
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems + plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisVm`]: build its render-side entry on
/// first sight, apply queued writes, then (re-)attach it as its target's
/// `DataContext`.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_view_models(
    mut views: Query<(Entity, &mut NoesisVm)>,
    changed: Res<SharedVmChangedQueue>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, mut vm) in &mut views {
        state.ensure_view_model(entity, vm.def(), &changed);
        let writes = vm.take_pending();
        if !writes.is_empty() {
            state.apply_view_model_writes_for(entity, &writes);
        }
    }
    state.attach_view_models();
}

/// Drain the shared change queue into [`NoesisViewModelChanged`] messages.
#[allow(clippy::needless_pass_by_value)]
pub fn drain_vm_changed_queue(
    queue: Res<SharedVmChangedQueue>,
    mut messages: MessageWriter<NoesisViewModelChanged>,
) {
    for (view, prop, value) in queue.drain() {
        messages.write(NoesisViewModelChanged { view, prop, value });
    }
}

/// Wires the per-view `ViewModel` / `DataContext` bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisViewModelPlugin;

impl Plugin for NoesisViewModelPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SharedVmChangedQueue::default())
            .add_message::<NoesisViewModelChanged>()
            .add_systems(PreUpdate, drain_vm_changed_queue)
            .add_systems(PostUpdate, sync_view_models.in_set(NoesisSet::Apply));
    }
}

//! Rust-owned view model bound as a view's `DataContext`.
//!
//! Drives a XAML scene's `{Binding ...}` controls from Rust without touching
//! Noesis pointers. Add a [`NoesisVm`] to the [`NoesisView`](crate::NoesisView)
//! camera entity. Its [`ViewModelDef`] names a Noesis class and its bindable
//! dependency properties; the bridge registers that class, creates one instance,
//! and sets it as the `DataContext` of the view root or a named element.
//!
//! Write to the UI with the `set_*` methods on [`NoesisVm`]. Changes from the UI
//! (a two-way bound slider, a checkbox) arrive as [`NoesisViewModelChanged`]
//! messages tagged with the view entity.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use noesis_bevy::classes::PropType;
//! use noesis_bevy::viewmodel::{NoesisViewModelChanged, NoesisVm, ViewModelDef};
//!
//! fn add_settings_vm(commands: &mut Commands, view: Entity) {
//!     commands.entity(view).insert(NoesisVm::new(
//!         ViewModelDef::new("Settings.ViewModel")
//!             .property("MasterVolume", PropType::Double)
//!             .property("Muted", PropType::Bool),
//!     ));
//! }
//!
//! fn set_volume(mut vms: Query<&mut NoesisVm>) {
//!     for mut vm in &mut vms {
//!         vm.set_f64("MasterVolume", 0.8);
//!     }
//! }
//!
//! fn on_change(mut changed: MessageReader<NoesisViewModelChanged>) {
//!     for ev in changed.read() {
//!         info!("{:?}: {} = {:?}", ev.view, ev.prop, ev.value);
//!     }
//! }
//! ```
//!
//! # Timing and lifetime
//!
//! Queued writes, class registration and the `DataContext` attach run in
//! [`NoesisSet::Apply`] on the main thread, where Noesis lives. Change callbacks
//! are queued and drained into [`NoesisViewModelChanged`] in the next
//! `PreUpdate`, so a change is visible one frame after it happens. A Rust write
//! that changes a value also comes back as a [`NoesisViewModelChanged`].
//!
//! The instance is reattached after a scene rebuild (hot reload). Re-inserting a
//! [`NoesisVm`] with a different def rebuilds the class and instance; removing
//! the component or despawning the view detaches and releases them.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_runtime::classes::{
    ClassBuilder, ClassInstance, ClassRegistration, Instance, PropertyChangeHandler, PropertyValue,
};
use noesis_runtime::ffi::{ClassBase, PropType};

use crate::render::{NoesisRenderState, NoesisSet, ReapOnRemove, add_bridge_reap};

/// An owned dependency-property value crossing the bridge in either direction.
///
/// Covers the value types a settings UI needs: `Slider.Value` (`Double`),
/// `CheckBox.IsChecked` (`Bool`), `ComboBox.SelectedIndex` (`Int32`), text
/// (`Str`). `Float` properties arrive widened to [`VmValue::Double`].
#[derive(Debug, Clone, PartialEq)]
pub enum VmValue {
    /// A `Double` property, such as a `Slider.Value`.
    Double(f64),
    /// A `Bool` property, such as a `CheckBox.IsChecked`.
    Bool(bool),
    /// An `Int32` property, such as a `ComboBox.SelectedIndex`.
    Int32(i32),
    /// A `String` property, such as bound text.
    Str(String),
}

impl VmValue {
    /// `None` for property kinds this bridge doesn't surface.
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

    fn apply_to(&self, instance: Instance, index: u32) {
        match self {
            Self::Double(v) => instance.set_double(index, *v),
            Self::Bool(v) => instance.set_bool(index, *v),
            Self::Int32(v) => instance.set_int32(index, *v),
            Self::Str(v) => instance.set_string(index, v),
        }
    }
}

/// Where the bridge attaches a view model's instance as `DataContext`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum AttachTarget {
    /// The view's root element (`View::content`).
    Root,
    /// The element resolved by `x:Name` via `FrameworkElement::find_name`.
    Named(String),
}

impl AttachTarget {
    pub(crate) fn describe(&self) -> String {
        match self {
            Self::Root => "root".to_owned(),
            Self::Named(name) => format!("x:Name {name:?}"),
        }
    }
}

/// Recipe for a view model: a Noesis class name, its bindable dependency
/// properties in order, and where to attach the instance.
///
/// Build with the chained setters, then pass to [`NoesisVm::new`]. Property
/// names must be unique within the def and match the `{Binding <name>}` paths in
/// the XAML. The class name is registered process-wide, so two live defs with
/// the same class name collide and the second fails to build (with a warning).
#[derive(Debug, Clone, PartialEq)]
pub struct ViewModelDef {
    class_name: String,
    props: Vec<(String, PropType)>,
    target: AttachTarget,
}

impl ViewModelDef {
    /// Starts a def for the Noesis class `class_name`, attached at the view root
    /// unless you call [`Self::attach_to`].
    #[must_use]
    pub fn new(class_name: impl Into<String>) -> Self {
        Self {
            class_name: class_name.into(),
            props: Vec::new(),
            target: AttachTarget::Root,
        }
    }

    /// Declares a bindable dependency property. `name` is the `{Binding name}`
    /// path. Writes and change messages support `Double`, `Float` (reported as
    /// [`VmValue::Double`]), `Bool`, `Int32` and `String`; other kinds bind but
    /// don't surface changes.
    #[must_use]
    pub fn property(mut self, name: impl Into<String>, kind: PropType) -> Self {
        self.props.push((name.into(), kind));
        self
    }

    /// Attaches the instance as the view root's `DataContext` (the default).
    #[must_use]
    pub fn attach_to_root(mut self) -> Self {
        self.target = AttachTarget::Root;
        self
    }

    /// Attaches the instance as the `DataContext` of the element named `x_name`.
    /// The name may be scope-qualified (`"Host/Leaf"`). Until the element
    /// exists, each frame logs a warning and retries.
    #[must_use]
    pub fn attach_to(mut self, x_name: impl Into<String>) -> Self {
        self.target = AttachTarget::Named(x_name.into());
        self
    }

    pub(crate) fn class_name(&self) -> &str {
        &self.class_name
    }
}

/// Per-view view model. Add it to a [`NoesisView`](crate::NoesisView) camera
/// entity. Holds the [`ViewModelDef`] and a queue of writes to the UI; the
/// `set_*` methods queue a write, applied in the next [`NoesisSet::Apply`].
/// A write to a property the def doesn't declare logs a warning and is dropped.
#[derive(Component)]
pub struct NoesisVm {
    def: ViewModelDef,
    pending: Vec<(String, VmValue)>,
}

impl NoesisVm {
    /// Creates the component from its def. Registration and the `DataContext`
    /// attach wait until the view's scene exists, so this is safe to insert from
    /// `Startup`.
    #[must_use]
    pub fn new(def: ViewModelDef) -> Self {
        Self {
            def,
            pending: Vec::new(),
        }
    }

    /// Queues a `Double` write (e.g. `Slider.Value`).
    pub fn set_f64(&mut self, prop: impl Into<String>, value: f64) {
        self.pending.push((prop.into(), VmValue::Double(value)));
    }

    /// Queues a `Bool` write (e.g. `CheckBox.IsChecked`).
    pub fn set_bool(&mut self, prop: impl Into<String>, value: bool) {
        self.pending.push((prop.into(), VmValue::Bool(value)));
    }

    /// Queues an `Int32` write (e.g. `ComboBox.SelectedIndex`).
    pub fn set_i32(&mut self, prop: impl Into<String>, value: i32) {
        self.pending.push((prop.into(), VmValue::Int32(value)));
    }

    /// Queues a `String` write.
    pub fn set_string(&mut self, prop: impl Into<String>, value: impl Into<String>) {
        self.pending.push((prop.into(), VmValue::Str(value.into())));
    }

    pub(crate) fn def(&self) -> &ViewModelDef {
        &self.def
    }

    /// `&self` so the reconcile system can check without tripping change detection.
    pub(crate) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(crate) fn take_pending(&mut self) -> Vec<(String, VmValue)> {
        std::mem::take(&mut self.pending)
    }
}

/// Changes queued by [`ViewModelChangeForwarder`] callbacks as
/// `(view, property, value)`, drained into [`NoesisViewModelChanged`] each
/// `PreUpdate`. Inserted by [`NoesisViewModelPlugin`].
#[derive(Resource, Clone, Default)]
pub struct SharedVmChangedQueue(Arc<Mutex<Vec<(Entity, String, VmValue)>>>);

impl SharedVmChangedQueue {
    pub(crate) fn push(&self, view: Entity, prop: String, value: VmValue) {
        self.0
            .lock()
            .expect("SharedVmChangedQueue poisoned")
            .push((view, prop, value));
    }

    /// Takes the queued changes. The plugin's drain system normally does this;
    /// it is public so headless tests can read the queue directly.
    ///
    /// # Panics
    ///
    /// Panics if the mutex is poisoned.
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

/// A view model's dependency property changed, from a two-way bound control (a
/// slider drag) or a Rust write that changed the value. Rust writes echo back,
/// so guard against feedback loops if you write in response.
#[derive(Message, Debug, Clone)]
pub struct NoesisViewModelChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose view model changed.
    pub view: Entity,
    /// The bindable property's name, as declared in [`ViewModelDef::property`].
    pub prop: String,
    /// The new value.
    pub value: VmValue,
}

/// [`PropertyChangeHandler`] that pushes a view model's property changes onto a
/// [`SharedVmChangedQueue`], tagged with the owning view entity. Public so
/// headless tests can wire the same forwarding; changes of unsupported kinds and
/// out-of-range indices are dropped.
pub struct ViewModelChangeForwarder {
    view: Entity,
    /// Index → name, in DP registration order.
    prop_names: Arc<Vec<String>>,
    queue: SharedVmChangedQueue,
}

impl ViewModelChangeForwarder {
    /// Creates a forwarder for the view model on `view`. `prop_names` must be in
    /// the order the class's properties were registered.
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

/// One live view model, owned per view by [`NoesisRenderState`]. Field order is
/// drop order: the instance must release before its class unregisters.
pub(crate) struct VmEntry {
    instance: ClassInstance,
    _registration: ClassRegistration,
    /// Rebuild fingerprint (see [`Self::matches`]) and name → index map for writes.
    def: ViewModelDef,
    /// Scene URI this is attached to; `None` until attached or after a rebuild.
    attached_for_uri: Option<String>,
}

impl VmEntry {
    /// `None` if registration or instantiation is rejected (e.g. a duplicate
    /// class name).
    pub(crate) fn build(
        view: Entity,
        def: &ViewModelDef,
        changed: &SharedVmChangedQueue,
    ) -> Option<Self> {
        let prop_names: Vec<String> = def.props.iter().map(|(n, _)| n.clone()).collect();
        let forwarder = ViewModelChangeForwarder::new(view, Arc::new(prop_names), changed.clone());
        let mut builder = ClassBuilder::new(&def.class_name, ClassBase::ContentControl, forwarder);
        for (name, kind) in &def.props {
            builder.add_property(name, *kind);
        }
        let registration = builder.register()?;
        let instance = registration.create_instance()?;
        Some(Self {
            instance,
            _registration: registration,
            def: def.clone(),
            attached_for_uri: None,
        })
    }

    pub(crate) fn matches(&self, def: &ViewModelDef) -> bool {
        &self.def == def
    }

    pub(crate) fn target(&self) -> &AttachTarget {
        &self.def.target
    }

    pub(crate) fn instance(&self) -> &ClassInstance {
        &self.instance
    }

    /// `false` when the def has no such property.
    pub(crate) fn write(&self, prop: &str, value: &VmValue) -> bool {
        let Some(index) = self.def.props.iter().position(|(n, _)| n == prop) else {
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

    /// Marks the entry unattached so the next attach pass rebinds to the rebuilt
    /// scene.
    pub(crate) fn reset_attach(&mut self) {
        self.attached_for_uri = None;
    }
}

/// Builds each view's [`VmEntry`] on first sight (or on a changed def), applies
/// queued writes, then attaches pending entries as `DataContext`.
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
        // Deref-mut only with queued writes, so idle frames don't mark `NoesisVm` changed.
        if vm.has_pending() {
            let writes = vm.take_pending();
            state.apply_view_model_writes_for(entity, &writes);
        }
    }
    state.attach_view_models();
}

/// Drains [`SharedVmChangedQueue`] into [`NoesisViewModelChanged`] messages.
/// Runs in `PreUpdate`.
#[allow(clippy::needless_pass_by_value)]
pub fn drain_vm_changed_queue(
    queue: Res<SharedVmChangedQueue>,
    mut messages: MessageWriter<NoesisViewModelChanged>,
) {
    for (view, prop, value) in queue.drain() {
        messages.write(NoesisViewModelChanged { view, prop, value });
    }
}

impl ReapOnRemove for NoesisVm {
    fn reap(state: &mut NoesisRenderState, entity: Entity) {
        state.reap_view_model_for(entity);
    }
}

/// Wires the [`NoesisVm`] bridge. Added by [`crate::NoesisPlugin`].
pub struct NoesisViewModelPlugin;

impl Plugin for NoesisViewModelPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SharedVmChangedQueue::default())
            .add_message::<NoesisViewModelChanged>()
            .add_systems(PreUpdate, drain_vm_changed_queue)
            .add_systems(PostUpdate, sync_view_models.in_set(NoesisSet::Apply));
        add_bridge_reap::<NoesisVm>(app);
    }
}

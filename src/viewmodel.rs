//! Rust-owned `ViewModel` / `DataContext` bridge (TODO §3 binding bridge).
//!
//! Lets a Bevy app drive a XAML scene's `{Binding ...}` controls from a
//! Rust-owned view model without ever touching Noesis pointers or the FFI.
//! The motivating case is a settings menu whose `Slider` / `CheckBox` /
//! `ComboBox` are two-way bound to game config: the app declares the bindable
//! properties, writes them from gameplay code, and receives control edits back
//! as ordinary Bevy [`Message`]s.
//!
//! # The three halves
//!
//! 1. **Declare + attach.** [`NoesisViewModels::register`] takes a
//!    [`ViewModelDef`] (a class name plus an ordered list of bindable
//!    dependency properties) and returns a stable [`ViewModelId`]. The bridge
//!    registers the Noesis class, instantiates it, and attaches the instance
//!    as the `DataContext` of the view root (or a named element) — all
//!    **render-side**, once the live `View` exists.
//!
//! 2. **Write (Rust → UI).** [`NoesisViewModels::set_f64`] / `set_bool` /
//!    `set_i32` / `set_string` queue a write keyed by `(id, property-name)`.
//!    The render world drains them onto the instance's dependency properties,
//!    so every bound control updates on the next `View::update`.
//!
//! 3. **Observe (UI → Rust).** Two-way edits (a slider drag, a checkbox click)
//!    flow back through Noesis's binding engine onto the VM's properties, fire
//!    the bridge's [`ViewModelChangeForwarder`], and surface as a main-world
//!    [`NoesisViewModelChanged`] message carrying the property name and new
//!    value.
//!
//! # Threading & lifetime
//!
//! Noesis objects are thread-affine to the thread that drives the `View` — in
//! a Bevy app the **render thread**. So the bridge creates the
//! [`ClassInstance`] render-side (never main-side) and owns it in
//! [`NoesisRenderState`](crate::render), whose `Drop` releases it before
//! `dm_noesis_runtime::shutdown`. The [`ClassRegistration`] is owned alongside
//! and outlives the instance.
//!
//! `PropertyChangeHandler::on_changed` also fires on the render thread, so the
//! forwarder does the minimum — map the property index to its name and push
//! onto a shared queue. The hop into ECS happens on the main thread via the
//! [`NoesisViewModelChanged`] message, exactly like
//! [`NoesisClicked`](crate::events::NoesisClicked).
//!
//! One [`ViewModelDef`] maps to one Noesis class and one instance, so the
//! forwarder can carry the [`ViewModelId`] it was built for. Register a fresh
//! def (with a distinct class name) per view model you need.
//!
//! # Usage
//!
//! ```ignore
//! use bevy::prelude::*;
//! use dm_noesis_bevy::viewmodel::{NoesisViewModels, NoesisViewModelChanged, ViewModelDef, VmValue};
//! use dm_noesis_bevy::classes::PropType;
//!
//! #[derive(Resource)]
//! struct SettingsVm(dm_noesis_bevy::viewmodel::ViewModelId);
//!
//! fn setup(mut commands: Commands, vms: Res<NoesisViewModels>) {
//!     let id = vms.register(
//!         ViewModelDef::new("Settings.ViewModel")
//!             .property("MasterVolume", PropType::Double)
//!             .property("Muted", PropType::Bool)
//!             .property("Quality", PropType::Int32)
//!             .attach_to_root(),
//!     );
//!     vms.set_f64(id, "MasterVolume", 0.8);
//!     commands.insert_resource(SettingsVm(id));
//! }
//!
//! fn on_change(mut changed: MessageReader<NoesisViewModelChanged>) {
//!     for ev in changed.read() {
//!         if let ("MasterVolume", VmValue::Double(v)) = (ev.prop.as_str(), &ev.value) {
//!             // commit *v to the audio config / live-preview
//!         }
//!     }
//! }
//! ```

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU64, Ordering},
};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};
use dm_noesis_runtime::classes::{
    ClassBuilder, ClassInstance, ClassRegistration, Instance, PropertyChangeHandler, PropertyValue,
};
use dm_noesis_runtime::ffi::{ClassBase, PropType};

use crate::render::NoesisRenderState;

// ─────────────────────────────────────────────────────────────────────────────
// Public value + id types
// ─────────────────────────────────────────────────────────────────────────────

/// Stable handle to a registered view model. Returned by
/// [`NoesisViewModels::register`]; pass it to the `set_*` writers and match it
/// against [`NoesisViewModelChanged::id`] to route changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewModelId(pub u64);

/// An owned dependency-property value crossing the bridge in either direction.
///
/// Covers the value types a settings UI needs: `Slider.Value` (`Double`),
/// `CheckBox.IsChecked` (`Bool`), `ComboBox.SelectedIndex` (`Int32`), and
/// text (`Str`). `Float` properties arrive widened to [`VmValue::Double`].
#[derive(Debug, Clone, PartialEq)]
pub enum VmValue {
    Double(f64),
    Bool(bool),
    Int32(i32),
    Str(String),
}

impl VmValue {
    /// Decode the value handed to a [`PropertyChangeHandler`]. Returns `None`
    /// for property kinds this bridge doesn't surface (`Thickness`, `Color`,
    /// `Rect`, `ImageSource`, `BaseComponent`, `UInt32`) — those changes are
    /// simply not forwarded.
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
/// Build with the chained setters, then hand to [`NoesisViewModels::register`].
/// Property order is irrelevant to the app — the bridge keys everything by
/// name — but each name must be unique within the def and must match the
/// `{Binding <name>}` paths authored in the XAML.
#[derive(Debug, Clone)]
pub struct ViewModelDef {
    class_name: String,
    props: Vec<(String, PropType)>,
    target: AttachTarget,
}

impl ViewModelDef {
    /// Begin a def for the Noesis class `class_name`. The name only has to be
    /// unique among registered classes; since the VM is attached from code
    /// (never referenced in XAML), there's no namespace mapping to author.
    /// Defaults to attaching at the view root — override with
    /// [`Self::attach_to`].
    #[must_use]
    pub fn new(class_name: impl Into<String>) -> Self {
        Self {
            class_name: class_name.into(),
            props: Vec::new(),
            target: AttachTarget::Root,
        }
    }

    /// Declare a bindable dependency property. `name` is the `{Binding name}`
    /// path; `kind` is its [`PropType`] (`Double` for `Slider.Value`, `Bool`
    /// for `CheckBox.IsChecked`, `Int32` for `ComboBox.SelectedIndex`, …).
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

    /// Attach the instance as the `DataContext` of the element named `x_name`
    /// (resolved via `x:Name`). Scopes the binding to one subtree — useful
    /// when several panels each have their own view model.
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
// Main-world resource — NoesisViewModels
// ─────────────────────────────────────────────────────────────────────────────

#[derive(Default)]
struct VmShared {
    next_id: AtomicU64,
    registrations: Mutex<Vec<(ViewModelId, ViewModelDef)>>,
    writes: Mutex<Vec<(ViewModelId, String, VmValue)>>,
}

/// Push onto a `Mutex<Vec<_>>`, keeping the (poison-)`expect` out of the public
/// API so it doesn't pull `# Panics` sections onto every caller.
fn push_locked<T>(slot: &Mutex<Vec<T>>, item: T) {
    slot.lock()
        .expect("NoesisViewModels queue poisoned")
        .push(item);
}

/// Take everything out of a `Mutex<Vec<_>>`. Cheap when empty (no allocation).
fn drain_locked<T>(slot: &Mutex<Vec<T>>) -> Vec<T> {
    let mut guard = slot.lock().expect("NoesisViewModels queue poisoned");
    if guard.is_empty() {
        Vec::new()
    } else {
        std::mem::take(&mut *guard)
    }
}

/// Main-app entry point for the binding bridge. Insert via
/// [`NoesisViewModelPlugin`]; the render world receives an `Arc`-aliased copy
/// each frame through [`ExtractResource`], so registrations and writes pushed
/// here are drained render-side without copying.
///
/// All methods take `&self` (interior-mutable queues), so they're callable
/// from a plain `Res<NoesisViewModels>` — no `ResMut` contention.
#[derive(Resource, Clone, Default)]
pub struct NoesisViewModels {
    shared: Arc<VmShared>,
}

impl NoesisViewModels {
    /// Register a view model and return its stable [`ViewModelId`]. The class
    /// registration, instantiation, and `DataContext` attachment all happen
    /// render-side on a later frame (retained until the `View` exists), so
    /// this never blocks and is safe to call from a `Startup` system before
    /// the scene is built.
    ///
    /// Keep the returned id: it's how you address [`Self::set_f64`] and match
    /// [`NoesisViewModelChanged`].
    #[must_use]
    pub fn register(&self, def: ViewModelDef) -> ViewModelId {
        let id = ViewModelId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        push_locked(&self.shared.registrations, (id, def));
        id
    }

    /// Queue a `Double` write (e.g. `Slider.Value`). Applied render-side on the
    /// next frame; the bound control updates on the following `View::update`.
    pub fn set_f64(&self, id: ViewModelId, prop: impl Into<String>, value: f64) {
        self.push_write(id, prop, VmValue::Double(value));
    }

    /// Queue a `Bool` write (e.g. `CheckBox.IsChecked`).
    pub fn set_bool(&self, id: ViewModelId, prop: impl Into<String>, value: bool) {
        self.push_write(id, prop, VmValue::Bool(value));
    }

    /// Queue an `Int32` write (e.g. `ComboBox.SelectedIndex`).
    pub fn set_i32(&self, id: ViewModelId, prop: impl Into<String>, value: i32) {
        self.push_write(id, prop, VmValue::Int32(value));
    }

    /// Queue a `String` write.
    pub fn set_string(&self, id: ViewModelId, prop: impl Into<String>, value: impl Into<String>) {
        self.push_write(id, prop, VmValue::Str(value.into()));
    }

    fn push_write(&self, id: ViewModelId, prop: impl Into<String>, value: VmValue) {
        push_locked(&self.shared.writes, (id, prop.into(), value));
    }

    /// Drain pending registrations. Render-world only; cheap when empty.
    pub(crate) fn drain_registrations(&self) -> Vec<(ViewModelId, ViewModelDef)> {
        drain_locked(&self.shared.registrations)
    }

    /// Drain pending writes. Render-world only; cheap when empty.
    pub(crate) fn drain_writes(&self) -> Vec<(ViewModelId, String, VmValue)> {
        drain_locked(&self.shared.writes)
    }
}

impl ExtractResource for NoesisViewModels {
    type Source = NoesisViewModels;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Change side — shared queue + message + forwarding handler
// ─────────────────────────────────────────────────────────────────────────────

/// Shared queue between the render-world [`ViewModelChangeForwarder`] and the
/// main-world drain system. `Clone` is an `Arc` clone; both apps see the same
/// `Vec`, so a change pushed render-side appears on the main side next frame.
#[derive(Resource, Clone, Default)]
pub struct SharedVmChangedQueue(Arc<Mutex<Vec<(ViewModelId, String, VmValue)>>>);

impl ExtractResource for SharedVmChangedQueue {
    type Source = SharedVmChangedQueue;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

impl SharedVmChangedQueue {
    /// Push a change from a forwarder. Render-world only — main-world readers
    /// go through the [`NoesisViewModelChanged`] message.
    pub(crate) fn push(&self, id: ViewModelId, prop: String, value: VmValue) {
        self.0
            .lock()
            .expect("SharedVmChangedQueue poisoned")
            .push((id, prop, value));
    }

    /// Take the pending changes. Drained into [`NoesisViewModelChanged`] by the
    /// plugin; also exposed so headless tests can read the queue directly.
    /// Cheap when empty.
    #[must_use]
    pub fn drain(&self) -> Vec<(ViewModelId, String, VmValue)> {
        let mut guard = self.0.lock().expect("SharedVmChangedQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Bevy message written in `PreUpdate` when a view model's dependency property
/// changes — whether from a two-way bound control (a slider drag) or from a
/// Rust [`NoesisViewModels::set_f64`] write that altered the value. Read with
/// `MessageReader<NoesisViewModelChanged>`.
///
/// Noesis only fires the underlying change callback when the value actually
/// differs, so a no-op write doesn't echo. A write that *does* change the value
/// will surface here once — commit logic should be idempotent.
#[derive(Message, Debug, Clone)]
pub struct NoesisViewModelChanged {
    /// Which view model changed.
    pub id: ViewModelId,
    /// The bindable property's name, as declared in [`ViewModelDef::property`].
    pub prop: String,
    /// The new value.
    pub value: VmValue,
}

/// Render-thread [`PropertyChangeHandler`] that forwards a view model's
/// dependency-property changes onto a [`SharedVmChangedQueue`]. The plugin
/// installs one per registered view model; it's `pub` so headless tests can
/// wire the exact same forwarding the bridge uses.
pub struct ViewModelChangeForwarder {
    id: ViewModelId,
    /// Property index → name, mirroring the order DPs were added in. Shared
    /// (not cloned per call) because the callback fires on the hot path.
    prop_names: Arc<Vec<String>>,
    queue: SharedVmChangedQueue,
}

impl ViewModelChangeForwarder {
    /// Build a forwarder for view model `id`. `prop_names` must be indexed the
    /// same way the class's dependency properties were registered (addition
    /// order); `queue` is the shared sink the main world drains.
    #[must_use]
    pub fn new(id: ViewModelId, prop_names: Arc<Vec<String>>, queue: SharedVmChangedQueue) -> Self {
        Self {
            id,
            prop_names,
            queue,
        }
    }
}

impl PropertyChangeHandler for ViewModelChangeForwarder {
    fn on_changed(&mut self, _instance: Instance, prop_index: u32, value: PropertyValue<'_>) {
        let Some(name) = self.prop_names.get(prop_index as usize) else {
            return;
        };
        let Some(value) = VmValue::from_property(&value) else {
            return;
        };
        self.queue.push(self.id, name.clone(), value);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-world entry — VmEntry
// ─────────────────────────────────────────────────────────────────────────────

/// One live view model owned by [`NoesisRenderState`]. Field order matters:
/// `instance` drops before `registration`, mirroring the C++ refcount rule
/// that a class's instances release before the class unregisters.
pub(crate) struct VmEntry {
    pub(crate) id: ViewModelId,
    instance: ClassInstance,
    _registration: ClassRegistration,
    /// Property index → name (addition order), for name→index write lookups.
    prop_names: Vec<String>,
    target: AttachTarget,
    /// URI of the scene this VM is currently attached to, or `None` when not
    /// yet attached (or detached by a scene rebuild). Re-attach happens when
    /// this doesn't match the live scene's URI.
    attached_for_uri: Option<String>,
}

impl VmEntry {
    /// Register the Noesis class, instantiate it, and wire its change forwarder
    /// to `changed`. Returns `None` if the class registration or instantiation
    /// is rejected (e.g. a duplicate class name). Render-thread only — Noesis
    /// objects are thread-affine to the `View`.
    pub(crate) fn build(
        id: ViewModelId,
        def: &ViewModelDef,
        changed: &SharedVmChangedQueue,
    ) -> Option<Self> {
        let prop_names: Vec<String> = def.props.iter().map(|(n, _)| n.clone()).collect();
        let forwarder =
            ViewModelChangeForwarder::new(id, Arc::new(prop_names.clone()), changed.clone());
        let mut builder = ClassBuilder::new(&def.class_name, ClassBase::ContentControl, forwarder);
        for (name, kind) in &def.props {
            builder.add_property(name, *kind);
        }
        let registration = builder.register()?;
        let instance = registration.create_instance()?;
        Some(Self {
            id,
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

    /// Apply a write by property name. Returns `false` when the VM has no such
    /// property.
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
// Render-app systems
// ─────────────────────────────────────────────────────────────────────────────

/// Drain pending registrations → register class, instantiate, store. No-op
/// (queue retained) until [`NoesisRenderState`] exists.
pub(crate) fn sync_view_model_registrations(
    vms: Option<Res<NoesisViewModels>>,
    changed: Option<Res<SharedVmChangedQueue>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(vms), Some(changed), Some(mut state)) = (vms, changed, state) else {
        return;
    };
    state.register_view_models(&vms, &changed);
}

/// Drain pending writes → apply to the instances' dependency properties.
pub(crate) fn apply_view_model_writes(
    vms: Option<Res<NoesisViewModels>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(vms), Some(mut state)) = (vms, state) else {
        return;
    };
    state.apply_view_model_writes(&vms);
}

/// Attach any not-yet-attached view model as its target's `DataContext`. No-op
/// until the `View` (and the named target) exists.
pub(crate) fn attach_view_models(state: Option<ResMut<NoesisRenderState>>) {
    let Some(mut state) = state else {
        return;
    };
    state.attach_view_models();
}

/// Main-app system: drain the shared change queue into [`NoesisViewModelChanged`]
/// messages. Runs in `PreUpdate` so Update-stage systems see them the same
/// frame, mirroring [`drain_click_queue`](crate::events::drain_click_queue).
pub fn drain_vm_changed_queue(
    queue: Res<SharedVmChangedQueue>,
    mut messages: MessageWriter<NoesisViewModelChanged>,
) {
    for (id, prop, value) in queue.drain() {
        messages.write(NoesisViewModelChanged { id, prop, value });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the `ViewModel` / `DataContext` bridge: installs [`NoesisViewModels`]
/// and the shared change queue, extracts both to the render world, runs the
/// render-side register/write/attach passes in `RenderSystems::Prepare`, and
/// drains changes into [`NoesisViewModelChanged`] on the main app each frame.
///
/// Added transitively by [`crate::NoesisPlugin`].
pub struct NoesisViewModelPlugin;

impl Plugin for NoesisViewModelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisViewModels>()
            .insert_resource(SharedVmChangedQueue::default())
            .add_message::<NoesisViewModelChanged>()
            .add_plugins((
                ExtractResourcePlugin::<NoesisViewModels>::default(),
                ExtractResourcePlugin::<SharedVmChangedQueue>::default(),
            ))
            .add_systems(PreUpdate, drain_vm_changed_queue);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // Order within the chain matters: registrations build the instances,
        // writes seed them, then attach binds the seeded VM as DataContext —
        // so the initial values are present the first frame the binding
        // resolves. Each pass is a no-op-and-retain until its prerequisites
        // (state, then scene) exist, so cross-chain ordering vs.
        // `ensure_noesis_scene` only costs at most a one-frame attach lag.
        render_app.add_systems(
            Render,
            (
                sync_view_model_registrations,
                apply_view_model_writes,
                attach_view_models,
            )
                .chain()
                .in_set(RenderSystems::Prepare),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_allocates_monotonic_ids_and_queues_defs() {
        let vms = NoesisViewModels::default();
        let a = vms.register(ViewModelDef::new("A").property("Foo", PropType::Double));
        let b = vms.register(ViewModelDef::new("B").attach_to("Panel"));
        assert_eq!(a, ViewModelId(0));
        assert_eq!(b, ViewModelId(1));

        let drained = vms.drain_registrations();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].0, a);
        assert_eq!(drained[0].1.class_name(), "A");
        assert!(matches!(drained[0].1.target, AttachTarget::Root));
        assert!(matches!(drained[1].1.target, AttachTarget::Named(ref n) if n == "Panel"));
        // Second drain is empty (take semantics).
        assert!(vms.drain_registrations().is_empty());
    }

    #[test]
    fn writes_queue_round_trips_each_setter() {
        let vms = NoesisViewModels::default();
        let id = vms.register(ViewModelDef::new("VM"));
        vms.set_f64(id, "Vol", 0.5);
        vms.set_bool(id, "Muted", true);
        vms.set_i32(id, "Quality", 2);
        vms.set_string(id, "Name", "ultra");

        let drained = vms.drain_writes();
        assert_eq!(
            drained,
            vec![
                (id, "Vol".to_string(), VmValue::Double(0.5)),
                (id, "Muted".to_string(), VmValue::Bool(true)),
                (id, "Quality".to_string(), VmValue::Int32(2)),
                (id, "Name".to_string(), VmValue::Str("ultra".to_string())),
            ],
        );
        assert!(vms.drain_writes().is_empty());
    }

    #[test]
    fn changed_queue_drains_in_push_order() {
        let q = SharedVmChangedQueue::default();
        q.push(ViewModelId(7), "Foo".into(), VmValue::Double(1.0));
        q.push(ViewModelId(7), "Bar".into(), VmValue::Bool(false));
        let drained = q.drain();
        assert_eq!(
            drained,
            vec![
                (ViewModelId(7), "Foo".to_string(), VmValue::Double(1.0)),
                (ViewModelId(7), "Bar".to_string(), VmValue::Bool(false)),
            ],
        );
        assert!(q.drain().is_empty());
    }

    #[test]
    fn vm_value_decodes_known_property_kinds() {
        assert_eq!(
            VmValue::from_property(&PropertyValue::Double(0.25)),
            Some(VmValue::Double(0.25)),
        );
        assert_eq!(
            VmValue::from_property(&PropertyValue::Float(0.5)),
            Some(VmValue::Double(0.5)),
        );
        assert_eq!(
            VmValue::from_property(&PropertyValue::Bool(true)),
            Some(VmValue::Bool(true)),
        );
        assert_eq!(
            VmValue::from_property(&PropertyValue::Int32(3)),
            Some(VmValue::Int32(3)),
        );
        assert_eq!(
            VmValue::from_property(&PropertyValue::String(Some("x"))),
            Some(VmValue::Str("x".to_string())),
        );
        assert_eq!(
            VmValue::from_property(&PropertyValue::String(None)),
            Some(VmValue::Str(String::new())),
        );
        // Unsupported kinds are dropped (not forwarded).
        assert_eq!(VmValue::from_property(&PropertyValue::UInt32(9)), None);
    }
}

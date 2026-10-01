//! Rust-owned `ICommand`s: let XAML `Command="{Binding Name}"` controls invoke
//! Rust code.
//!
//! Add a [`NoesisCommands`] component, built from a [`CommandsDef`], to a
//! [`NoesisView`](crate::NoesisView) camera entity. The bridge registers a
//! Noesis class with one property per declared command, fills each with a
//! Rust-backed [`Command`], and sets an instance of the class as the
//! `DataContext` of the view root or a named element. A `Button`, `MenuItem` or
//! `InputBinding` bound with `Command="{Binding Fire}"` then invokes `Fire`.
//!
//! Each invocation arrives as a [`NoesisCommandInvoked`] message in the next
//! frame's `PreUpdate`, carrying the view entity, the command name, and the
//! `CommandParameter` if one was set.
//!
//! ```ignore
//! use noesis_bevy::commands::{CommandsDef, NoesisCommandInvoked, NoesisCommands};
//!
//! commands.entity(view).insert(NoesisCommands::new(
//!     CommandsDef::new("MainMenu.Commands")
//!         .command("NewGame")
//!         .command("Quit"),
//! ));
//!
//! fn on_command(mut invoked: MessageReader<NoesisCommandInvoked>) {
//!     for ev in invoked.read() {
//!         match ev.name.as_str() {
//!             "NewGame" => { /* ev.view */ }
//!             "Quit" => {}
//!             _ => {}
//!         }
//!     }
//! }
//! ```
//!
//! # `DataContext` conflicts
//!
//! The command host replaces the `DataContext` of its target. A
//! [`NoesisVm`](crate::viewmodel::NoesisVm) or plain view model on the same view
//! that targets the same element competes for it: the last one attached wins,
//! and the other's bindings stop resolving. The bridge logs a warning when this
//! happens. Attach one of them to a named element with
//! [`CommandsDef::attach_to`] to keep both.
//!
//! The bridge acts on view entities only. Changing the [`CommandsDef`] of a
//! re-inserted component rebuilds the host; removing the component tears it
//! down.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_runtime::classes::{
    ClassBuilder, ClassInstance, ClassRegistration, Instance, PropertyChangeHandler, PropertyValue,
};
use noesis_runtime::commands::{Command, CommandHandler, CommandParameterValue};
use noesis_runtime::ffi::{ClassBase, PropType};

use crate::render::{NoesisRenderState, NoesisSet, ReapOnRemove, add_bridge_reap};
use crate::viewmodel::AttachTarget;

/// The commands a view exposes: a Noesis class name, the command names, and
/// which element's `DataContext` receives them.
///
/// Build it with the chained setters and pass it to [`NoesisCommands::new`].
/// Command names must be unique within the def and match the `{Binding name}`
/// paths in the XAML.
#[derive(Debug, Clone, PartialEq)]
pub struct CommandsDef {
    class_name: String,
    commands: Vec<String>,
    target: AttachTarget,
}

impl CommandsDef {
    /// Start a def for the Noesis class `class_name`, attached at the view root
    /// unless you call [`Self::attach_to`].
    ///
    /// `class_name` must be unique across the process. Two views with the same
    /// commands need distinct class names (`"MainMenu.Commands.A"`, `".B"`); a
    /// duplicate fails to register and logs a warning.
    #[must_use]
    pub fn new(class_name: impl Into<String>) -> Self {
        Self {
            class_name: class_name.into(),
            commands: Vec::new(),
            target: AttachTarget::Root,
        }
    }

    /// Declare a command, bound in XAML as `Command="{Binding name}"`.
    #[must_use]
    pub fn command(mut self, name: impl Into<String>) -> Self {
        self.commands.push(name.into());
        self
    }

    /// Attach the command host as the view root's `DataContext` (the default).
    #[must_use]
    pub fn attach_to_root(mut self) -> Self {
        self.target = AttachTarget::Root;
        self
    }

    /// Attach the command host as the `DataContext` of the element named
    /// `x_name`.
    #[must_use]
    pub fn attach_to(mut self, x_name: impl Into<String>) -> Self {
        self.target = AttachTarget::Named(x_name.into());
        self
    }

    pub(crate) fn class_name(&self) -> &str {
        &self.class_name
    }
}

/// Per-view command host. Add it to a [`NoesisView`](crate::NoesisView) entity;
/// see the [module docs](self). Enable or disable individual commands at runtime
/// with [`set_enabled`](Self::set_enabled).
#[derive(Component)]
pub struct NoesisCommands {
    def: CommandsDef,
    pending_enables: Vec<(String, bool)>,
}

impl NoesisCommands {
    /// A command host for `def`. Registration and the `DataContext` attach run
    /// in the reconcile system once the view's scene exists, so inserting it from
    /// `Startup` is fine. All commands start enabled.
    #[must_use]
    pub fn new(def: CommandsDef) -> Self {
        Self {
            def,
            pending_enables: Vec::new(),
        }
    }

    /// Queue an enabled-state change for command `name`, applied by the next
    /// reconcile. A disabled command's `CanExecute` returns `false`, so bound
    /// controls grey out and stop invoking it. An unknown name logs a warning.
    pub fn set_enabled(&mut self, name: impl Into<String>, enabled: bool) {
        self.pending_enables.push((name.into(), enabled));
    }

    /// Shorthand for [`set_enabled(name, true)`](Self::set_enabled).
    pub fn enable(&mut self, name: impl Into<String>) {
        self.set_enabled(name, true);
    }

    /// Shorthand for [`set_enabled(name, false)`](Self::set_enabled).
    pub fn disable(&mut self, name: impl Into<String>) {
        self.set_enabled(name, false);
    }

    pub(crate) fn def(&self) -> &CommandsDef {
        &self.def
    }

    /// Read through `&self` so idle frames don't trip change detection.
    pub(crate) fn has_pending_enables(&self) -> bool {
        !self.pending_enables.is_empty()
    }

    pub(crate) fn take_pending_enables(&mut self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.pending_enables)
    }
}

/// Command invocations waiting to become [`NoesisCommandInvoked`] messages, as
/// `(view, name, parameter)`. Filled by [`CommandForwarder`] during
/// `PostUpdate` and drained in `PreUpdate`.
#[derive(Resource, Clone, Default)]
pub struct SharedCommandQueue(Arc<Mutex<Vec<(Entity, String, Option<String>)>>>);

impl SharedCommandQueue {
    pub(crate) fn push(&self, view: Entity, name: String, parameter: Option<String>) {
        self.0
            .lock()
            .expect("SharedCommandQueue poisoned")
            .push((view, name, parameter));
    }

    /// Take every pending invocation. [`drain_command_queue`] calls this each
    /// frame; tests can call it directly.
    ///
    /// # Panics
    ///
    /// If the queue's mutex was poisoned by an earlier panic.
    #[must_use]
    pub fn drain(&self) -> Vec<(Entity, String, Option<String>)> {
        let mut guard = self.0.lock().expect("SharedCommandQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Sent when a control invokes one of a view's declared commands. Arrives in the
/// `PreUpdate` after the invocation.
#[derive(Message, Debug, Clone)]
pub struct NoesisCommandInvoked {
    /// The [`NoesisView`](crate::NoesisView) entity whose command was invoked.
    pub view: Entity,
    /// The command's name, as declared in [`CommandsDef::command`].
    pub name: String,
    /// The control's `CommandParameter` as a string. `i32`, `f64` and `bool`
    /// parameters are formatted with `to_string`. `None` when there is no
    /// parameter or it has another type.
    pub parameter: Option<String>,
}

/// [`CommandHandler`] that pushes each `Execute` onto a [`SharedCommandQueue`],
/// tagged with its view and command name. `CanExecute` reads a shared
/// [`AtomicBool`]. Apps use [`NoesisCommands`]; this is `pub` for headless
/// tests.
pub struct CommandForwarder {
    view: Entity,
    name: String,
    queue: SharedCommandQueue,
    enabled: Arc<AtomicBool>,
}

impl CommandForwarder {
    /// A forwarder for command `name` on `view`. After flipping `enabled`, call
    /// [`Command::raise_can_execute_changed`](noesis_runtime::commands::Command::raise_can_execute_changed)
    /// so bound controls re-query.
    #[must_use]
    pub fn new(
        view: Entity,
        name: String,
        queue: SharedCommandQueue,
        enabled: Arc<AtomicBool>,
    ) -> Self {
        Self {
            view,
            name,
            queue,
            enabled,
        }
    }
}

impl CommandHandler for CommandForwarder {
    fn can_execute(&self, _param: CommandParameterValue) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    fn execute(&self, param: CommandParameterValue) {
        self.queue
            .push(self.view, self.name.clone(), decode_command_param(&param));
    }
}

/// XAML `CommandParameter="..."` literals box as strings; other types fall through.
fn decode_command_param(param: &CommandParameterValue) -> Option<String> {
    if param.is_none() {
        return None;
    }
    if let Some(s) = param.as_str() {
        return Some(s.to_owned());
    }
    if let Some(i) = param.as_i32() {
        return Some(i.to_string());
    }
    if let Some(f) = param.as_f64() {
        return Some(f.to_string());
    }
    if let Some(b) = param.as_bool() {
        return Some(b.to_string());
    }
    None
}

/// [`ClassBuilder::new`] requires a handler; command properties are only set at build.
struct NoCommandChanges;

impl PropertyChangeHandler for NoCommandChanges {
    fn on_changed(&self, _instance: Instance, _prop_index: u32, _value: PropertyValue<'_>) {}
}

/// One view's live command host. Field order is drop order: `instance` must
/// release before `_registration` unregisters the class, and the [`Command`]s
/// drop after the instance has released its property references.
pub(crate) struct CommandEntry {
    instance: ClassInstance,
    _registration: ClassRegistration,
    /// One per declared name, in property order. Held for
    /// `raise_can_execute_changed`; each property holds its own reference too.
    commands: Vec<Command>,
    /// Per-command enabled flag shared with the matching [`CommandForwarder`].
    enabled: Vec<Arc<AtomicBool>>,
    /// Rebuild fingerprint ([`Self::matches`]) and the name-to-index map for
    /// `commands` and `enabled`.
    def: CommandsDef,
    /// URI of the scene this host is attached to; `None` until attached or after
    /// a rebuild.
    attached_for_uri: Option<String>,
}

impl CommandEntry {
    /// Register the class, instantiate it, and set one [`Command`] per declared
    /// name. `None` if Noesis rejects the registration (e.g. a duplicate class
    /// name) or the instance.
    pub(crate) fn build(
        view: Entity,
        def: &CommandsDef,
        queue: &SharedCommandQueue,
    ) -> Option<Self> {
        let mut builder =
            ClassBuilder::new(&def.class_name, ClassBase::ContentControl, NoCommandChanges);
        for name in &def.commands {
            builder.add_property(name, PropType::BaseComponent);
        }
        let registration = builder.register()?;
        let instance = registration.create_instance()?;

        let mut commands = Vec::with_capacity(def.commands.len());
        let mut enabled = Vec::with_capacity(def.commands.len());
        for (idx, name) in def.commands.iter().enumerate() {
            let flag = Arc::new(AtomicBool::new(true));
            let forwarder =
                CommandForwarder::new(view, name.clone(), queue.clone(), Arc::clone(&flag));
            let command = Command::new(forwarder);
            instance.handle().set_command(idx as u32, &command);
            commands.push(command);
            enabled.push(flag);
        }

        Some(Self {
            instance,
            _registration: registration,
            commands,
            enabled,
            def: def.clone(),
            attached_for_uri: None,
        })
    }

    /// `false` when a re-inserted [`NoesisCommands`] changed the def and the host
    /// must be rebuilt.
    pub(crate) fn matches(&self, def: &CommandsDef) -> bool {
        &self.def == def
    }

    pub(crate) fn target(&self) -> &AttachTarget {
        &self.def.target
    }

    pub(crate) fn instance(&self) -> &ClassInstance {
        &self.instance
    }

    /// Set command `name`'s enabled flag and make bound controls re-query.
    /// `false` when the host has no such command.
    pub(crate) fn set_enabled(&self, name: &str, value: bool) -> bool {
        let Some(idx) = self.def.commands.iter().position(|n| n == name) else {
            return false;
        };
        self.enabled[idx].store(value, Ordering::Relaxed);
        self.commands[idx].raise_can_execute_changed();
        true
    }

    pub(crate) fn needs_attach(&self, uri: &str) -> bool {
        self.attached_for_uri.as_deref() != Some(uri)
    }

    pub(crate) fn mark_attached(&mut self, uri: &str) {
        self.attached_for_uri = Some(uri.to_owned());
    }

    /// Mark detached so the next attach pass targets the rebuilt scene.
    pub(crate) fn reset_attach(&mut self) {
        self.attached_for_uri = None;
    }
}

/// Build or rebuild each view's command host, apply queued enabled-state edits,
/// then attach any host not yet attached to the current scene.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_commands(
    mut views: Query<(Entity, &mut NoesisCommands)>,
    queue: Res<SharedCommandQueue>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, mut cmds) in &mut views {
        state.ensure_commands(entity, cmds.def(), &queue);
        if cmds.has_pending_enables() {
            let enables = cmds.take_pending_enables();
            state.apply_command_enables_for(entity, &enables);
        }
    }
    state.attach_commands();
}

/// Turn queued invocations into [`NoesisCommandInvoked`] messages. Runs in
/// `PreUpdate`.
#[allow(clippy::needless_pass_by_value)]
pub fn drain_command_queue(
    queue: Res<SharedCommandQueue>,
    mut messages: MessageWriter<NoesisCommandInvoked>,
) {
    for (view, name, parameter) in queue.drain() {
        messages.write(NoesisCommandInvoked {
            view,
            name,
            parameter,
        });
    }
}

impl ReapOnRemove for NoesisCommands {
    fn reap(state: &mut NoesisRenderState, entity: Entity) {
        state.reap_commands_for(entity);
    }
}

/// Registers the [`NoesisCommands`] systems, [`SharedCommandQueue`] and
/// [`NoesisCommandInvoked`]. Added by [`crate::NoesisPlugin`].
pub struct NoesisCommandsPlugin;

impl Plugin for NoesisCommandsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SharedCommandQueue::default())
            .add_message::<NoesisCommandInvoked>()
            .add_systems(PreUpdate, drain_command_queue)
            .add_systems(PostUpdate, sync_commands.in_set(NoesisSet::Apply));
        add_bridge_reap::<NoesisCommands>(app);
    }
}

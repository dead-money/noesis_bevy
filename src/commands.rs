//! Per-view Rust-owned `ICommand` bridge (TODO §3).
//!
//! Lets XAML `Command="{Binding Name}"` controls (a `Button`, a `MenuItem`, an
//! `InputBinding`/`MouseBinding`, …) invoke Rust logic without touching Noesis
//! pointers. Add a [`NoesisCommands`] component to the view's camera entity: it
//! declares the named commands (a [`CommandsDef`]); the bridge registers a
//! Noesis class whose dependency properties are each a
//! [`PropType::BaseComponent`] holding a Rust-backed
//! [`Command`](noesis_runtime::commands::Command), creates an instance, and
//! attaches it as the view's (or a named element's) `DataContext`. Authoring
//! `Command="{Binding Fire}"` then resolves `Fire` to that command.
//!
//! When the UI invokes a command, the command's `Execute` runs on the
//! view-driving thread and the bridge surfaces a [`NoesisCommandInvoked`]
//! message carrying the originating `view` entity and the command `name`.
//!
//! This is the read-watch counterpart of the write-only
//! [`viewmodel`](crate::viewmodel) bridge, and it deliberately mirrors its
//! shape: a declarative per-view component + a render-state entry attached as a
//! `DataContext`. The difference is the DP payload — a `BaseComponent` command
//! object rather than a scalar value — and the direction of flow (UI → Rust).
//!
//! ```ignore
//! use dm_noesis_bevy::commands::{NoesisCommands, CommandsDef, NoesisCommandInvoked};
//!
//! commands.entity(view).insert(NoesisCommands::new(
//!     CommandsDef::new("MainMenu.Commands")
//!         .command("NewGame")
//!         .command("Quit"),
//! ));
//!
//! // observe UI -> Rust:
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
//! # The binding mechanism (how XAML reaches a Rust command)
//!
//! Noesis exposes a command to a control's `Command` property the same way it
//! exposes any object to a binding: the bound source must be a
//! `DependencyObject` carrying the value under the bound path. The runtime's
//! [`Instance::set_command`](noesis_runtime::classes::Instance::set_command) sets
//! a `BaseComponent`-typed DP to a Rust [`Command`] (whose runtime type is an
//! `ICommand`). A control bound `Command="{Binding Fire}"` against that instance
//! as its `DataContext` reads the DP and invokes it on activation. This is the
//! exact path the runtime's `commands` module documents (steps 1–4 of its
//! module docs). Confidence is high; see `NOTES.md` for the one open gap
//! (decoding the command *parameter*).
//!
//! # Threading & lifetime
//!
//! The class registration + instance + per-command [`Command`] objects are
//! created on the main thread (Noesis is thread-affine to the `View`) and owned
//! per-view in [`NoesisRenderState`](crate::render), released before
//! `noesis_runtime::shutdown`. A command's `Execute` fires on the main thread;
//! the forwarder pushes onto a [`SharedCommandQueue`] drained into messages.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_runtime::classes::{
    ClassBuilder, ClassInstance, ClassRegistration, Instance, PropertyChangeHandler, PropertyValue,
};
use noesis_runtime::commands::{Command, CommandHandler, CommandParameterValue};
use noesis_runtime::ffi::{ClassBase, PropType};

use crate::render::{NoesisRenderState, NoesisSet};
// Reuse the viewmodel bridge's attach-target enum verbatim — a command host is
// attached as a `DataContext` exactly like a view model.
use crate::viewmodel::AttachTarget;

// ─────────────────────────────────────────────────────────────────────────────
// CommandsDef — declarative recipe
// ─────────────────────────────────────────────────────────────────────────────

/// A declarative recipe for a view's commands: a Noesis class name, the ordered
/// set of command names, and where to attach the instance as a `DataContext`.
///
/// Build with the chained setters, then hand to [`NoesisCommands::new`]. Each
/// command name must be unique within the def and match the `{Binding <name>}`
/// paths authored in the XAML's `Command="…"` attributes.
#[derive(Debug, Clone)]
pub struct CommandsDef {
    class_name: String,
    commands: Vec<String>,
    target: AttachTarget,
}

impl CommandsDef {
    /// Begin a def for the Noesis class `class_name`. Defaults to attaching at
    /// the view root — override with [`Self::attach_to`].
    ///
    /// `class_name` must be globally unique: Noesis class registration is keyed
    /// by name, so two views needing the same commands must use distinct class
    /// names (e.g. `"MainMenu.Commands.A"` / `"…B"`).
    #[must_use]
    pub fn new(class_name: impl Into<String>) -> Self {
        Self {
            class_name: class_name.into(),
            commands: Vec::new(),
            target: AttachTarget::Root,
        }
    }

    /// Declare a named command. `name` is the `{Binding name}` path authored on
    /// the control's `Command` property.
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

// ─────────────────────────────────────────────────────────────────────────────
// Per-view component
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view command-host component. Attach to a [`NoesisView`](crate::NoesisView)
/// entity. Holds the [`CommandsDef`] and a queue of pending enabled-state edits;
/// mutate it (`set_enabled`) to gate a command, which applies on the next frame
/// and re-queries any bound control's `IsEnabled`.
#[derive(Component)]
pub struct NoesisCommands {
    def: CommandsDef,
    pending_enables: Vec<(String, bool)>,
}

impl NoesisCommands {
    /// Build a command host from its [`CommandsDef`]. The class registration,
    /// instantiation, and `DataContext` attach happen on a later frame (retained
    /// until the view exists), so this is safe from `Startup`.
    #[must_use]
    pub fn new(def: CommandsDef) -> Self {
        Self {
            def,
            pending_enables: Vec::new(),
        }
    }

    /// Queue an enabled-state change for command `name`. A disabled command's
    /// `CanExecute` reports `false`, so a bound `Button` greys out and stops
    /// invoking it. Applies on the next frame.
    pub fn set_enabled(&mut self, name: impl Into<String>, enabled: bool) {
        self.pending_enables.push((name.into(), enabled));
    }

    pub(crate) fn def(&self) -> &CommandsDef {
        &self.def
    }

    /// Take the queued enabled-state edits (called by the reconcile system).
    pub(crate) fn take_pending_enables(&mut self) -> Vec<(String, bool)> {
        std::mem::take(&mut self.pending_enables)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Invocation side — shared queue + message + forwarding handler
// ─────────────────────────────────────────────────────────────────────────────

/// Queue between the (main-thread) [`CommandForwarder`] callbacks and the drain
/// system. Entries carry the originating view entity, the command name, and the
/// (currently always-`None`) decoded parameter — see `NOTES.md`.
#[derive(Resource, Clone, Default)]
pub struct SharedCommandQueue(Arc<Mutex<Vec<(Entity, String, Option<String>)>>>);

impl SharedCommandQueue {
    /// Push an invocation from a forwarder.
    pub(crate) fn push(&self, view: Entity, name: String, parameter: Option<String>) {
        self.0
            .lock()
            .expect("SharedCommandQueue poisoned")
            .push((view, name, parameter));
    }

    /// Take the pending invocations. Drained into [`NoesisCommandInvoked`]; also
    /// exposed so headless tests can read the queue directly.
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

/// Emitted when a UI control invokes one of a view's declared commands.
#[derive(Message, Debug, Clone)]
pub struct NoesisCommandInvoked {
    /// The [`NoesisView`](crate::NoesisView) entity whose command was invoked.
    pub view: Entity,
    /// The command's name, as declared in [`CommandsDef::command`].
    pub name: String,
    /// The decoded command parameter, when the bound control supplied one.
    ///
    /// Currently always `None`: the runtime hands the parameter to the Rust
    /// handler as an opaque borrowed `BaseComponent*`
    /// ([`CommandParameterValue`](noesis_runtime::commands::CommandParameterValue)), and
    /// the `unsafe`-free Bevy crate has no safe way to unbox it (the runtime's
    /// `ConvertArg::new` is `pub(crate)`). Decoding it is a small
    /// `noesis_runtime` addition — see `NOTES.md` "OPEN RISKS".
    pub parameter: Option<String>,
}

/// Main-thread [`CommandHandler`] that forwards a single command's `Execute`
/// onto a [`SharedCommandQueue`], tagged with the owning view entity and the
/// command name. `can_execute` gates the command on a shared [`AtomicBool`] the
/// reconcile system flips for [`NoesisCommands::set_enabled`]. `pub` so headless
/// tests can wire the same forwarding.
pub struct CommandForwarder {
    view: Entity,
    name: String,
    queue: SharedCommandQueue,
    enabled: Arc<AtomicBool>,
}

impl CommandForwarder {
    /// Build a forwarder for `name`'s command owned by `view`. `enabled` gates
    /// `can_execute`; flip it then call
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

    fn execute(&self, _param: CommandParameterValue) {
        // The parameter is an opaque borrowed `BaseComponent*` we can't safely
        // unbox from this crate; surface `None` for now (see `NoesisCommandInvoked`).
        self.queue.push(self.view, self.name.clone(), None);
    }
}

/// No-op [`PropertyChangeHandler`] for the command-host class. Command DPs are
/// set once at build time and never written from XAML, so there's nothing to
/// observe — but [`ClassBuilder::new`] requires a handler.
struct NoCommandChanges;

impl PropertyChangeHandler for NoCommandChanges {
    fn on_changed(&self, _instance: Instance, _prop_index: u32, _value: PropertyValue<'_>) {}
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-world entry — CommandEntry
// ─────────────────────────────────────────────────────────────────────────────

/// One live command host, owned per-view by [`NoesisRenderState`]. Field order
/// matters: `instance` drops before `registration`, mirroring the C++ refcount
/// rule that a class's instances release before the class unregisters. The
/// owned [`Command`]s drop after the instance has released its DP references.
pub(crate) struct CommandEntry {
    instance: ClassInstance,
    _registration: ClassRegistration,
    /// The Rust-backed command objects, one per declared name (DP addition
    /// order). Held so we can call `raise_can_execute_changed` after an enabled
    /// flip; the DP also holds its own reference, so the command stays live
    /// while bound regardless.
    commands: Vec<Command>,
    /// Per-command enabled flag shared with the matching [`CommandForwarder`].
    enabled: Vec<Arc<AtomicBool>>,
    /// Command name → dense index (DP / `commands` / `enabled` order).
    names: Vec<String>,
    target: AttachTarget,
    /// URI of the scene this host is currently attached to, or `None` when not
    /// yet attached / detached by a scene rebuild.
    attached_for_uri: Option<String>,
}

impl CommandEntry {
    /// Register the Noesis class (one `BaseComponent` DP per command), create an
    /// instance, build a Rust [`Command`] per name (its forwarder tagged with
    /// `view` and pushing to `queue`), and assign each to its DP. `None` if
    /// registration / instantiation is rejected (e.g. a duplicate class name).
    /// Main-thread only.
    pub(crate) fn build(
        view: Entity,
        def: &CommandsDef,
        queue: &SharedCommandQueue,
    ) -> Option<Self> {
        let names: Vec<String> = def.commands.clone();
        let mut builder =
            ClassBuilder::new(&def.class_name, ClassBase::ContentControl, NoCommandChanges);
        for name in &names {
            builder.add_property(name, PropType::BaseComponent);
        }
        let registration = builder.register()?;
        let instance = registration.create_instance()?;

        let mut commands = Vec::with_capacity(names.len());
        let mut enabled = Vec::with_capacity(names.len());
        for (idx, name) in names.iter().enumerate() {
            let flag = Arc::new(AtomicBool::new(true));
            let forwarder =
                CommandForwarder::new(view, name.clone(), queue.clone(), Arc::clone(&flag));
            let command = Command::new(forwarder);
            // Set the BaseComponent DP at `idx` to the command (the C++ side
            // takes its own reference; `command` keeps ours).
            instance.handle().set_command(idx as u32, &command);
            commands.push(command);
            enabled.push(flag);
        }

        Some(Self {
            instance,
            _registration: registration,
            commands,
            enabled,
            names,
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

    /// Apply an enabled-state edit by command name, re-querying bound controls.
    /// `false` when the host has no such command.
    pub(crate) fn set_enabled(&self, name: &str, value: bool) -> bool {
        let Some(idx) = self.names.iter().position(|n| n == name) else {
            return false;
        };
        self.enabled[idx].store(value, Ordering::Relaxed);
        // Tell bound controls to re-query `CanExecute` (re-evaluate `IsEnabled`).
        self.commands[idx].raise_can_execute_changed();
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

/// Reconcile every view's [`NoesisCommands`]: build its render-side entry on
/// first sight, apply queued enabled-state edits, then (re-)attach it as its
/// target's `DataContext`.
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
        let enables = cmds.take_pending_enables();
        if !enables.is_empty() {
            state.apply_command_enables_for(entity, &enables);
        }
    }
    state.attach_commands();
}

/// Drain the shared invocation queue into [`NoesisCommandInvoked`] messages.
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

/// Wires the per-view `ICommand` bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisCommandsPlugin;

impl Plugin for NoesisCommandsPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(SharedCommandQueue::default())
            .add_message::<NoesisCommandInvoked>()
            .add_systems(PreUpdate, drain_command_queue)
            .add_systems(PostUpdate, sync_commands.in_set(NoesisSet::Apply));
    }
}

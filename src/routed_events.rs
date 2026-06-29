//! Per-view **generic routed-event** bridge — surface any `RoutedEvent` (mouse,
//! key, focus, drag, manipulation, lifecycle bubbling, …) from named elements
//! of a single [`NoesisView`] as Bevy messages.
//!
//! This is the general case of the [`crate::events`] click/keydown bridge: where
//! `NoesisClickWatch` hard-codes `BaseButton::Click` and `NoesisKeyDownWatch`
//! hard-codes `UIElement::KeyDown`, [`NoesisEventWatch`] subscribes an arbitrary
//! `(x:Name, RoutedEvent)` pair through `noesis_runtime::events::subscribe_event`.
//!
//! Add a [`NoesisEventWatch`] component to the view's camera entity listing the
//! `(name, event)` pairs to observe. The reconcile system keeps each view's live
//! subscription set in sync; a fired event surfaces as a [`NoesisRoutedEvent`]
//! message carrying the originating `view` entity, the element `name`, the
//! [`RoutedEvent`] that fired, and a best-effort [`RoutedEventSnapshot`] of the
//! event args (position / key / button / wheel / char / new-size) read out
//! before the borrowed C++ args go out of scope.
//!
//! ```ignore
//! use noesis_runtime::events::RoutedEvent;
//!
//! commands.entity(view).insert(NoesisEventWatch::new([
//!     EventWatchEntry::new("Target", RoutedEvent::MouseDown),
//!     EventWatchEntry::new("Target", RoutedEvent::MouseEnter),
//!     // Stop a preview keydown from reaching the focused TextBox:
//!     EventWatchEntry::new("Box", RoutedEvent::PreviewKeyDown).mark_handled(),
//! ]));
//!
//! fn on_routed(mut events: MessageReader<NoesisRoutedEvent>) {
//!     for ev in events.read() {
//!         // ev.view: Entity, ev.name: String, ev.event: RoutedEvent,
//!         // ev.args.position: Option<(f32, f32)>, …
//!     }
//! }
//! ```
//!
//! Routed-event callbacks fire from inside Noesis's input pump while the view is
//! driven (on whatever thread drains [`crate::input::NoesisInputQueue`] onto the
//! `View`); they push `(view, name, event, snapshot)` onto a small `Arc<Mutex>`
//! queue that the `PreUpdate` drain turns into messages the next frame. Every
//! fire emits one message — there is no per-frame dedupe (unlike the read-watch
//! text/dp bridges); a routed event *is* the change.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
pub use noesis_runtime::events::{EventArgs, RoutedEvent};
pub use noesis_runtime::view::{Key, MouseButton};

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Event-arg snapshot
// ─────────────────────────────────────────────────────────────────────────────

/// Owned, `Send` snapshot of a routed event's arguments, captured inside the
/// callback before the borrowed C++ [`EventArgs`] go out of scope. Every field
/// is `None` for events that don't carry it (e.g. a `MouseEnter` has a
/// `position` but no `key`); the un-applied default is therefore "all `None`",
/// which makes a captured snapshot trivially distinguishable from a missing one.
///
/// Mirrors the typed accessors on [`EventArgs`]; add a field here (and a line in
/// [`Self::capture`]) when a new accessor earns surfacing.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoutedEventSnapshot {
    /// Pointer position in the source element's coordinate space (mouse /
    /// mouse-button / wheel events).
    pub position: Option<(f32, f32)>,
    /// Changed mouse button (mouse-button events).
    pub mouse_button: Option<MouseButton>,
    /// Wheel rotation delta, ~120 per notch (wheel events).
    pub wheel_delta: Option<i32>,
    /// Pressed/released key, mapped to the safe [`Key`] mirror (key events).
    pub key: Option<Key>,
    /// Input character / code point (text-input events).
    pub text_char: Option<char>,
    /// New size in DIPs (`SizeChanged`).
    pub new_size: Option<(f32, f32)>,
}

impl RoutedEventSnapshot {
    /// Read every typed accessor off the borrowed live args into an owned,
    /// `Send` snapshot. Called from the routed-event callback while `args` is
    /// still valid. Pure reads — never retains the borrow or any raw pointer.
    #[must_use]
    pub(crate) fn capture(args: &EventArgs) -> Self {
        Self {
            position: args.position(),
            mouse_button: args.mouse_button(),
            wheel_delta: args.wheel_delta(),
            key: args.key(),
            text_char: args.text_char(),
            new_size: args.new_size(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Message + watch component
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when a watched element raises its subscribed [`RoutedEvent`].
#[derive(Message, Debug, Clone)]
pub struct NoesisRoutedEvent {
    /// The [`NoesisView`](crate::NoesisView) entity whose element raised the event.
    pub view: Entity,
    /// `x:Name` of the element the handler was attached to.
    pub name: String,
    /// Which routed event fired.
    pub event: RoutedEvent,
    /// Best-effort snapshot of the event args (all-`None` for events that carry
    /// nothing we surface).
    pub args: RoutedEventSnapshot,
}

/// One entry in [`NoesisEventWatch`]: an element `x:Name`, the [`RoutedEvent`] to
/// subscribe, and two routing flags.
///
/// * `mark_handled` — when `true`, the callback returns `handled = true`,
///   marking the routed event handled and stopping bubbling/tunneling past this
///   element (e.g. swallow a `PreviewKeyDown` so it never reaches a `TextBox`).
///   Default `false`: observe without consuming.
/// * `handled_too` — forwarded to `subscribe_event`; when `true`, this handler
///   still runs even if a prior handler on the *same* element already marked the
///   event handled. Default `false`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventWatchEntry {
    pub name: String,
    pub event: RoutedEvent,
    pub mark_handled: bool,
    pub handled_too: bool,
}

impl EventWatchEntry {
    /// Watch `event` on the element named `name`, observing without consuming.
    pub fn new(name: impl Into<String>, event: RoutedEvent) -> Self {
        Self {
            name: name.into(),
            event,
            mark_handled: false,
            handled_too: false,
        }
    }

    /// Builder — mark the event handled when it fires (stops further routing).
    #[must_use]
    pub fn mark_handled(mut self) -> Self {
        self.mark_handled = true;
        self
    }

    /// Builder — also run when a prior same-element handler already marked the
    /// event handled.
    #[must_use]
    pub fn handled_too(mut self) -> Self {
        self.handled_too = true;
        self
    }
}

/// Per-view component: `(x:Name, RoutedEvent)` pairs to subscribe routed-event
/// handlers against. Add to a [`NoesisView`](crate::NoesisView) entity. Entries
/// are diff-synced each frame — adding installs a subscription, removing tears
/// it down; changing an entry's `mark_handled`/`handled_too` re-binds it (the
/// flags are captured by the callback at subscription time).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisEventWatch {
    pub entries: Vec<EventWatchEntry>,
}

impl NoesisEventWatch {
    pub fn new(entries: impl IntoIterator<Item = EventWatchEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Queue (callback → drain)
// ─────────────────────────────────────────────────────────────────────────────

/// Queue between the routed-event callbacks (fired from the input pump) and the
/// drain system. `Clone` is an `Arc` clone. Entries carry the originating view
/// entity, the element name, the event, and the captured arg snapshot.
#[derive(Resource, Clone, Default)]
pub struct SharedRoutedEventQueue(
    pub(crate) Arc<Mutex<Vec<(Entity, String, RoutedEvent, RoutedEventSnapshot)>>>,
);

impl SharedRoutedEventQueue {
    /// Push `(view, name, event, snapshot)` from a routed-event callback.
    pub(crate) fn push(
        &self,
        view: Entity,
        name: String,
        event: RoutedEvent,
        args: RoutedEventSnapshot,
    ) {
        self.0
            .lock()
            .expect("SharedRoutedEventQueue poisoned")
            .push((view, name, event, args));
    }

    fn drain(&self) -> Vec<(Entity, String, RoutedEvent, RoutedEventSnapshot)> {
        let mut guard = self.0.lock().expect("SharedRoutedEventQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Drain the routed-event queue into [`NoesisRoutedEvent`] messages (one per fire).
#[allow(clippy::needless_pass_by_value)]
pub fn drain_routed_event_queue(
    queue: Res<SharedRoutedEventQueue>,
    mut messages: MessageWriter<NoesisRoutedEvent>,
) {
    for (view, name, event, args) in queue.drain() {
        messages.write(NoesisRoutedEvent {
            view,
            name,
            event,
            args,
        });
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Reconcile system
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisEventWatch`] against its live subscription set.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_event_subscriptions(
    views: Query<(Entity, &NoesisEventWatch)>,
    queue: Res<SharedRoutedEventQueue>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, watch) in &views {
        state.sync_event_subscriptions_for(entity, &watch.entries, &queue);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the per-view generic routed-event bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisRoutedEventsPlugin;

impl Plugin for NoesisRoutedEventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisRoutedEvent>()
            .insert_resource(SharedRoutedEventQueue::default())
            .add_systems(PreUpdate, drain_routed_event_queue)
            .add_systems(
                PostUpdate,
                sync_event_subscriptions.in_set(NoesisSet::Apply),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_routed_queue_drain_takes_all_and_resets() {
        let q = SharedRoutedEventQueue::default();
        let v = Entity::PLACEHOLDER;
        q.push(
            v,
            "Alpha".into(),
            RoutedEvent::MouseDown,
            RoutedEventSnapshot::default(),
        );
        q.push(
            v,
            "Beta".into(),
            RoutedEvent::MouseUp,
            RoutedEventSnapshot::default(),
        );
        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].1, "Alpha");
        assert_eq!(drained[0].2, RoutedEvent::MouseDown);
        assert_eq!(drained[1].2, RoutedEvent::MouseUp);
        assert!(q.drain().is_empty());
    }

    #[test]
    fn event_watch_entry_builders() {
        let e = EventWatchEntry::new("Box", RoutedEvent::PreviewKeyDown)
            .mark_handled()
            .handled_too();
        assert_eq!(e.name, "Box");
        assert_eq!(e.event, RoutedEvent::PreviewKeyDown);
        assert!(e.mark_handled);
        assert!(e.handled_too);

        let d = EventWatchEntry::new("Target", RoutedEvent::MouseDown);
        assert!(!d.mark_handled);
        assert!(!d.handled_too);
    }

    #[test]
    fn event_watch_constructor_collects_entries() {
        let w = NoesisEventWatch::new([
            EventWatchEntry::new("A", RoutedEvent::MouseEnter),
            EventWatchEntry::new("B", RoutedEvent::MouseLeave),
        ]);
        assert_eq!(w.entries.len(), 2);
        assert_eq!(w.entries[0].name, "A");
        assert_eq!(w.entries[1].event, RoutedEvent::MouseLeave);
    }
}

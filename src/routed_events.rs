//! Surfaces any Noesis `RoutedEvent` (mouse, key, focus, drag, manipulation,
//! lifecycle) raised on named elements as Bevy messages and observer events.
//!
//! This is the general form of the [`crate::events`] click and keydown bridges,
//! which each hard-code one event. Add a [`NoesisEventWatch`] to a
//! [`NoesisView`](crate::NoesisView) camera (or to a
//! [`UiPanel`](crate::panel::UiPanel) entity, to reach names inside the panel's
//! fragment) listing `(x:Name, RoutedEvent)` pairs. Each fire produces one
//! [`NoesisRoutedEvent`] message and one [`UiRoutedEvent`] observer trigger,
//! both carrying a [`RoutedEventSnapshot`] of the event args.
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
//!         // ev.view, ev.name, ev.event, ev.args.position, ...
//!     }
//! }
//! ```
//!
//! Handlers run on the main thread, inside whichever Noesis call raises the
//! event: input processing in [`NoesisSet::Apply`], the view update in
//! [`NoesisSet::Drive`], or a bridge write. They queue the event, and a
//! `PreUpdate` system emits the message and trigger on the next frame. There is
//! no dedupe: every fire is delivered.
//!
//! The subscription set follows [`NoesisEventWatch::entries`] every frame. A
//! name that isn't found is skipped with a warning, then retried (and warned
//! about again) every frame until it resolves.
//! Removing the component drops all of that entity's subscriptions.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
pub use noesis_runtime::events::{EventArgs, RoutedEvent};
pub use noesis_runtime::view::{Key, MouseButton};

use crate::render::{NoesisRenderState, NoesisSet, ReapOnRemove, add_bridge_reap};

/// Owned copy of a routed event's arguments, taken inside the handler while the
/// Noesis args are still alive. A field is `None` when the event doesn't carry
/// it: a `MouseEnter` has a `position` but no `key`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RoutedEventSnapshot {
    /// Pointer position in view coordinates, not relative to the element
    /// (mouse, mouse-button and wheel events).
    pub position: Option<(f32, f32)>,
    /// Changed mouse button (mouse-button events).
    pub mouse_button: Option<MouseButton>,
    /// Wheel rotation delta, ~120 per notch (wheel events).
    pub wheel_delta: Option<i32>,
    /// Pressed or released key (key events).
    pub key: Option<Key>,
    /// Input character / code point (text-input events).
    pub text_char: Option<char>,
    /// New size in DIPs (`SizeChanged`).
    pub new_size: Option<(f32, f32)>,
}

impl RoutedEventSnapshot {
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

/// Emitted when a watched element raises its subscribed [`RoutedEvent`].
#[derive(Message, Debug, Clone)]
pub struct NoesisRoutedEvent {
    /// The [`NoesisView`](crate::NoesisView) entity whose element raised the event.
    pub view: Entity,
    /// `x:Name` of the element the handler was attached to.
    pub name: String,
    /// Which routed event fired.
    pub event: RoutedEvent,
    /// The event args. All `None` for events that carry none of the captured
    /// fields.
    pub args: RoutedEventSnapshot,
}

/// Observer form of [`NoesisRoutedEvent`], triggered on the watch entry's
/// [`target`](EventWatchEntry::target). Without a target it goes to the entity
/// holding the [`NoesisEventWatch`]: the view, or the panel.
#[derive(EntityEvent, Debug, Clone)]
pub struct UiRoutedEvent {
    /// Trigger target: the entry's `target`, else the watching view or panel.
    pub entity: Entity,
    /// The [`NoesisView`](crate::NoesisView) entity the event originated in.
    pub view: Entity,
    /// `x:Name` of the element the handler was attached to.
    pub name: String,
    /// Which routed event fired.
    pub event: RoutedEvent,
    /// The event args.
    pub args: RoutedEventSnapshot,
}

/// One subscription in [`NoesisEventWatch`]: an element `x:Name` (may be
/// scope-qualified, `"Host/Leaf"`), the [`RoutedEvent`] to watch, and routing
/// options. [`new`](Self::new) observes without consuming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EventWatchEntry {
    /// `x:Name` of the element to attach the handler to.
    pub name: String,
    /// Routed event to subscribe on that element.
    pub event: RoutedEvent,
    /// Mark the event handled when it fires, stopping it from bubbling or
    /// tunneling past this element (swallow a `PreviewKeyDown` so it never
    /// reaches a `TextBox`).
    pub mark_handled: bool,
    /// Run even when an earlier handler already marked the event handled.
    pub handled_too: bool,
    /// Entity the [`UiRoutedEvent`] is triggered on. `None` means the entity
    /// holding the [`NoesisEventWatch`].
    pub target: Option<Entity>,
}

impl EventWatchEntry {
    /// Watch `event` on the element named `name`, observing without consuming.
    pub fn new(name: impl Into<String>, event: RoutedEvent) -> Self {
        Self {
            name: name.into(),
            event,
            mark_handled: false,
            handled_too: false,
            target: None,
        }
    }

    /// Builder: mark the event handled when it fires (stops further routing).
    #[must_use]
    pub fn mark_handled(mut self) -> Self {
        self.mark_handled = true;
        self
    }

    /// Builder: also run when an earlier handler already marked the event
    /// handled.
    #[must_use]
    pub fn handled_too(mut self) -> Self {
        self.handled_too = true;
        self
    }

    /// Builder: trigger the [`UiRoutedEvent`] on `target` instead of the
    /// watching entity.
    #[must_use]
    pub fn target(mut self, target: Entity) -> Self {
        self.target = Some(target);
        self
    }
}

/// Routed events to watch on a [`NoesisView`](crate::NoesisView) or
/// [`UiPanel`](crate::panel::UiPanel) entity. Synced every frame: adding an
/// entry subscribes, removing one unsubscribes, and changing an entry's flags or
/// target re-subscribes it.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisEventWatch {
    /// Subscriptions to keep live.
    pub entries: Vec<EventWatchEntry>,
}

impl NoesisEventWatch {
    /// Build a watch from a list of [`EventWatchEntry`] values.
    pub fn new(entries: impl IntoIterator<Item = EventWatchEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

/// Events queued by routed-event handlers until [`drain_routed_event_queue`]
/// delivers them. `Clone` shares the queue.
#[derive(Resource, Clone, Default)]
pub struct SharedRoutedEventQueue(
    pub(crate) Arc<Mutex<Vec<(Entity, Entity, String, RoutedEvent, RoutedEventSnapshot)>>>,
);

impl SharedRoutedEventQueue {
    pub(crate) fn push(
        &self,
        view: Entity,
        target: Entity,
        name: String,
        event: RoutedEvent,
        args: RoutedEventSnapshot,
    ) {
        self.0
            .lock()
            .expect("SharedRoutedEventQueue poisoned")
            .push((view, target, name, event, args));
    }

    fn drain(&self) -> Vec<(Entity, Entity, String, RoutedEvent, RoutedEventSnapshot)> {
        let mut guard = self.0.lock().expect("SharedRoutedEventQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Delivers queued events: one [`NoesisRoutedEvent`] message and one
/// [`UiRoutedEvent`] trigger per fire. Runs in `PreUpdate`.
#[allow(clippy::needless_pass_by_value)]
pub fn drain_routed_event_queue(
    queue: Res<SharedRoutedEventQueue>,
    mut messages: MessageWriter<NoesisRoutedEvent>,
    mut commands: Commands,
) {
    for (view, target, name, event, args) in queue.drain() {
        messages.write(NoesisRoutedEvent {
            view,
            name: name.clone(),
            event,
            args: args.clone(),
        });
        commands.trigger(UiRoutedEvent {
            entity: target,
            view,
            name,
            event,
            args,
        });
    }
}

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

impl ReapOnRemove for NoesisEventWatch {
    fn reap(state: &mut NoesisRenderState, entity: Entity) {
        state.reap_event_watch_for(entity);
    }
}

/// Registers the routed-event bridge. Added by [`crate::NoesisPlugin`].
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
        add_bridge_reap::<NoesisEventWatch>(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_routed_queue_drain_takes_all_and_resets() {
        let q = SharedRoutedEventQueue::default();
        let v = Entity::PLACEHOLDER;
        let t = Entity::PLACEHOLDER;
        q.push(
            v,
            t,
            "Alpha".into(),
            RoutedEvent::MouseDown,
            RoutedEventSnapshot::default(),
        );
        q.push(
            v,
            t,
            "Beta".into(),
            RoutedEvent::MouseUp,
            RoutedEventSnapshot::default(),
        );
        let drained = q.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(drained[0].2, "Alpha");
        assert_eq!(drained[0].3, RoutedEvent::MouseDown);
        assert_eq!(drained[1].3, RoutedEvent::MouseUp);
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

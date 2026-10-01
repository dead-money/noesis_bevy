//! Button clicks and key presses from named elements, delivered as Bevy
//! messages and as observer events.
//!
//! Add a [`NoesisClickWatch`] or [`NoesisKeyDownWatch`] listing `x:Name`s to a
//! [`NoesisView`](crate::NoesisView) camera entity or a [`UiPanel`](crate::UiPanel)
//! entity. The bridge keeps one subscription per listed name, adding and
//! removing them as the list changes. Each `Click` (from any `BaseButton`) or
//! `KeyDown` arrives twice:
//!
//! * as a [`NoesisClicked`] / [`NoesisKeyDown`] message carrying the view entity,
//!   and
//! * as a [`UiClicked`] / [`UiKeyDown`] [`EntityEvent`] targeting the watch
//!   entry's `target`: the view entity by default, or the panel entity for a
//!   watch on a panel.
//!
//! ```ignore
//! commands.entity(view).insert((
//!     NoesisClickWatch::new(["NewGameButton", "QuitButton"]),
//!     NoesisKeyDownWatch::new([KeyDownWatchEntry::new("CommandInput").swallow(Key::Return)]),
//! ));
//!
//! fn on_click(mut clicks: MessageReader<NoesisClicked>) {
//!     for ev in clicks.read() { /* ev.view, ev.name */ }
//! }
//!
//! // On a panel watch, the event target is the panel entity.
//! fn observe_click(on: On<UiClicked>, panels: Query<&Health>) {
//!     if let Ok(hp) = panels.get(on.event_target()) { /* ... */ }
//! }
//! ```
//!
//! Noesis raises the events during `PostUpdate`; they are delivered in the next
//! frame's `PreUpdate`, outside any Noesis borrow, so observers may freely touch
//! the `World`. Because of that one-frame delay, a global observer can receive
//! an event whose target was despawned in between: look the target up with
//! `Query::get` rather than assuming it exists.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
pub use noesis_runtime::view::Key;

use crate::render::{NoesisRenderState, NoesisSet, ReapOnRemove, add_bridge_reap};

/// Sent when an element listed in a [`NoesisClickWatch`] raises `Click`.
#[derive(Message, Debug, Clone)]
pub struct NoesisClicked {
    /// The [`NoesisView`](crate::NoesisView) entity the click came from (the
    /// host view, for a watch on a panel).
    pub view: Entity,
    /// `x:Name` of the element that raised the click.
    pub name: String,
}

/// Observer form of [`NoesisClicked`]. Its target is the watch entry's
/// `target` entity: the view by default, the panel for a panel watch, or the row
/// entity for a templated list row. Read it with `On::event_target`.
///
/// Delivered one frame after the click, so the target may have been despawned
/// since. Observers on that entity are gone with it, but global observers still
/// run.
#[derive(EntityEvent, Debug, Clone)]
pub struct UiClicked {
    /// Event target: the view, panel, or list-row entity.
    pub entity: Entity,
    /// The [`NoesisView`](crate::NoesisView) entity the click originated in.
    pub view: Entity,
    /// `x:Name` of the clicked element, or the list control's `x:Name` for a
    /// templated row click (rows carry no name of their own).
    pub name: String,
}

/// One entry in [`NoesisClickWatch`]: an element `x:Name` and the entity its
/// [`UiClicked`] targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClickWatchEntry {
    /// `x:Name` of a `BaseButton` (`Button`, `CheckBox`, ...). Other element
    /// types are skipped with a warning.
    pub name: String,
    /// Entity [`UiClicked`] targets. `None` means the entity carrying the watch.
    pub target: Option<Entity>,
}

impl ClickWatchEntry {
    /// Watch `Click` on `name`, targeting the entity carrying the watch.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            target: None,
        }
    }

    /// Builder: send [`UiClicked`] to `target` instead.
    #[must_use]
    pub fn target(mut self, target: Entity) -> Self {
        self.target = Some(target);
        self
    }
}

/// The elements whose `Click` to report. Add it to a
/// [`NoesisView`](crate::NoesisView) or [`UiPanel`](crate::UiPanel) entity; see
/// the [module docs](self). Synced every frame: adding an entry subscribes,
/// removing one unsubscribes.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisClickWatch {
    /// One entry per watched element.
    pub entries: Vec<ClickWatchEntry>,
}

impl NoesisClickWatch {
    /// Watch the given `x:Name`s with default targets.
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            entries: names.into_iter().map(ClickWatchEntry::new).collect(),
        }
    }

    /// Watch explicit entries, for per-entry [`UiClicked`] targets.
    pub fn from_entries(entries: impl IntoIterator<Item = ClickWatchEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }

    /// Add one `x:Name` with the default target. Push a [`ClickWatchEntry`] onto
    /// `entries` for a custom target.
    pub fn watch(&mut self, name: impl Into<String>) -> &mut Self {
        self.entries.push(ClickWatchEntry::new(name));
        self
    }

    /// Add several `x:Name`s with the default target.
    pub fn extend_names(
        &mut self,
        names: impl IntoIterator<Item = impl Into<String>>,
    ) -> &mut Self {
        self.entries
            .extend(names.into_iter().map(ClickWatchEntry::new));
        self
    }
}

/// Clicks waiting for [`drain_click_queue`], as `(view, target, name)`. Clones
/// share one queue.
#[derive(Resource, Clone, Default)]
pub struct SharedClickQueue(pub(crate) Arc<Mutex<Vec<(Entity, Entity, String)>>>);

impl SharedClickQueue {
    pub(crate) fn push(&self, view: Entity, target: Entity, name: String) {
        self.0
            .lock()
            .expect("SharedClickQueue poisoned")
            .push((view, target, name));
    }

    fn drain(&self) -> Vec<(Entity, Entity, String)> {
        let mut guard = self.0.lock().expect("SharedClickQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Send a [`NoesisClicked`] and trigger a [`UiClicked`] for each queued click.
/// Runs in `PreUpdate`, outside any Noesis borrow, so observers may touch the
/// `World`.
#[allow(clippy::needless_pass_by_value)]
pub fn drain_click_queue(
    queue: Res<SharedClickQueue>,
    mut messages: MessageWriter<NoesisClicked>,
    mut commands: Commands,
) {
    for (view, target, name) in queue.drain() {
        messages.write(NoesisClicked {
            view,
            name: name.clone(),
        });
        commands.trigger(UiClicked {
            entity: target,
            view,
            name,
        });
    }
}

/// Sync each entity's `Click` subscriptions to its [`NoesisClickWatch`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_click_subscriptions(
    views: Query<(Entity, &NoesisClickWatch)>,
    queue: Res<SharedClickQueue>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, watch) in &views {
        state.sync_click_subscriptions_for(entity, &watch.entries, &queue);
    }
}

/// Sent when an element listed in a [`NoesisKeyDownWatch`] raises `KeyDown`.
#[derive(Message, Debug, Clone)]
pub struct NoesisKeyDown {
    /// The [`NoesisView`](crate::NoesisView) entity the key press came from (the
    /// host view, for a watch on a panel).
    pub view: Entity,
    /// `x:Name` of the element.
    pub name: String,
    /// The pressed key; keys with no [`Key`] variant arrive as [`Key::None`].
    pub key: Key,
}

/// Observer form of [`NoesisKeyDown`]. Its target is the watch entry's
/// `target` entity (by default the entity carrying the watch). Like
/// [`UiClicked`], it arrives a frame late, so the target may be gone.
#[derive(EntityEvent, Debug, Clone)]
pub struct UiKeyDown {
    /// Event target.
    pub entity: Entity,
    /// The [`NoesisView`](crate::NoesisView) entity the keydown originated in.
    pub view: Entity,
    /// `x:Name` of the element that received the keydown.
    pub name: String,
    /// The pressed key.
    pub key: Key,
}

/// One entry in [`NoesisKeyDownWatch`]: an element `x:Name`, the keys to
/// swallow, and the entity its [`UiKeyDown`] targets.
#[derive(Clone, Debug)]
pub struct KeyDownWatchEntry {
    /// `x:Name` of the element to watch for `UIElement::KeyDown`.
    pub name: String,
    /// Keys marked handled so they stop routing, e.g. `Return` so a submit
    /// doesn't also insert a newline. Swallowed keys are still reported. Empty
    /// by default.
    pub swallow: Vec<Key>,
    /// Entity [`UiKeyDown`] targets. `None` means the entity carrying the watch.
    pub target: Option<Entity>,
}

impl KeyDownWatchEntry {
    /// Watch `name`, swallowing nothing, with the default target.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            swallow: Vec::new(),
            target: None,
        }
    }

    /// Builder: also swallow `key`.
    #[must_use]
    pub fn swallow(mut self, key: Key) -> Self {
        self.swallow.push(key);
        self
    }

    /// Builder: also swallow every key in `keys`.
    #[must_use]
    pub fn swallow_all<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Key>,
    {
        self.swallow.extend(keys);
        self
    }

    /// Builder: send [`UiKeyDown`] to `target` instead.
    #[must_use]
    pub fn target(mut self, target: Entity) -> Self {
        self.target = Some(target);
        self
    }
}

/// The elements whose `KeyDown` to report. Add it to a
/// [`NoesisView`](crate::NoesisView) or [`UiPanel`](crate::UiPanel) entity; see
/// the [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisKeyDownWatch {
    /// One entry per watched element.
    pub entries: Vec<KeyDownWatchEntry>,
}

impl NoesisKeyDownWatch {
    /// Watch the given entries.
    pub fn new(entries: impl IntoIterator<Item = KeyDownWatchEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

/// Key presses waiting for [`drain_keydown_queue`], as
/// `(view, target, name, key)`. Clones share one queue.
#[derive(Resource, Clone, Default)]
pub struct SharedKeyDownQueue(pub(crate) Arc<Mutex<Vec<(Entity, Entity, String, Key)>>>);

impl SharedKeyDownQueue {
    pub(crate) fn push(&self, view: Entity, target: Entity, name: String, key: Key) {
        self.0
            .lock()
            .expect("SharedKeyDownQueue poisoned")
            .push((view, target, name, key));
    }

    fn drain(&self) -> Vec<(Entity, Entity, String, Key)> {
        let mut guard = self.0.lock().expect("SharedKeyDownQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Send a [`NoesisKeyDown`] and trigger a [`UiKeyDown`] for each queued key
/// press. Runs in `PreUpdate`.
#[allow(clippy::needless_pass_by_value)]
pub fn drain_keydown_queue(
    queue: Res<SharedKeyDownQueue>,
    mut messages: MessageWriter<NoesisKeyDown>,
    mut commands: Commands,
) {
    for (view, target, name, key) in queue.drain() {
        messages.write(NoesisKeyDown {
            view,
            name: name.clone(),
            key,
        });
        commands.trigger(UiKeyDown {
            entity: target,
            view,
            name,
            key,
        });
    }
}

/// Sync each entity's `KeyDown` subscriptions to its [`NoesisKeyDownWatch`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_keydown_subscriptions(
    views: Query<(Entity, &NoesisKeyDownWatch)>,
    queue: Res<SharedKeyDownQueue>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, watch) in &views {
        state.sync_keydown_subscriptions_for(entity, &watch.entries, &queue);
    }
}

impl ReapOnRemove for NoesisClickWatch {
    fn reap(state: &mut NoesisRenderState, entity: Entity) {
        state.reap_click_watch_for(entity);
    }
}

impl ReapOnRemove for NoesisKeyDownWatch {
    fn reap(state: &mut NoesisRenderState, entity: Entity) {
        state.reap_keydown_watch_for(entity);
    }
}

/// Registers the click and key-down watches, their queues and messages. Added
/// by [`crate::NoesisPlugin`].
pub struct NoesisEventsPlugin;

impl Plugin for NoesisEventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisClicked>()
            .add_message::<NoesisKeyDown>()
            .insert_resource(SharedClickQueue::default())
            .insert_resource(SharedKeyDownQueue::default())
            .add_systems(PreUpdate, (drain_click_queue, drain_keydown_queue))
            .add_systems(
                PostUpdate,
                (sync_click_subscriptions, sync_keydown_subscriptions).in_set(NoesisSet::Apply),
            );
        add_bridge_reap::<NoesisClickWatch>(app);
        add_bridge_reap::<NoesisKeyDownWatch>(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_click_queue_drain_takes_all_and_resets() {
        let q = SharedClickQueue::default();
        let v = Entity::PLACEHOLDER;
        let t = Entity::PLACEHOLDER;
        q.push(v, t, "Alpha".into());
        q.push(v, t, "Beta".into());
        let drained = q.drain();
        assert_eq!(
            drained,
            vec![(v, t, "Alpha".to_string()), (v, t, "Beta".to_string())]
        );
        assert!(q.drain().is_empty());
    }

    #[test]
    fn click_watch_constructor_normalizes_into_entries() {
        let w = NoesisClickWatch::new(["a", "b", "c"]);
        let names: Vec<&str> = w.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b", "c"]);
        assert!(w.entries.iter().all(|e| e.target.is_none()));
    }

    #[test]
    fn click_watch_entry_target_builder() {
        let e = ClickWatchEntry::new("Row").target(Entity::PLACEHOLDER);
        assert_eq!(e.name, "Row");
        assert_eq!(e.target, Some(Entity::PLACEHOLDER));
    }

    #[test]
    fn keydown_entry_swallow_builder() {
        let e = KeyDownWatchEntry::new("Input").swallow(Key::Return);
        assert_eq!(e.name, "Input");
        assert_eq!(e.swallow, vec![Key::Return]);
        assert_eq!(e.target, None);
    }
}

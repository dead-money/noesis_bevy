//! Per-view routed-event bridge: surface `BaseButton::Click` and
//! `UIElement::KeyDown` from named elements of a single [`crate::NoesisView`] as
//! both Bevy messages **and** Bevy `EntityEvent`s (observers).
//!
//! Add a [`NoesisClickWatch`] / [`NoesisKeyDownWatch`] component to the view's
//! camera entity listing the `x:Name`s to observe. The reconcile systems keep
//! each view's live subscription set in sync. A fired event surfaces two ways:
//!
//! * as a [`NoesisClicked`] / [`NoesisKeyDown`] **message** carrying the
//!   originating `view` entity (the original, pull-based API), and
//! * as a [`UiClicked`] / [`UiKeyDown`] **`EntityEvent`** targeting the watch
//!   entry's `target` entity (defaulting to the `view` entity), so an observer
//!   recovers the clicked entity via [`On::event_target`].
//!
//! ```ignore
//! commands.entity(view).insert((
//!     NoesisClickWatch::new(["NewGameButton", "QuitButton"]),
//!     NoesisKeyDownWatch::new([KeyDownWatchEntry::new("CommandInput").swallow(Key::Return)]),
//! ));
//!
//! // Pull-based (messages):
//! fn on_click(mut clicks: MessageReader<NoesisClicked>) {
//!     for ev in clicks.read() { /* ev.view: Entity, ev.name: String */ }
//! }
//!
//! // Push-based (observer): the trigger target IS the panel entity.
//! fn observe_click(on: On<UiClicked>, panels: Query<&Health>) {
//!     if let Ok(hp) = panels.get(on.event_target()) { /* … */ }
//! }
//! ```
//!
//! Click/keydown callbacks fire on the main thread (during the view's
//! `View::update`); they push `(view, target, name[, key])` onto a small queue
//! that the `PreUpdate` drain turns into messages + triggered events the next
//! frame. The drain holds no Noesis borrow, so firing observers there is safe.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
pub use noesis_runtime::view::Key;

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Click bridge
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when a watched element raises `BaseButton::Click`.
#[derive(Message, Debug, Clone)]
pub struct NoesisClicked {
    /// The [`NoesisView`](crate::NoesisView) entity whose element was clicked.
    pub view: Entity,
    /// `x:Name` of the element that raised the click.
    pub name: String,
}

/// Observer-facing twin of [`NoesisClicked`]: a click surfaced as an
/// `EntityEvent` whose target is the watch entry's `target` entity (the `view`
/// entity by default, or a per-row entity for templated list rows). Read the
/// target with [`On::event_target`].
///
/// Fired via the global self-targeting `commands.trigger`, so a stale/despawned
/// target is safe: no entity-targeted observer exists for it.
#[derive(EntityEvent, Debug, Clone)]
pub struct UiClicked {
    /// Trigger target: the panel/view entity (named elements) or the row entity
    /// (templated list rows).
    pub entity: Entity,
    /// The [`NoesisView`](crate::NoesisView) entity the click originated in.
    pub view: Entity,
    /// `x:Name` of the clicked element, or the list control's `x:Name` for a
    /// templated row click (rows carry no name of their own).
    pub name: String,
}

/// One entry in [`NoesisClickWatch`]: an element `x:Name` plus the entity the
/// resulting [`UiClicked`] should target. `target` defaults to the view entity
/// (set it to redirect the observer at a different entity).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClickWatchEntry {
    /// `x:Name` of the element to subscribe a `Click` handler against.
    pub name: String,
    /// Entity the fired [`UiClicked`] targets; `None` → the view entity.
    pub target: Option<Entity>,
}

impl ClickWatchEntry {
    /// Watch `Click` on the element named `name`, targeting the view entity.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            target: None,
        }
    }

    /// Builder: target the fired [`UiClicked`] at `target` instead of the view.
    #[must_use]
    pub fn target(mut self, target: Entity) -> Self {
        self.target = Some(target);
        self
    }
}

/// Per-view component: elements to subscribe a `Click` handler against. Add to a
/// [`NoesisView`](crate::NoesisView) entity. Entries are diff-synced each frame:
/// adding installs a subscription, removing tears it down.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisClickWatch {
    /// Per-element watch entries (`x:Name` + optional [`UiClicked`] target).
    pub entries: Vec<ClickWatchEntry>,
}

impl NoesisClickWatch {
    /// Builds a watch over the given element `x:Name`s, each [`UiClicked`]
    /// targeting the view entity.
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            entries: names.into_iter().map(ClickWatchEntry::new).collect(),
        }
    }

    /// Builds a watch from explicit [`ClickWatchEntry`] values (to set per-entry
    /// [`UiClicked`] targets).
    pub fn from_entries(entries: impl IntoIterator<Item = ClickWatchEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

/// Queue between the (main-thread) click callbacks and the drain system.
/// `Clone` is an `Arc` clone. Entries carry `(view, target, name)`.
#[derive(Resource, Clone, Default)]
pub struct SharedClickQueue(pub(crate) Arc<Mutex<Vec<(Entity, Entity, String)>>>);

impl SharedClickQueue {
    /// Push `(view, target, name)` from a click callback.
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

/// Drain the click queue: write a [`NoesisClicked`] message **and** trigger a
/// [`UiClicked`] `EntityEvent` (one of each per click). Runs in `PreUpdate` with
/// no Noesis borrow held, so triggering observers here is safe.
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

/// Reconcile every view's [`NoesisClickWatch`] against its live subscription set.
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

// ─────────────────────────────────────────────────────────────────────────────
// KeyDown bridge
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when a watched element raises `UIElement::KeyDown`.
#[derive(Message, Debug, Clone)]
pub struct NoesisKeyDown {
    /// The [`NoesisView`](crate::NoesisView) entity whose element received the keydown.
    pub view: Entity,
    /// `x:Name` of the element.
    pub name: String,
    /// Pressed key, mapped to the safe [`Key`] mirror (unmapped ordinals → [`Key::None`]).
    pub key: Key,
}

/// Observer-facing twin of [`NoesisKeyDown`]: a keydown surfaced as an
/// `EntityEvent` whose target is the watch entry's `target` entity (the `view`
/// entity by default). Read the target with [`On::event_target`].
#[derive(EntityEvent, Debug, Clone)]
pub struct UiKeyDown {
    /// Trigger target: the watch entry's `target` (the view entity by default).
    pub entity: Entity,
    /// The [`NoesisView`](crate::NoesisView) entity the keydown originated in.
    pub view: Entity,
    /// `x:Name` of the element that received the keydown.
    pub name: String,
    /// Pressed key, mapped to the safe [`Key`] mirror.
    pub key: Key,
}

/// One entry in [`NoesisKeyDownWatch`]: an element `x:Name`, the per-name swallow
/// set, and the entity the resulting [`UiKeyDown`] should target. Keys in
/// `swallow` are marked handled by the C++ trampoline, stopping further routing
/// (e.g. swallow `Return` so a submit doesn't append a newline). Empty by
/// default: every key propagates, none are swallowed. `target` defaults to the
/// view entity.
#[derive(Clone, Debug)]
pub struct KeyDownWatchEntry {
    /// `x:Name` of the element to watch for `UIElement::KeyDown`.
    pub name: String,
    /// Keys marked handled by the C++ trampoline, stopping further routing.
    pub swallow: Vec<Key>,
    /// Entity the fired [`UiKeyDown`] targets; `None` → the view entity.
    pub target: Option<Entity>,
}

impl KeyDownWatchEntry {
    /// Builds an entry watching `name`, with an empty swallow set, targeting the
    /// view entity.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            swallow: Vec::new(),
            target: None,
        }
    }

    /// Builder: append `key` to the swallow set.
    #[must_use]
    pub fn swallow(mut self, key: Key) -> Self {
        self.swallow.push(key);
        self
    }

    /// Builder: append every key in `keys` to the swallow set.
    #[must_use]
    pub fn swallow_all<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Key>,
    {
        self.swallow.extend(keys);
        self
    }

    /// Builder: target the fired [`UiKeyDown`] at `target` instead of the view.
    #[must_use]
    pub fn target(mut self, target: Entity) -> Self {
        self.target = Some(target);
        self
    }
}

/// Per-view component: `x:Name`s + per-name swallow sets to watch for
/// `UIElement::KeyDown`. Add to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisKeyDownWatch {
    /// Per-element watch entries, each pairing an `x:Name` with its swallow set.
    pub entries: Vec<KeyDownWatchEntry>,
}

impl NoesisKeyDownWatch {
    /// Builds a watch from the given [`KeyDownWatchEntry`] list.
    pub fn new(entries: impl IntoIterator<Item = KeyDownWatchEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

/// Queue between the (main-thread) keydown callbacks and the drain system.
/// Entries carry `(view, target, name, key)`.
#[derive(Resource, Clone, Default)]
pub struct SharedKeyDownQueue(pub(crate) Arc<Mutex<Vec<(Entity, Entity, String, Key)>>>);

impl SharedKeyDownQueue {
    /// Push `(view, target, name, key)` from a keydown callback.
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

/// Drain the keydown queue: write a [`NoesisKeyDown`] message **and** trigger a
/// [`UiKeyDown`] `EntityEvent` (one of each per keydown).
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

/// Reconcile every view's [`NoesisKeyDownWatch`] against its live subscription set.
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

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the per-view click + keydown bridges. Added transitively by
/// [`crate::NoesisPlugin`].
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

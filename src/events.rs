//! Per-view routed-event bridge — surface `BaseButton::Click` and
//! `UIElement::KeyDown` from named elements of a single [`NoesisView`] as Bevy
//! messages.
//!
//! Add a [`NoesisClickWatch`] / [`NoesisKeyDownWatch`] component to the view's
//! camera entity listing the `x:Name`s to observe. The reconcile systems keep
//! each view's live subscription set in sync; a fired event surfaces as a
//! [`NoesisClicked`] / [`NoesisKeyDown`] message carrying the originating
//! `view` entity.
//!
//! ```ignore
//! commands.entity(view).insert((
//!     NoesisClickWatch::new(["NewGameButton", "QuitButton"]),
//!     NoesisKeyDownWatch::new([KeyDownWatchEntry::new("CommandInput").swallow(Key::Return)]),
//! ));
//!
//! fn on_click(mut clicks: MessageReader<NoesisClicked>) {
//!     for ev in clicks.read() { /* ev.view: Entity, ev.name: String */ }
//! }
//! ```
//!
//! Click/keydown callbacks fire on the main thread (during the view's
//! `View::update`); they push `(view, name[, key])` onto a small queue that the
//! `PreUpdate` drain turns into messages the next frame.

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

/// Per-view component: element `x:Name`s to subscribe a `Click` handler against.
/// Add to a [`NoesisView`](crate::NoesisView) entity. Names are diff-synced each
/// frame — adding installs a subscription, removing tears it down.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisClickWatch {
    pub names: Vec<String>,
}

impl NoesisClickWatch {
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            names: names.into_iter().map(Into::into).collect(),
        }
    }
}

/// Queue between the (main-thread) click callbacks and the drain system.
/// `Clone` is an `Arc` clone. Entries carry the originating view entity.
#[derive(Resource, Clone, Default)]
pub struct SharedClickQueue(pub(crate) Arc<Mutex<Vec<(Entity, String)>>>);

impl SharedClickQueue {
    /// Push `(view, name)` from a click callback.
    pub(crate) fn push(&self, view: Entity, name: String) {
        self.0
            .lock()
            .expect("SharedClickQueue poisoned")
            .push((view, name));
    }

    fn drain(&self) -> Vec<(Entity, String)> {
        let mut guard = self.0.lock().expect("SharedClickQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Drain the click queue into [`NoesisClicked`] messages (one per click).
#[allow(clippy::needless_pass_by_value)]
pub fn drain_click_queue(queue: Res<SharedClickQueue>, mut messages: MessageWriter<NoesisClicked>) {
    for (view, name) in queue.drain() {
        messages.write(NoesisClicked { view, name });
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
        state.sync_click_subscriptions_for(entity, &watch.names, &queue);
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

/// One entry in [`NoesisKeyDownWatch`]: an element `x:Name` plus the per-name
/// swallow set. Keys in `swallow` are marked handled by the C++ trampoline,
/// stopping further routing (e.g. swallow `Return` so a submit doesn't append a
/// newline). Empty by default — every key propagates, none are swallowed.
#[derive(Clone, Debug)]
pub struct KeyDownWatchEntry {
    pub name: String,
    pub swallow: Vec<Key>,
}

impl KeyDownWatchEntry {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            swallow: Vec::new(),
        }
    }

    /// Builder — append `key` to the swallow set.
    #[must_use]
    pub fn swallow(mut self, key: Key) -> Self {
        self.swallow.push(key);
        self
    }

    /// Builder — append every key in `keys` to the swallow set.
    #[must_use]
    pub fn swallow_all<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Key>,
    {
        self.swallow.extend(keys);
        self
    }
}

/// Per-view component: `x:Name`s + per-name swallow sets to watch for
/// `UIElement::KeyDown`. Add to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisKeyDownWatch {
    pub entries: Vec<KeyDownWatchEntry>,
}

impl NoesisKeyDownWatch {
    pub fn new(entries: impl IntoIterator<Item = KeyDownWatchEntry>) -> Self {
        Self {
            entries: entries.into_iter().collect(),
        }
    }
}

/// Queue between the (main-thread) keydown callbacks and the drain system.
#[derive(Resource, Clone, Default)]
pub struct SharedKeyDownQueue(pub(crate) Arc<Mutex<Vec<(Entity, String, Key)>>>);

impl SharedKeyDownQueue {
    /// Push `(view, name, key)` from a keydown callback.
    pub(crate) fn push(&self, view: Entity, name: String, key: Key) {
        self.0
            .lock()
            .expect("SharedKeyDownQueue poisoned")
            .push((view, name, key));
    }

    fn drain(&self) -> Vec<(Entity, String, Key)> {
        let mut guard = self.0.lock().expect("SharedKeyDownQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Drain the keydown queue into [`NoesisKeyDown`] messages.
#[allow(clippy::needless_pass_by_value)]
pub fn drain_keydown_queue(
    queue: Res<SharedKeyDownQueue>,
    mut messages: MessageWriter<NoesisKeyDown>,
) {
    for (view, name, key) in queue.drain() {
        messages.write(NoesisKeyDown { view, name, key });
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
        q.push(v, "Alpha".into());
        q.push(v, "Beta".into());
        let drained = q.drain();
        assert_eq!(
            drained,
            vec![(v, "Alpha".to_string()), (v, "Beta".to_string())]
        );
        assert!(q.drain().is_empty());
    }

    #[test]
    fn click_watch_constructor_normalizes_into_strings() {
        let w = NoesisClickWatch::new(["a", "b", "c"]);
        assert_eq!(
            w.names,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }

    #[test]
    fn keydown_entry_swallow_builder() {
        let e = KeyDownWatchEntry::new("Input").swallow(Key::Return);
        assert_eq!(e.name, "Input");
        assert_eq!(e.swallow, vec![Key::Return]);
    }
}

//! Bridge Noesis routed events into Bevy events (Phase 5.B).
//!
//! Exposes [`NoesisClicked`] for `BaseButton::Click` and [`NoesisKeyDown`]
//! for `UIElement::KeyDown`. The pattern generalizes — any routed event
//! with the standard `Delegate<void(BaseComponent*, const RoutedEventArgs&)>`
//! signature can fit through the same plumbing once `dm_noesis_runtime`
//! exposes a subscriber for it.
//!
//! # Wiring shape
//!
//! ```text
//!   main world                                 render world
//!   ─────────────────────────                  ─────────────────────────
//!   NoesisClickWatch.names                     scene.click_subs:
//!   (Vec<String>)        ── extract ─────►     HashMap<String, ClickSubscription>
//!                                              │
//!                                              │  on click:
//!                                              │  queue.lock().push(name)
//!                                              ▼
//!   NoesisClicked event ◄── drain ────────────  SharedClickQueue
//!                                              (Arc<Mutex<Vec<String>>>)
//! ```
//!
//! The shared queue is a `Resource` registered on both apps; the same
//! `Arc` lives in both, so writes from the render side appear immediately
//! on the main side. Bevy's `ExtractResourcePlugin` clones the `Arc`
//! every frame, which keeps the two views aliased — no data duplication.
//!
//! # Adding a watched element
//!
//! ```ignore
//! commands.insert_resource(NoesisClickWatch::new(["NewGameButton", "QuitButton"]));
//! commands.add_observer(|trigger: Trigger<NoesisClicked>| {
//!     match trigger.event().name.as_str() {
//!         "NewGameButton" => /* ... */ {}
//!         "QuitButton"    => /* ... */ {}
//!         _ => {}
//!     }
//! });
//! ```
//!
//! Reactive sync — names added to / removed from `NoesisClickWatch` after
//! the scene is up are picked up on the next frame.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};
pub use dm_noesis_runtime::view::Key;

use crate::render::NoesisRenderState;

/// Bevy message written in `PreUpdate` when a watched named element raises
/// `BaseButton::Click`. Use [`NoesisClickWatch`] to declare which `x:Name`
/// values to observe.
///
/// Note: Bevy 0.18 split the old `Event` concept — buffered, queue-style
/// notifications are `Message` now (this), while `Event` is reserved for
/// observer-style triggers. Read with `MessageReader<NoesisClicked>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisClicked {
    /// `x:Name` of the element that raised the click.
    pub name: String,
}

/// Element `x:Name`s to subscribe a Click handler against. Insert as a
/// `Resource` on the main app; the render world receives a copy via
/// [`ExtractResource`] each frame.
///
/// Names are diff-synced against the live subscription set every render
/// frame: adding a name installs a subscription on the next frame the
/// element is present in the tree; removing it tears the subscription
/// down. Names absent from the current scene log a warning once per
/// scene-build (warnings repeat on URI change because subscriptions
/// don't survive scene teardown).
#[derive(Resource, ExtractResource, Clone, Default, Debug)]
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

/// Shared queue between the render-world Click callbacks and the main-world
/// drain system. `Clone` is an `Arc` clone; both apps see the same `Vec`.
#[derive(Resource, Clone, Default)]
pub struct SharedClickQueue(pub(crate) Arc<Mutex<Vec<String>>>);

impl ExtractResource for SharedClickQueue {
    type Source = SharedClickQueue;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

impl SharedClickQueue {
    /// Push a `name` onto the queue from a click callback. Render-world
    /// only — main-world readers go through the [`NoesisClicked`] event
    /// emitted by [`drain_click_queue`].
    pub(crate) fn push(&self, name: String) {
        self.0.lock().expect("SharedClickQueue poisoned").push(name);
    }

    /// Drain the queue into a fresh `Vec`. Cheap when empty.
    fn drain(&self) -> Vec<String> {
        let mut guard = self.0.lock().expect("SharedClickQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Main-app system: drain the shared queue and write one [`NoesisClicked`]
/// message per name. Runs every frame in `PreUpdate` so Update-stage systems
/// see the messages on the same frame they were captured (the render side
/// pushed during the previous frame's Render schedule, which finishes
/// before this frame's `PreUpdate`).
#[allow(clippy::needless_pass_by_value)]
pub fn drain_click_queue(queue: Res<SharedClickQueue>, mut messages: MessageWriter<NoesisClicked>) {
    for name in queue.drain() {
        messages.write(NoesisClicked { name });
    }
}

/// Render-app system: reconcile [`NoesisClickWatch`] against the live
/// scene's subscription set. Runs after `ensure_noesis_scene` so the View
/// (and its named-element tree) exists.
pub(crate) fn sync_click_subscriptions(
    watch: Option<Res<NoesisClickWatch>>,
    queue: Option<Res<SharedClickQueue>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(watch), Some(queue), Some(mut state)) = (watch, queue, state) else {
        return;
    };
    state.sync_click_subscriptions(&watch.names, &queue);
}

// ─────────────────────────────────────────────────────────────────────────────
// KeyDown bridge
// ─────────────────────────────────────────────────────────────────────────────

/// Bevy message written in `PreUpdate` when a watched named element raises
/// `UIElement::KeyDown`. Mirrors [`NoesisClicked`] but carries the
/// pressed key.
#[derive(Message, Debug, Clone)]
pub struct NoesisKeyDown {
    /// `x:Name` of the element that received the keydown.
    pub name: String,
    /// Pressed key, mapped to the safe [`Key`] mirror. Unmapped raw
    /// ordinals collapse to [`Key::None`] — see
    /// `dm_noesis_runtime::events::subscribe_keydown` for the mapping
    /// table.
    pub key: Key,
}

/// One entry in [`NoesisKeyDownWatch`]. Pairs the element's `x:Name`
/// with the per-name swallow set: keys in `swallow` are marked
/// `KeyEventArgs::handled = true` by the C++-side trampoline, stopping
/// further routing (e.g. swallow `OemTilde` so backtick doesn't get
/// typed into the focused `TextBox`; swallow `Return` so a newline
/// doesn't get appended on submit).
///
/// `swallow` defaults to empty — every key on the element propagates as
/// a [`NoesisKeyDown`] event, none are swallowed. Add the keys your
/// callback actually consumes; leave the rest alone so default WPF
/// behaviours (caret moves, character entry, …) keep working.
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

    /// Builder helper — append `key` to the swallow set.
    #[must_use]
    pub fn swallow(mut self, key: Key) -> Self {
        self.swallow.push(key);
        self
    }

    /// Builder helper — append every key in `keys` to the swallow set.
    #[must_use]
    pub fn swallow_all<I>(mut self, keys: I) -> Self
    where
        I: IntoIterator<Item = Key>,
    {
        self.swallow.extend(keys);
        self
    }
}

/// `x:Name`s + per-name swallow sets to watch for `UIElement::KeyDown`.
/// Insert as a `Resource` on the main app; the render world receives a
/// copy via [`ExtractResource`] each frame and reconciles its live
/// subscription set against the entries.
///
/// Reactive sync — entries added / removed / re-configured between
/// frames are picked up next frame; changing an entry's swallow set
/// re-binds the C++-side delegate (so the closure captures the new
/// list).
#[derive(Resource, ExtractResource, Clone, Default, Debug)]
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

/// Shared queue between the render-world `KeyDown` callbacks and the
/// main-world drain system. Mirrors [`SharedClickQueue`].
#[derive(Resource, Clone, Default)]
pub struct SharedKeyDownQueue(pub(crate) Arc<Mutex<Vec<(String, Key)>>>);

impl ExtractResource for SharedKeyDownQueue {
    type Source = SharedKeyDownQueue;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

impl SharedKeyDownQueue {
    /// Push a `(name, key)` pair from a keydown callback. Render-world
    /// only — main-world readers drain via [`drain_keydown_queue`].
    pub(crate) fn push(&self, name: String, key: Key) {
        self.0
            .lock()
            .expect("SharedKeyDownQueue poisoned")
            .push((name, key));
    }

    /// Drain into a fresh `Vec`. Cheap when empty.
    fn drain(&self) -> Vec<(String, Key)> {
        let mut guard = self.0.lock().expect("SharedKeyDownQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Main-app system: drain the shared queue and write one [`NoesisKeyDown`]
/// per `(name, key)` pair. Mirrors [`drain_click_queue`].
#[allow(clippy::needless_pass_by_value)]
pub fn drain_keydown_queue(
    queue: Res<SharedKeyDownQueue>,
    mut messages: MessageWriter<NoesisKeyDown>,
) {
    for (name, key) in queue.drain() {
        messages.write(NoesisKeyDown { name, key });
    }
}

/// Render-app system: reconcile [`NoesisKeyDownWatch`] against the live
/// scene's subscription set. Mirrors [`sync_click_subscriptions`].
pub(crate) fn sync_keydown_subscriptions(
    watch: Option<Res<NoesisKeyDownWatch>>,
    queue: Option<Res<SharedKeyDownQueue>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(watch), Some(queue), Some(mut state)) = (watch, queue, state) else {
        return;
    };
    state.sync_keydown_subscriptions(&watch.entries, &queue);
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the click-event bridge: extracts [`NoesisClickWatch`] +
/// [`SharedClickQueue`] to the render world, runs the render-side sync
/// after `ensure_noesis_scene`, and drains the queue into [`NoesisClicked`]
/// events on the main app each frame.
pub struct NoesisEventsPlugin;

impl Plugin for NoesisEventsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisClicked>()
            .add_message::<NoesisKeyDown>()
            .init_resource::<NoesisClickWatch>()
            .init_resource::<NoesisKeyDownWatch>()
            .insert_resource(SharedClickQueue::default())
            .insert_resource(SharedKeyDownQueue::default())
            .add_plugins((
                ExtractResourcePlugin::<NoesisClickWatch>::default(),
                ExtractResourcePlugin::<SharedClickQueue>::default(),
                ExtractResourcePlugin::<NoesisKeyDownWatch>::default(),
                ExtractResourcePlugin::<SharedKeyDownQueue>::default(),
            ))
            .add_systems(PreUpdate, (drain_click_queue, drain_keydown_queue));

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            // Mirrors NoesisRenderPlugin's no-op behaviour for headless setups.
            return;
        };

        // The render-side sync system runs in `Prepare` after the scene
        // ensure pass. We schedule it loosely (no explicit `.after`) and
        // rely on it being a no-op when `NoesisRenderState.scene` is None;
        // the order matters only for the first frame the scene appears.
        // `sync_click_subscriptions` is idempotent on empty diffs — running
        // it ahead of `ensure_noesis_scene` once is harmless.
        render_app.add_systems(
            Render,
            (sync_click_subscriptions, sync_keydown_subscriptions).in_set(RenderSystems::Prepare),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_click_queue_drain_takes_all_and_resets() {
        let q = SharedClickQueue::default();
        q.push("Alpha".into());
        q.push("Beta".into());
        let drained = q.drain();
        assert_eq!(drained, vec!["Alpha".to_string(), "Beta".to_string()]);
        // Second drain returns empty without allocating.
        let empty: Vec<String> = q.drain();
        assert!(empty.is_empty());
    }

    #[test]
    fn click_watch_constructor_normalizes_into_strings() {
        let w = NoesisClickWatch::new(["a", "b", "c"]);
        assert_eq!(
            w.names,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
    }
}

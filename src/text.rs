//! Imperative `Text` writes + reactive `Text` reads against named XAML
//! elements (`TextBox` / `TextBlock`).
//!
//! Two halves:
//!
//! 1. [`NoesisTextRequests`] — main-app push queue for `(x:Name, text)`
//!    writes. Drained on the render side each frame and applied via
//!    `dm_noesis_runtime::view::FrameworkElement::set_text`. Mirrors
//!    [`crate::visibility::NoesisVisibilityRequests`] in shape.
//!
//! 2. [`NoesisTextReadWatch`] + [`NoesisTextChanged`] — reactive read
//!    side. Push an `x:Name` onto the watch list to subscribe to
//!    text-property changes; the render world polls `text()` each
//!    frame, dedupes against the previous snapshot, and emits a
//!    [`NoesisTextChanged`] message when the value differs. The first
//!    frame after subscription always emits (the snapshot starts
//!    empty), so callers reliably see the current text without having
//!    to issue a probe.
//!
//! The split (writes go through one queue, reads through another)
//! mirrors the click / visibility pattern: writes are infrequent and
//! main-driven, reads are render-driven and continuous. Combining them
//! into a single resource would mean a single Mutex for both, which is
//! the wrong shape on the read path (the lock would be held while the
//! main world drains the change list).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};

use crate::render::NoesisRenderState;

// ─────────────────────────────────────────────────────────────────────────────
// Write side — NoesisTextRequests
// ─────────────────────────────────────────────────────────────────────────────

/// Main-app-side queue of pending text writes. Push via [`Self::set`];
/// the render world drains and applies during `RenderSystems::Prepare`.
///
/// Cheap to keep around even when no writes are pending — the underlying
/// `Vec` only allocates on first push.
#[derive(Resource, Clone, Default)]
pub struct NoesisTextRequests(SharedTextWriteQueue);

impl NoesisTextRequests {
    /// Queue a write setting `name`'s `Text` to `text`. The element must
    /// be a `TextBox` or `TextBlock` (or another type that implements
    /// the same `Text` DP — see the runtime FFI for which casts are
    /// supported); type mismatches log a warning on apply.
    pub fn set(&self, name: impl Into<String>, text: impl Into<String>) {
        self.0.push(name.into(), text.into());
    }
}

impl ExtractResource for NoesisTextRequests {
    type Source = NoesisTextRequests;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

#[derive(Clone, Default)]
pub(crate) struct SharedTextWriteQueue(Arc<Mutex<Vec<(String, String)>>>);

impl SharedTextWriteQueue {
    fn push(&self, name: String, text: String) {
        self.0
            .lock()
            .expect("SharedTextWriteQueue poisoned")
            .push((name, text));
    }

    pub(crate) fn drain(&self) -> Vec<(String, String)> {
        let mut guard = self.0.lock().expect("SharedTextWriteQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn apply_text_writes(
    requests: Option<Res<NoesisTextRequests>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(requests), Some(mut state)) = (requests, state) else {
        return;
    };
    state.apply_text_writes(&requests.0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Read side — NoesisTextReadWatch + NoesisTextChanged
// ─────────────────────────────────────────────────────────────────────────────

/// `x:Name`s whose `Text` property the render world should poll each
/// frame. Mirrors [`crate::events::NoesisClickWatch`] in shape — push
/// names onto `names` to subscribe, remove them to unsubscribe.
///
/// Subscribed-then-resolved emits a [`NoesisTextChanged`] message even
/// when the text hasn't changed since the last frame, because the
/// render-side snapshot starts empty. After that initial event, only
/// genuine changes drive the message.
#[derive(Resource, ExtractResource, Clone, Default, Debug)]
pub struct NoesisTextReadWatch {
    pub names: Vec<String>,
}

impl NoesisTextReadWatch {
    pub fn new(names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            names: names.into_iter().map(Into::into).collect(),
        }
    }
}

/// Bevy message written in `PreUpdate` when a watched element's `Text`
/// differs from the previous frame's snapshot.
#[derive(Message, Debug, Clone)]
pub struct NoesisTextChanged {
    /// `x:Name` of the element whose Text changed.
    pub name: String,
    /// Current `Text` value. Empty string for an unset / cleared DP.
    pub text: String,
}

#[derive(Resource, Clone, Default)]
pub struct SharedTextChangedQueue(pub(crate) Arc<Mutex<Vec<(String, String)>>>);

impl ExtractResource for SharedTextChangedQueue {
    type Source = SharedTextChangedQueue;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

impl SharedTextChangedQueue {
    pub(crate) fn push(&self, name: String, text: String) {
        self.0
            .lock()
            .expect("SharedTextChangedQueue poisoned")
            .push((name, text));
    }

    fn drain(&self) -> Vec<(String, String)> {
        let mut guard = self.0.lock().expect("SharedTextChangedQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
pub fn drain_text_changed_queue(
    queue: Res<SharedTextChangedQueue>,
    mut messages: MessageWriter<NoesisTextChanged>,
) {
    for (name, text) in queue.drain() {
        messages.write(NoesisTextChanged { name, text });
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn poll_text_reads(
    watch: Option<Res<NoesisTextReadWatch>>,
    queue: Option<Res<SharedTextChangedQueue>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(watch), Some(queue), Some(mut state)) = (watch, queue, state) else {
        return;
    };
    state.poll_text_reads(&watch.names, &queue);
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the text-write + text-read bridges. Insert via
/// [`crate::NoesisPlugin`] (which adds it transitively).
pub struct NoesisTextPlugin;

impl Plugin for NoesisTextPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisTextRequests>()
            .init_resource::<NoesisTextReadWatch>()
            .insert_resource(SharedTextChangedQueue::default())
            .add_message::<NoesisTextChanged>()
            .add_plugins((
                ExtractResourcePlugin::<NoesisTextRequests>::default(),
                ExtractResourcePlugin::<NoesisTextReadWatch>::default(),
                ExtractResourcePlugin::<SharedTextChangedQueue>::default(),
            ))
            .add_systems(PreUpdate, drain_text_changed_queue);

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app.add_systems(
            Render,
            (apply_text_writes, poll_text_reads).in_set(RenderSystems::Prepare),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_write_queue_drain_round_trip() {
        let q = SharedTextWriteQueue::default();
        q.push("LogText".into(), "hello".into());
        q.push("CommandInput".into(), "world".into());
        let drained = q.drain();
        assert_eq!(
            drained,
            vec![
                ("LogText".to_string(), "hello".to_string()),
                ("CommandInput".to_string(), "world".to_string()),
            ],
        );
        assert!(q.drain().is_empty(), "second drain should be empty");
    }

    #[test]
    fn text_read_watch_constructor_normalizes() {
        let w = NoesisTextReadWatch::new(["a", "b", "c"]);
        assert_eq!(
            w.names,
            vec!["a".to_string(), "b".to_string(), "c".to_string()],
        );
    }
}

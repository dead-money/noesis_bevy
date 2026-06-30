//! Per-view `Text` bridge: write and observe the `Text` of named XAML
//! elements (`TextBox` / `TextBlock`) on a single [`crate::NoesisView`].
//!
//! Add a [`NoesisText`] component to the view's camera entity. Its `set` map is
//! the desired text per `x:Name`, applied to the view's elements whenever the
//! component changes (Bevy change detection). Its `watch` list names elements
//! whose `Text` to observe; changes surface as a [`NoesisTextChanged`] message
//! carrying the originating `view` entity.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisText::new()
//!         .with("Title", "Hello, Noesis!")
//!         .watching(["CommandInput"]),
//! );
//!
//! fn on_text(mut changed: MessageReader<NoesisTextChanged>) {
//!     for ev in changed.read() {
//!         info!("view {:?} element {:?} -> {:?}", ev.view, ev.name, ev.text);
//!     }
//! }
//! ```
//!
//! Each `x:Name` may be **scope-qualified** with `/` to reach an element inside
//! a composed control whose private namescope a root-level lookup can't see —
//! e.g. `with("MainMenu/Title", "Hello")` writes the `Title` inside a hosted
//! `MainMenu` control. Watched qualified names are echoed back verbatim on
//! [`NoesisTextChanged`], so two controls that each contain a `"Title"` stay
//! distinguishable. Plain names are unchanged.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component, applies writes + polls the
//! watch list against that view's live scene, and emits messages directly; no
//! cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view text bridge. Attach to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisText {
    /// Desired `Text` per element `x:Name`. Written to the view's elements
    /// whenever this component changes. Each target must be a `TextBox` /
    /// `TextBlock` (or another element exposing the `Text` DP).
    pub set: HashMap<String, String>,
    /// Element `x:Name`s whose `Text` to observe. A change (vs. the previous
    /// frame) emits a [`NoesisTextChanged`]; the first poll after a name is
    /// added always reports, so callers see the current value.
    pub watch: Vec<String>,
}

impl NoesisText {
    /// Creates an empty bridge with no writes and no watched elements. Chain
    /// [`with`](Self::with) and [`watching`](Self::watching) to populate it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s `Text` to `text`.
    #[must_use]
    pub fn with(mut self, name: impl Into<String>, text: impl Into<String>) -> Self {
        self.set.insert(name.into(), text.into());
        self
    }

    /// Builder: observe these elements' `Text`.
    #[must_use]
    pub fn watching(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.watch.extend(names.into_iter().map(Into::into));
        self
    }

    /// Set element `name`'s `Text` from a system holding `&mut NoesisText`. The
    /// runtime counterpart of [`with`](Self::with): the next reconcile applies
    /// it to the live element.
    pub fn write(&mut self, name: impl Into<String>, text: impl Into<String>) {
        self.set.insert(name.into(), text.into());
    }

    /// Observe element `name`'s `Text` from a system holding `&mut NoesisText`.
    /// No-op if it is already watched. The runtime counterpart of
    /// [`watching`](Self::watching). Named `observe` (not `watch`) to avoid
    /// colliding with the [`watch`](Self::watch) field.
    pub fn observe(&mut self, name: impl Into<String>) {
        let name = name.into();
        if !self.watch.contains(&name) {
            self.watch.push(name);
        }
    }
}

/// Emitted when a watched element's `Text` differs from the previous frame.
#[derive(Message, Debug, Clone)]
pub struct NoesisTextChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the element.
    pub name: String,
    /// Current `Text`. Empty string for an unset / cleared DP.
    pub text: String,
}

/// Reconcile every view's [`NoesisText`]: apply desired writes when the
/// component changed, then poll its watch list and emit [`NoesisTextChanged`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_text_bridge(
    views: Query<(Entity, Ref<NoesisText>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisTextChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, text) in &views {
        if text.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_text_writes_for(entity, &text.set);
        }
        for (name, value) in state.poll_text_reads_for(entity, &text.watch) {
            changed.write(NoesisTextChanged {
                view: entity,
                name,
                text: value,
            });
        }
    }
}

/// Wires the per-view text bridge. Added transitively by [`crate::NoesisPlugin`].
pub struct NoesisTextPlugin;

impl Plugin for NoesisTextPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisTextChanged>()
            .add_systems(PostUpdate, sync_text_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_set_and_watch() {
        let t = NoesisText::new()
            .with("Title", "Hello")
            .with("Sub", "World")
            .watching(["Status", "Clock"]);
        assert_eq!(t.set.get("Title").map(String::as_str), Some("Hello"));
        assert_eq!(t.set.get("Sub").map(String::as_str), Some("World"));
        assert_eq!(t.watch, vec!["Status".to_string(), "Clock".to_string()]);
    }
}

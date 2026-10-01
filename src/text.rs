//! Writes and watches the `Text` of named elements (`TextBox`, `TextBlock`) in
//! a [`NoesisView`](crate::NoesisView).
//!
//! Add a [`NoesisText`] to the view's camera entity. [`set`](NoesisText::set)
//! maps `x:Name` to the text to write; [`watch`](NoesisText::watch) lists
//! elements whose `Text` to report through [`NoesisTextChanged`].
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
//! A name may be scope-qualified with `/` to reach an element inside a composed
//! control's private namescope: `with("MainMenu/Title", "Hello")` writes the
//! `Title` inside a hosted `MainMenu`. Watched names are reported verbatim, so
//! two controls that each contain a `Title` stay distinguishable.
//!
//! Whenever the component changes, or the view's scene is rebuilt, every entry
//! in `set` is written again. An entry for a `TextBox` therefore overwrites
//! whatever the user typed the next time any part of the component changes;
//! remove the entry once it has been written if the user should own the text.
//! Removing an entry does not clear the element. Writes made through `set` do
//! not echo back as [`NoesisTextChanged`].

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Text writes and watches for named elements. Add to a
/// [`NoesisView`](crate::NoesisView) camera entity; see the
/// [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisText {
    /// `Text` to write per element `x:Name`. The target must expose a `Text`
    /// property (`TextBox`, `TextBlock`); a missing name or other element type
    /// is skipped with a warning.
    pub set: HashMap<String, String>,
    /// Element `x:Name`s whose `Text` to report. Polled every frame; a change
    /// emits a [`NoesisTextChanged`]. A newly watched name reports its current
    /// value once.
    pub watch: Vec<String>,
}

impl NoesisText {
    /// An empty bridge. Chain [`with`](Self::with) and
    /// [`watching`](Self::watching) to populate it.
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

    /// Set element `name`'s `Text`. The `&mut` form of [`with`](Self::with),
    /// for systems that update the component.
    pub fn write(&mut self, name: impl Into<String>, text: impl Into<String>) {
        self.set.insert(name.into(), text.into());
    }

    /// Watch element `name`'s `Text`, if not already watched. The `&mut` form
    /// of [`watching`](Self::watching).
    pub fn observe(&mut self, name: impl Into<String>) {
        let name = name.into();
        if !self.watch.contains(&name) {
            self.watch.push(name);
        }
    }
}

/// A watched element's `Text` changed since the previous frame, or the name
/// was just added to [`NoesisText::watch`].
#[derive(Message, Debug, Clone)]
pub struct NoesisTextChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the element, as listed in [`NoesisText::watch`].
    pub name: String,
    /// Current `Text`; empty when unset.
    pub text: String,
}

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

/// Registers the text bridge. Added by [`crate::NoesisPlugin`].
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

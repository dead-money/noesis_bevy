//! Per-view `ItemsSource` bridge — populate XAML list controls
//! (`ComboBox` / `ListBox` / `ItemsControl`) from a Bevy app.
//!
//! Add a [`NoesisItems`] component to the view's camera entity mapping each list
//! control's `x:Name` to its desired items. The reconcile system keeps a
//! Rust-owned [`ObservableCollection`](noesis_runtime::binding::ObservableCollection)
//! per `(view, x:Name)`, sets it to the desired list whenever the component
//! changes, and binds it to the element's `ItemsSource` once the element exists
//! (re-binding after a scene rebuild).
//!
//! # String items only
//!
//! The safe `ObservableCollection` surface (`unsafe_code = forbid`) is
//! `push_string` / `remove_at` / `clear`, so items are **strings** — exactly
//! what a `ComboBox` of text options needs. `SelectedIndex` is typically two-way
//! bound through a view model (see [`crate::viewmodel`]).
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisItems::new().with("QualityCombo", ["Low", "Medium", "High"]),
//! );
//! ```
//!
//! # Lifetime & threading
//!
//! Collections are owned in [`NoesisRenderState`](crate::render) (Noesis objects
//! are thread-affine to the `View`) and released before
//! `noesis_runtime::shutdown`.

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::binding::ObservableCollection;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view component: desired item list per list-control `x:Name`. Attach to a
/// [`NoesisView`](crate::NoesisView) entity. Setting a list replaces the
/// control's items (the collection is observable, so the live control updates
/// without a view rebuild).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisItems {
    pub sources: HashMap<String, Vec<String>>,
}

impl NoesisItems {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s items.
    #[must_use]
    pub fn with(
        mut self,
        name: impl Into<String>,
        items: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        self.sources
            .insert(name.into(), items.into_iter().map(Into::into).collect());
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-world binding — ItemsBinding
// ─────────────────────────────────────────────────────────────────────────────

/// One element's Rust-owned items list: an [`ObservableCollection`] plus the URI
/// of the scene it's currently bound to. Owned by
/// [`NoesisRenderState`](crate::render), released before runtime shutdown.
///
/// `pub` so headless tests can exercise the same op → collection translation the
/// render systems use; apps drive it through [`NoesisItems`], never directly.
pub struct ItemsBinding {
    coll: ObservableCollection,
    bound_for_uri: Option<String>,
}

impl Default for ItemsBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemsBinding {
    /// A fresh, empty, unbound items collection.
    #[must_use]
    pub fn new() -> Self {
        Self {
            coll: ObservableCollection::new(),
            bound_for_uri: None,
        }
    }

    /// Replace the whole list.
    pub fn set<I, S>(&mut self, items: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.coll.clear();
        for item in items {
            self.coll.push_string(item.as_ref());
        }
    }

    /// Append one item.
    pub fn push(&mut self, item: &str) {
        self.coll.push_string(item);
    }

    /// Remove the item at `index` (ignored if out of range).
    pub fn remove_at(&mut self, index: usize) {
        self.coll.remove_at(index);
    }

    /// Empty the list.
    pub fn clear(&mut self) {
        self.coll.clear();
    }

    /// The backing collection, for handing to
    /// [`FrameworkElement::set_items_source`](noesis_runtime::view::FrameworkElement::set_items_source).
    #[must_use]
    pub fn collection(&self) -> &ObservableCollection {
        &self.coll
    }

    pub(crate) fn needs_bind(&self, uri: &str) -> bool {
        self.bound_for_uri.as_deref() != Some(uri)
    }

    pub(crate) fn mark_bound(&mut self, uri: &str) {
        self.bound_for_uri = Some(uri.to_owned());
    }

    /// Detach (logically) so the next bind pass re-binds against the rebuilt
    /// scene. Called from scene teardown.
    pub(crate) fn reset_bind(&mut self) {
        self.bound_for_uri = None;
    }
}

/// Reconcile every view's [`NoesisItems`]: set collections when the component
/// changed, and (re-)bind them to their elements each frame.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_items_bridge(
    views: Query<(Entity, Ref<NoesisItems>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, items) in &views {
        state.apply_items_for(entity, &items.sources, items.is_changed());
    }
}

/// Wires the per-view `ItemsSource` bridge. Added transitively by [`crate::NoesisPlugin`].
pub struct NoesisItemsPlugin;

impl Plugin for NoesisItemsPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_items_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_sources() {
        let i = NoesisItems::new()
            .with("Combo", ["a", "b"])
            .with("List", vec!["x".to_string()]);
        assert_eq!(i.sources["Combo"], vec!["a".to_string(), "b".to_string()]);
        assert_eq!(i.sources["List"], vec!["x".to_string()]);
    }
}

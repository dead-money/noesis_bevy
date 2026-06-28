//! Rust-owned `ItemsSource` bridge (TODO §3): populate XAML list controls
//! (`ComboBox` / `ListBox` / `ItemsControl`) from a Bevy app.
//!
//! Where the [`viewmodel`](crate::viewmodel) bridge drives a control's scalar
//! dependency properties, this drives its *items*: an
//! [`ObservableCollection`](dm_noesis_runtime::binding::ObservableCollection)
//! is attached as a named element's `ItemsSource`, and the app mutates it from
//! gameplay code. Because the collection is observable, incremental edits
//! (`push` / `remove` / `clear`) flow to the live control without rebuilding
//! the view.
//!
//! # String items only
//!
//! The safe `ObservableCollection` surface this crate can reach
//! (`unsafe_code = forbid`) is `push_string` / `remove_at` / `clear`, so the
//! bridge handles **string** items — exactly what a `ComboBox` of text options
//! needs (`"Low"` / `"Medium"` / `"High"`). The control displays each string
//! and `SelectedIndex` is typically two-way bound through a view model (see
//! [`crate::viewmodel`]). Typed items (numbers, view models) would need a safe
//! `push_*` added to the runtime, mirroring how the binding bridge needed a
//! safe `set_data_context`.
//!
//! # Lifetime & threading
//!
//! Each named element gets one render-world [`ObservableCollection`], owned in
//! [`NoesisRenderState`](crate::render) (Noesis objects are thread-affine to
//! the `View`) and released before `dm_noesis_runtime::shutdown`. The
//! collection is bound to the element via the safe
//! [`FrameworkElement::set_items_source`](dm_noesis_runtime::view::FrameworkElement::set_items_source)
//! once the element exists, and re-bound after a scene rebuild. Edits flow
//! main → render through the usual queue → [`ExtractResource`] → drain in
//! `RenderSystems::Prepare`, retained until the element resolves.
//!
//! # Usage
//!
//! ```ignore
//! use bevy::prelude::*;
//! use dm_noesis_bevy::items::NoesisItemsSources;
//!
//! fn setup(items: Res<NoesisItemsSources>) {
//!     // Populate a <ComboBox x:Name="QualityCombo"/> from Rust.
//!     items.set("QualityCombo", ["Low", "Medium", "High"]);
//! }
//!
//! fn add_option(items: Res<NoesisItemsSources>) {
//!     items.push("QualityCombo", "Ultra"); // appears live in the open ComboBox
//! }
//! ```

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};
use dm_noesis_runtime::binding::ObservableCollection;

use crate::render::NoesisRenderState;

// ─────────────────────────────────────────────────────────────────────────────
// Op queue
// ─────────────────────────────────────────────────────────────────────────────

/// A pending edit to a named element's items list. Drained render-side and
/// applied to the element's [`ObservableCollection`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ItemsOp {
    /// Replace the whole list (clear, then append each).
    Set(Vec<String>),
    /// Append one item.
    Push(String),
    /// Remove the item at `index` (no-op if out of range).
    RemoveAt(usize),
    /// Empty the list.
    Clear,
}

// ─────────────────────────────────────────────────────────────────────────────
// Main-world resource — NoesisItemsSources
// ─────────────────────────────────────────────────────────────────────────────

/// Main-app handle for driving named list controls' items. Insert via
/// [`NoesisItemsPlugin`]; the render world receives an `Arc`-aliased copy each
/// frame via [`ExtractResource`], so edits pushed here are drained render-side
/// without copying.
///
/// All methods take `&self` (interior-mutable queue), so they're callable from
/// a plain `Res<NoesisItemsSources>`. Edits to the same `x:Name` within a frame
/// apply in order.
#[derive(Resource, Clone, Default)]
pub struct NoesisItemsSources {
    queue: Arc<Mutex<Vec<(String, ItemsOp)>>>,
}

impl NoesisItemsSources {
    /// Replace the entire items list of the element named `name` with `items`.
    /// The element must be an `ItemsControl` (`ComboBox` / `ListBox` / …); a
    /// type mismatch logs a warning on apply.
    pub fn set(&self, name: impl Into<String>, items: impl IntoIterator<Item = impl Into<String>>) {
        let items = items.into_iter().map(Into::into).collect();
        self.push_op(name, ItemsOp::Set(items));
    }

    /// Append one item to the element named `name`.
    pub fn push(&self, name: impl Into<String>, item: impl Into<String>) {
        self.push_op(name, ItemsOp::Push(item.into()));
    }

    /// Remove the item at `index` from the element named `name`. Out-of-range
    /// indices are ignored render-side.
    pub fn remove_at(&self, name: impl Into<String>, index: usize) {
        self.push_op(name, ItemsOp::RemoveAt(index));
    }

    /// Clear the items of the element named `name`.
    pub fn clear(&self, name: impl Into<String>) {
        self.push_op(name, ItemsOp::Clear);
    }

    fn push_op(&self, name: impl Into<String>, op: ItemsOp) {
        self.queue
            .lock()
            .expect("NoesisItemsSources queue poisoned")
            .push((name.into(), op));
    }

    /// Drain pending edits. Render-world only; cheap when empty.
    pub(crate) fn drain(&self) -> Vec<(String, ItemsOp)> {
        let mut guard = self
            .queue
            .lock()
            .expect("NoesisItemsSources queue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

impl ExtractResource for NoesisItemsSources {
    type Source = NoesisItemsSources;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-world binding — ItemsBinding
// ─────────────────────────────────────────────────────────────────────────────

/// One named element's Rust-owned items list: an [`ObservableCollection`] plus
/// the URI of the scene it's currently bound to. Owned by
/// [`NoesisRenderState`](crate::render) and released before runtime shutdown.
///
/// `pub` so headless tests can exercise the same translation the render systems
/// use (op → `ObservableCollection` call → bound control), but apps drive it
/// through [`NoesisItemsSources`], never directly.
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
    /// [`FrameworkElement::set_items_source`](dm_noesis_runtime::view::FrameworkElement::set_items_source).
    #[must_use]
    pub fn collection(&self) -> &ObservableCollection {
        &self.coll
    }

    fn apply(&mut self, op: ItemsOp) {
        match op {
            ItemsOp::Set(items) => self.set(items),
            ItemsOp::Push(item) => self.push(&item),
            ItemsOp::RemoveAt(index) => self.remove_at(index),
            ItemsOp::Clear => self.clear(),
        }
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

/// Apply a drained op to the binding for `name`, creating it on first use.
/// Lives here (not on [`NoesisRenderState`]) so the op→collection translation
/// is unit-testable without a render world.
pub(crate) fn apply_op(
    bindings: &mut std::collections::HashMap<String, ItemsBinding>,
    name: String,
    op: ItemsOp,
) {
    bindings.entry(name).or_default().apply(op);
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-app system
// ─────────────────────────────────────────────────────────────────────────────

/// Drain pending items edits → apply to per-element collections → bind any
/// unbound collection to its element's `ItemsSource`. No-op (queue retained)
/// until [`NoesisRenderState`] and the target element exist.
pub(crate) fn apply_items_sources(
    requests: Option<Res<NoesisItemsSources>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(requests), Some(mut state)) = (requests, state) else {
        return;
    };
    state.apply_items_sources(&requests);
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the `ItemsSource` bridge: installs [`NoesisItemsSources`], extracts it
/// to the render world, and runs the render-side apply/bind pass in
/// `RenderSystems::Prepare`. Added transitively by [`crate::NoesisPlugin`].
pub struct NoesisItemsPlugin;

impl Plugin for NoesisItemsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisItemsSources>()
            .add_plugins(ExtractResourcePlugin::<NoesisItemsSources>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app.add_systems(Render, apply_items_sources.in_set(RenderSystems::Prepare));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_queue_round_trips_in_order() {
        let items = NoesisItemsSources::default();
        items.set("Combo", ["a", "b"]);
        items.push("Combo", "c");
        items.remove_at("Combo", 0);
        items.clear("Other");

        let drained = items.drain();
        assert_eq!(
            drained,
            vec![
                (
                    "Combo".to_string(),
                    ItemsOp::Set(vec!["a".to_string(), "b".to_string()]),
                ),
                ("Combo".to_string(), ItemsOp::Push("c".to_string())),
                ("Combo".to_string(), ItemsOp::RemoveAt(0)),
                ("Other".to_string(), ItemsOp::Clear),
            ],
        );
        assert!(items.drain().is_empty());
    }

    #[test]
    fn set_accepts_strings_and_str_slices() {
        let items = NoesisItemsSources::default();
        items.set("X", vec!["one".to_string(), "two".to_string()]);
        items.set("Y", ["three", "four"]);
        let drained = items.drain();
        assert_eq!(drained.len(), 2);
        assert_eq!(
            drained[0].1,
            ItemsOp::Set(vec!["one".to_string(), "two".to_string()]),
        );
        assert_eq!(
            drained[1].1,
            ItemsOp::Set(vec!["three".to_string(), "four".to_string()]),
        );
    }
}

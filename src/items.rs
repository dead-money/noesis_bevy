//! Per-view `ItemsSource` bridge — populate XAML list controls
//! (`ComboBox` / `ListBox` / `ItemsControl`) from a Bevy app, with typed items.
//!
//! Add a [`NoesisItems`] component to the view's camera entity mapping each list
//! control's `x:Name` to its desired items. The reconcile system keeps a
//! Rust-owned [`ObservableCollection`](noesis_runtime::binding::ObservableCollection)
//! per `(view, x:Name)`, sets it to the desired list whenever the component
//! changes, and binds it to the element's `ItemsSource` once the element exists
//! (re-binding after a scene rebuild).
//!
//! # Typed items
//!
//! Items are [`ItemValue`]s: strings, `i32`, `f64`, or `bool`. The safe
//! `ObservableCollection` surface (`unsafe_code = forbid`) boxes each kind with
//! the matching `push_*`, so a list can be e.g. integers (`<ListBox>` of port
//! numbers) or strings (`ComboBox` of text options). [`with`](NoesisItems::with)
//! stays string-compatible — `with("Combo", ["Low", "High"])` still works,
//! because `&str` is `Into<ItemValue>` — and accepts any homogeneous typed
//! iterator (`with("Ports", [80, 443])`); use
//! [`with_items`](NoesisItems::with_items) for an explicit / mixed list.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisItems::new()
//!         .with("QualityCombo", ["Low", "Medium", "High"]) // strings
//!         .with("PortList", [80, 443, 8080])               // i32
//!         .select("PortList", 1),                          // drive selection
//! );
//! ```
//!
//! # Selection read-back
//!
//! [`select`](NoesisItems::select) drives a control's `SelectedIndex` (and its
//! current item). Each frame the bridge emits a [`NoesisItemsCurrent`] message
//! carrying the control's item `count`, its `selected_index`, the view's
//! `current_position`, and the *typed* `current` item read back out of Noesis
//! (via an `ICollectionView`'s `CurrentItem` accessors) — proving the typed
//! value made the round trip through the engine, not just the Rust copy.
//!
//! # Collection-view navigation
//!
//! Every bound list also has a default `ICollectionView` over its source (the
//! same shared view a `Selector` synchronizes against). [`navigate`](NoesisItems::navigate)
//! drives that view's *current item* with a [`CollectionViewOp`]
//! (`First`/`Last`/`Next`/`Previous`/`To(pos)`), mirroring
//! `ICollectionView::MoveCurrentTo*`. The op is applied once each time the
//! component changes; the resulting `current_position` / `current` item surface
//! via [`NoesisItemsCurrent`]. Sorting, filtering and grouping are a genuine
//! Noesis SDK limitation (no programmatic `SortDescription`/`Filter` is
//! exposed), so they are intentionally absent — see
//! [`noesis_runtime::collection_view`].
//!
//! # Lifetime & threading
//!
//! Collections are owned in [`NoesisRenderState`](crate::render) (Noesis objects
//! are thread-affine to the `View`) and released before
//! `noesis_runtime::shutdown`.

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::binding::ObservableCollection;
use noesis_runtime::collection_view::{CollectionView, CollectionViewSource, CurrentItem};
use noesis_runtime::view::FrameworkElement;

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Typed item value
// ─────────────────────────────────────────────────────────────────────────────

/// One typed list item. The variant selects the runtime boxing
/// (`push_string` / `push_i32` / `push_f64` / `push_bool`) and the matching
/// unbox used when reading the current item back.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemValue {
    Str(String),
    I32(i32),
    F64(f64),
    Bool(bool),
}

impl From<&str> for ItemValue {
    fn from(v: &str) -> Self {
        Self::Str(v.to_owned())
    }
}

impl From<String> for ItemValue {
    fn from(v: String) -> Self {
        Self::Str(v)
    }
}

impl From<&String> for ItemValue {
    fn from(v: &String) -> Self {
        Self::Str(v.clone())
    }
}

impl From<i32> for ItemValue {
    fn from(v: i32) -> Self {
        Self::I32(v)
    }
}

impl From<f64> for ItemValue {
    fn from(v: f64) -> Self {
        Self::F64(v)
    }
}

impl From<bool> for ItemValue {
    fn from(v: bool) -> Self {
        Self::Bool(v)
    }
}

impl ItemValue {
    /// Append this item to `coll` with the boxing matching its variant.
    fn push_into(&self, coll: &mut ObservableCollection) {
        match self {
            Self::Str(v) => {
                coll.push_string(v);
            }
            Self::I32(v) => {
                coll.push_i32(*v);
            }
            Self::F64(v) => {
                coll.push_f64(*v);
            }
            Self::Bool(v) => {
                coll.push_bool(*v);
            }
        }
    }
}

/// Unbox an `ICollectionView` current item into a typed [`ItemValue`], probing
/// each boxed primitive type (the boxes are mutually exclusive, so only the
/// pushed kind matches). `None` if the item is not a boxed primitive.
fn current_item_value(item: &CurrentItem) -> Option<ItemValue> {
    if let Some(s) = item.as_string() {
        return Some(ItemValue::Str(s));
    }
    if let Some(b) = item.as_bool() {
        return Some(ItemValue::Bool(b));
    }
    if let Some(i) = item.as_i32() {
        return Some(ItemValue::I32(i));
    }
    if let Some(f) = item.as_f64() {
        return Some(ItemValue::F64(f));
    }
    None
}

// ─────────────────────────────────────────────────────────────────────────────
// Collection-view navigation
// ─────────────────────────────────────────────────────────────────────────────

/// One `ICollectionView` current-item navigation op, mirroring
/// `ICollectionView::MoveCurrentTo*`. Applied to a bound list's default view.
///
/// `First`/`Last`/`To` are absolute (idempotent); `Next`/`Previous` are relative
/// and step from the current position each time they are applied. The bridge
/// applies the op once per [`NoesisItems`] change (see the module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollectionViewOp {
    /// `MoveCurrentToFirst`.
    First,
    /// `MoveCurrentToLast`.
    Last,
    /// `MoveCurrentToNext` (lands *after the last* at the end).
    Next,
    /// `MoveCurrentToPrevious` (lands *before the first* at the start).
    Previous,
    /// `MoveCurrentToPosition(pos)` (`-1` = before first, `count` = after last).
    To(i32),
}

impl CollectionViewOp {
    /// Apply this op to `view`, returning the raw `bool` Noesis reports (its
    /// boundary meaning is an SDK detail — query the resulting position instead).
    fn apply(self, view: &CollectionView) -> bool {
        match self {
            Self::First => view.move_current_to_first(),
            Self::Last => view.move_current_to_last(),
            Self::Next => view.move_current_to_next(),
            Self::Previous => view.move_current_to_previous(),
            Self::To(pos) => view.move_current_to_position(pos),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Component
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view component: desired item list per list-control `x:Name`. Attach to a
/// [`NoesisView`](crate::NoesisView) entity. Setting a list replaces the
/// control's items (the collection is observable, so the live control updates
/// without a view rebuild). [`select`](Self::select) drives a control's
/// selected index.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisItems {
    /// Desired items per `x:Name`.
    pub sources: HashMap<String, Vec<ItemValue>>,
    /// Desired selected index per `x:Name` (`-1` clears the selection). Applied
    /// when the component changes; the resulting selection surfaces via
    /// [`NoesisItemsCurrent`].
    pub select: HashMap<String, i32>,
    /// Desired collection-view navigation op per `x:Name`. Applied to the
    /// control's default `ICollectionView` once each time the component changes;
    /// the resulting current item surfaces via [`NoesisItemsCurrent`].
    pub navigate: HashMap<String, CollectionViewOp>,
}

impl NoesisItems {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s items from any homogeneous typed iterator.
    /// `&str` / `String` / `i32` / `f64` / `bool` all convert, so the original
    /// string usage (`with("Combo", ["a", "b"])`) is unchanged.
    #[must_use]
    pub fn with(
        mut self,
        name: impl Into<String>,
        items: impl IntoIterator<Item = impl Into<ItemValue>>,
    ) -> Self {
        self.sources
            .insert(name.into(), items.into_iter().map(Into::into).collect());
        self
    }

    /// Builder: set element `name`'s items from an explicit (possibly mixed)
    /// [`ItemValue`] list.
    #[must_use]
    pub fn with_items(mut self, name: impl Into<String>, items: Vec<ItemValue>) -> Self {
        self.sources.insert(name.into(), items);
        self
    }

    /// Builder: drive element `name`'s `SelectedIndex` to `index` (`-1` clears).
    #[must_use]
    pub fn select(mut self, name: impl Into<String>, index: i32) -> Self {
        self.select.insert(name.into(), index);
        self
    }

    /// Builder: drive element `name`'s default `ICollectionView` current item
    /// with a [`CollectionViewOp`]. Applied once per component change.
    #[must_use]
    pub fn navigate(mut self, name: impl Into<String>, op: CollectionViewOp) -> Self {
        self.navigate.insert(name.into(), op);
        self
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-world binding — ItemsBinding
// ─────────────────────────────────────────────────────────────────────────────

/// One element's Rust-owned items list: an [`ObservableCollection`], a
/// [`CollectionViewSource`] over it (for typed current-item read-back), and the
/// URI of the scene it's currently bound to. Owned by
/// [`NoesisRenderState`](crate::render), released before runtime shutdown.
///
/// `pub` so headless tests can exercise the same op → collection translation the
/// render systems use; apps drive it through [`NoesisItems`], never directly.
pub struct ItemsBinding {
    coll: ObservableCollection,
    /// Source of the view over `coll`. Declared after `coll` so it drops first
    /// (it holds a ref to `coll`).
    cvs: CollectionViewSource,
    /// Cached `ICollectionView` over `coll`, used to read the current item back
    /// as a typed value. Held for the binding's lifetime: dropping it would let
    /// Noesis discard the cached view and rebuild a fresh one (current position
    /// reset to the first item) on the next `GetView`.
    view: Option<CollectionView>,
    bound_for_uri: Option<String>,
    /// Desired selected index from [`NoesisItems::select`] (`None` = leave the
    /// control's selection alone).
    desired_select: Option<i32>,
    /// Last index actually pushed onto the control / view, so selection is
    /// driven once per change rather than every frame.
    applied_select: Option<i32>,
    /// Desired collection-view navigation op from [`NoesisItems::navigate`]
    /// (`None` = leave the view's current item alone).
    desired_nav: Option<CollectionViewOp>,
    /// Set when [`Self::set_desired_nav`] records an op on a component change;
    /// cleared once [`Self::drive_navigation`] applies it. Relative ops
    /// (`Next`/`Previous`) re-fire on each change rather than only on op change.
    nav_pending: bool,
    /// Last `(count, selected_index, current_position, current)` reported, to
    /// emit a message only on change. Mirrors the DP bridge's snapshot.
    last_readback: Option<(usize, i32, i32, Option<ItemValue>)>,
}

impl Default for ItemsBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemsBinding {
    /// A fresh, empty, unbound items collection (with its view over it).
    #[must_use]
    pub fn new() -> Self {
        let coll = ObservableCollection::new();
        let mut cvs = CollectionViewSource::new();
        cvs.set_source(&coll);
        let view = cvs.view();
        Self {
            coll,
            cvs,
            view,
            bound_for_uri: None,
            desired_select: None,
            applied_select: None,
            desired_nav: None,
            nav_pending: false,
            last_readback: None,
        }
    }

    /// Replace the whole list with string items (back-compat string API).
    pub fn set<I, S>(&mut self, items: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.coll.clear();
        for item in items {
            self.coll.push_string(item.as_ref());
        }
        self.applied_select = None;
    }

    /// Replace the whole list with typed items.
    pub fn set_typed(&mut self, items: &[ItemValue]) {
        self.coll.clear();
        for item in items {
            item.push_into(&mut self.coll);
        }
        // A new list invalidates any previously-applied selection.
        self.applied_select = None;
    }

    /// Append one string item.
    pub fn push(&mut self, item: &str) {
        self.coll.push_string(item);
    }

    /// Append one typed item.
    pub fn push_value(&mut self, item: &ItemValue) {
        item.push_into(&mut self.coll);
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

    /// Set the desired selected index (`None` = leave selection alone).
    pub(crate) fn set_desired_select(&mut self, index: Option<i32>) {
        if self.desired_select != index {
            self.desired_select = index;
            self.applied_select = None;
        }
    }

    /// Set the desired collection-view navigation op (`None` = leave the current
    /// item alone). Called once per component change, so relative ops re-arm on
    /// each change even when the op value is unchanged.
    pub(crate) fn set_desired_nav(&mut self, op: Option<CollectionViewOp>) {
        self.desired_nav = op;
        if op.is_some() {
            self.nav_pending = true;
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
        // The control is new; its selection must be re-driven.
        self.applied_select = None;
    }

    /// The cached `ICollectionView` over the collection (lazily re-fetched if it
    /// was never produced, e.g. the source was empty at construction).
    fn view(&mut self) -> Option<&CollectionView> {
        if self.view.is_none() {
            self.view = self.cvs.view();
        }
        self.view.as_ref()
    }

    /// Drive `element`'s selected index (and the view's current item) to the
    /// desired index, once per change. No-op when no selection is desired or it
    /// is already applied.
    pub(crate) fn drive_selection(&mut self, element: &mut FrameworkElement) {
        let Some(index) = self.desired_select else {
            return;
        };
        if self.applied_select == Some(index) {
            return;
        }
        // Drive the control's selection...
        let ok = element.set_selected_index(index);
        // ...and the view's current item, so the typed read-back reflects it.
        if let Some(view) = self.view() {
            view.move_current_to_position(index);
        }
        if ok {
            self.applied_select = Some(index);
        }
    }

    /// Apply the pending collection-view navigation op (set via
    /// [`Self::set_desired_nav`]) to the view's current item, once per change.
    /// No-op when no op is pending or the view is unavailable.
    pub(crate) fn drive_navigation(&mut self) {
        if !self.nav_pending {
            return;
        }
        let Some(op) = self.desired_nav else {
            self.nav_pending = false;
            return;
        };
        if let Some(view) = self.view() {
            op.apply(view);
            self.nav_pending = false;
        }
    }

    /// Apply a collection-view navigation op directly and report whether the
    /// view accepted the move. Imperative counterpart of the declarative
    /// [`NoesisItems::navigate`] path; query [`Self::current_position`] /
    /// [`Self::current_item_value`] for the resulting state.
    pub fn navigate(&mut self, op: CollectionViewOp) -> bool {
        self.view().is_some_and(|view| op.apply(view))
    }

    /// The view's current ordinal position (`-1` before first, `count` after
    /// last), or `-1` when no view exists yet.
    #[must_use]
    pub fn current_position(&mut self) -> i32 {
        self.view().map_or(-1, CollectionView::current_position)
    }

    /// The view's current item unboxed to its typed [`ItemValue`], or `None`
    /// when the cursor is off the ends (or the item is not a boxed primitive).
    #[must_use]
    pub fn current_item_value(&mut self) -> Option<ItemValue> {
        self.view()
            .and_then(CollectionView::current_item)
            .and_then(|item| current_item_value(&item))
    }

    /// Read `(count, selected_index, current_position, current-typed-value)` for
    /// `element`, returning it only when it differs from the last report.
    /// `count` is the control's item count; `selected_index` its `SelectedIndex`;
    /// `current_position` the view's `CurrentPosition`; `current` the view's
    /// current item unboxed to its [`ItemValue`].
    pub(crate) fn read_changed(
        &mut self,
        element: &FrameworkElement,
    ) -> Option<(usize, i32, i32, Option<ItemValue>)> {
        let count = element.items_count().unwrap_or(0);
        let selected_index = element.selected_index().unwrap_or(-1);
        let current_position = self.current_position();
        let current = self.current_item_value();
        let snap = (count, selected_index, current_position, current);
        if self.last_readback.as_ref() == Some(&snap) {
            return None;
        }
        self.last_readback = Some(snap.clone());
        Some(snap)
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Read-back message
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when a bound list control's `(count, selected_index, current item)`
/// differs from the previous frame. Read with `MessageReader<NoesisItemsCurrent>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisItemsCurrent {
    /// The [`NoesisView`](crate::NoesisView) entity owning the control.
    pub view: Entity,
    /// `x:Name` of the list control.
    pub name: String,
    /// Number of items the control sees through its bound source.
    pub count: usize,
    /// The control's `SelectedIndex` (`-1` when nothing is selected).
    pub selected_index: i32,
    /// The default `ICollectionView`'s `CurrentPosition` (`-1` before first,
    /// `count` after last).
    pub current_position: i32,
    /// The view's current item unboxed to its typed value, or `None` when the
    /// cursor is off the ends (or the item is not a boxed primitive).
    pub current: Option<ItemValue>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisItems`]: set collections + selection when the
/// component changed, (re-)bind them to their elements each frame, and emit a
/// [`NoesisItemsCurrent`] when a control's selection/count changes.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_items_bridge(
    views: Query<(Entity, Ref<NoesisItems>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut current: MessageWriter<NoesisItemsCurrent>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, items) in &views {
        state.apply_items_for(
            entity,
            &items.sources,
            &items.select,
            &items.navigate,
            items.is_changed(),
        );
        for (name, count, selected_index, current_position, value) in state.poll_items_for(entity) {
            current.write(NoesisItemsCurrent {
                view: entity,
                name,
                count,
                selected_index,
                current_position,
                current: value,
            });
        }
    }
}

/// Wires the per-view `ItemsSource` bridge. Added transitively by [`crate::NoesisPlugin`].
pub struct NoesisItemsPlugin;

impl Plugin for NoesisItemsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisItemsCurrent>()
            .add_systems(PostUpdate, sync_items_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_sources() {
        let i = NoesisItems::new()
            .with("Combo", ["a", "b"])
            .with("List", vec!["x".to_string()])
            .with("Ports", [80, 443])
            .select("Combo", 1)
            .navigate("Combo", CollectionViewOp::Next);
        assert_eq!(
            i.sources["Combo"],
            vec![ItemValue::Str("a".into()), ItemValue::Str("b".into())],
        );
        assert_eq!(i.sources["List"], vec![ItemValue::Str("x".into())]);
        assert_eq!(
            i.sources["Ports"],
            vec![ItemValue::I32(80), ItemValue::I32(443)],
        );
        assert_eq!(i.select["Combo"], 1);
        assert_eq!(i.navigate["Combo"], CollectionViewOp::Next);
    }

    #[test]
    fn item_value_conversions() {
        assert_eq!(ItemValue::from("s"), ItemValue::Str("s".into()));
        assert_eq!(ItemValue::from(3i32), ItemValue::I32(3));
        assert_eq!(ItemValue::from(2.5f64), ItemValue::F64(2.5));
        assert_eq!(ItemValue::from(true), ItemValue::Bool(true));
    }
}

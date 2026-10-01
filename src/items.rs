//! Fills XAML list controls (`ComboBox`, `ListBox`, `ItemsControl`) from Bevy.
//!
//! Add a [`NoesisItems`] component to the [`NoesisView`](crate::NoesisView)
//! camera entity, mapping each list control's `x:Name` to its items. The
//! bridge keeps one observable collection per `(view, x:Name)`, updates it
//! when the component changes, and binds it to the control's `ItemsSource`
//! once the element exists, again after each scene rebuild. Lists whose
//! items are unchanged are left alone, so editing one list doesn't reset the
//! selection or scroll of the others.
//!
//! For per-row entities with diffed updates and selection markers, use
//! [`UiList`](crate::UiList) instead.
//!
//! # Items
//!
//! Items are [`ItemValue`]s: strings, `i32`, `f64` or `bool`.
//! [`with`](NoesisItems::with) takes any iterator of values that convert into
//! [`ItemValue`]; [`with_items`](NoesisItems::with_items) takes an explicit,
//! possibly mixed list. For rows a `DataTemplate` binds by property name, use
//! [`with_objects`](NoesisItems::with_objects).
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use noesis_bevy::NoesisItems;
//! # fn fill(mut commands: Commands, view: Entity) {
//! commands.entity(view).insert(
//!     NoesisItems::new()
//!         .with("QualityCombo", ["Low", "Medium", "High"])
//!         .with("PortList", [80, 443, 8080])
//!         .select("PortList", 1),
//! );
//! # }
//! ```
//!
//! # Selection and current item
//!
//! [`select`](NoesisItems::select) sets a control's `SelectedIndex`, and
//! [`navigate`](NoesisItems::navigate) moves the current item of the list's
//! default `ICollectionView` with a [`CollectionViewOp`]. When a control's item
//! count, selected index or current item changes, including from user input,
//! the bridge writes a [`NoesisItemsCurrent`] message with the current item
//! read back from Noesis as an [`ItemValue`].
//!
//! Sorting, filtering and grouping are not available; see
//! [`noesis_runtime::collection_view`].

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::binding::ObservableCollection;
use noesis_runtime::classes::{
    ClassBuilder, ClassInstance, ClassRegistration, Instance, PropertyChangeHandler, PropertyValue,
};
use noesis_runtime::collection_view::{CollectionView, CollectionViewSource, CurrentItem};
use noesis_runtime::ffi::{ClassBase, PropType};
use noesis_runtime::view::FrameworkElement;

use crate::render::{NoesisRenderState, NoesisSet, ReapOnRemove, add_bridge_reap};

/// One list item, boxed in Noesis as the matching primitive type.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemValue {
    /// A string item.
    Str(String),
    /// A 32-bit integer item.
    I32(i32),
    /// A 64-bit float item.
    F64(f64),
    /// A boolean item.
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

    fn prop_type(&self) -> PropType {
        match self {
            Self::Str(_) => PropType::String,
            Self::I32(_) => PropType::Int32,
            Self::F64(_) => PropType::Double,
            Self::Bool(_) => PropType::Bool,
        }
    }

    fn set_on(&self, instance: Instance, index: u32) {
        match self {
            Self::Str(v) => instance.set_string(index, v),
            Self::I32(v) => instance.set_int32(index, *v),
            Self::F64(v) => instance.set_double(index, *v),
            Self::Bool(v) => instance.set_bool(index, *v),
        }
    }
}

/// One bindable object item as `(property name, value)` pairs. Each pair
/// becomes a dependency property on a generated Noesis class, so a
/// `DataTemplate` can bind `{Binding <name>}` against it.
pub type ObjectRow = Vec<(String, ItemValue)>;

/// Bindable object items for one list control, plus the Noesis class name to
/// register them under.
///
/// The class is registered the first time the list gets a non-empty source,
/// with property names and types taken from that first row. Later fields not
/// in that schema are ignored, and the class name and schema stay fixed until
/// the list is removed. `class_name` must not collide with another registered
/// Noesis class; if registration fails the items aren't applied and a warning
/// is logged.
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectSource {
    /// Noesis class name registered for these item objects.
    pub class_name: String,
    /// One entry per item; each is the item's `(property, value)` fields.
    pub rows: Vec<ObjectRow>,
}

/// Item properties are written once at construction, so changes need no forwarding.
struct NoopChangeHandler;

impl PropertyChangeHandler for NoopChangeHandler {
    fn on_changed(&self, _instance: Instance, _prop_index: u32, _value: PropertyValue<'_>) {}
}

/// `None` if the item is not a boxed primitive. The boxed types are mutually
/// exclusive, so probe order doesn't matter.
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

/// A move of the current item in a list's default `ICollectionView`
/// (`ICollectionView::MoveCurrentTo*`).
///
/// `First`, `Last` and `To` are absolute. `Next` and `Previous` step from the
/// current position each time they are applied, and [`NoesisItems`] applies
/// its op on every change to the component.
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
    /// Returns the raw Noesis result; query the position for the actual outcome.
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

/// Items, selection and navigation for list controls, keyed by `x:Name`.
///
/// Add it to a [`NoesisView`](crate::NoesisView) camera entity; it has no
/// effect on a [`UiPanel`](crate::UiPanel). Changing a list replaces the
/// control's items without rebuilding the view. Removing a name stops
/// updating that control, and removing the component detaches every list it
/// bound. Unknown names and controls that aren't `ItemsControl`s log a warning.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisItems {
    /// Primitive items per `x:Name`.
    pub sources: HashMap<String, Vec<ItemValue>>,
    /// `SelectedIndex` per `x:Name` (`-1` clears the selection). Pushed when the
    /// value changes, when the list's items are replaced, and after a scene
    /// rebuild. The user can still change the selection in between.
    pub select: HashMap<String, i32>,
    /// Current-item move per `x:Name`, applied once on every change to the
    /// component (so `Next` steps again even when an unrelated field changed).
    /// When both are set, it runs after [`select`](field@Self::select) and wins.
    pub navigate: HashMap<String, CollectionViewOp>,
    /// Object items per `x:Name`, for lists whose `DataTemplate` binds item
    /// properties (`{Binding Name}`). A name in both this and [`Self::sources`]
    /// uses the object items and logs a warning.
    pub objects: HashMap<String, ObjectSource>,
}

impl NoesisItems {
    /// An empty component. Build it up with [`with`](Self::with),
    /// [`select`](Self::select) and the other builders.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `name`'s items from any iterator of `&str`, `String`, `i32`, `f64`
    /// or `bool`.
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

    /// Sets `name`'s items from an explicit, possibly mixed, [`ItemValue`] list.
    #[must_use]
    pub fn with_items(
        mut self,
        name: impl Into<String>,
        items: impl IntoIterator<Item = ItemValue>,
    ) -> Self {
        self.sources
            .insert(name.into(), items.into_iter().collect());
        self
    }

    /// Sets `name`'s `SelectedIndex` (`-1` clears). See the [`select`](field@Self::select) field.
    #[must_use]
    pub fn select(mut self, name: impl Into<String>, index: i32) -> Self {
        self.select.insert(name.into(), index);
        self
    }

    /// Moves `name`'s current item. See the [`navigate`](field@Self::navigate) field.
    #[must_use]
    pub fn navigate(mut self, name: impl Into<String>, op: CollectionViewOp) -> Self {
        self.navigate.insert(name.into(), op);
        self
    }

    /// Sets `name`'s items to bindable objects of the Noesis class
    /// `class_name`. Each row is one item's `(property, value)` fields; see
    /// [`ObjectSource`] for how the class schema is fixed.
    #[must_use]
    pub fn with_objects(
        mut self,
        name: impl Into<String>,
        class_name: impl Into<String>,
        rows: Vec<ObjectRow>,
    ) -> Self {
        self.objects.insert(
            name.into(),
            ObjectSource {
                class_name: class_name.into(),
                rows,
            },
        );
        self
    }

    /// In-place form of [`with`](Self::with), for a system holding
    /// `&mut NoesisItems`.
    pub fn set(
        &mut self,
        name: impl Into<String>,
        items: impl IntoIterator<Item = impl Into<ItemValue>>,
    ) {
        self.sources
            .insert(name.into(), items.into_iter().map(Into::into).collect());
    }

    /// In-place form of [`with_items`](Self::with_items).
    pub fn set_items(
        &mut self,
        name: impl Into<String>,
        items: impl IntoIterator<Item = ItemValue>,
    ) {
        self.sources
            .insert(name.into(), items.into_iter().collect());
    }

    /// In-place form of [`select`](Self::select()).
    pub fn set_selection(&mut self, name: impl Into<String>, index: i32) {
        self.select.insert(name.into(), index);
    }

    /// In-place form of [`navigate`](Self::navigate()).
    pub fn set_navigation(&mut self, name: impl Into<String>, op: CollectionViewOp) {
        self.navigate.insert(name.into(), op);
    }

    /// In-place form of [`with_objects`](Self::with_objects).
    pub fn set_objects(
        &mut self,
        name: impl Into<String>,
        class_name: impl Into<String>,
        rows: Vec<ObjectRow>,
    ) {
        self.objects.insert(
            name.into(),
            ObjectSource {
                class_name: class_name.into(),
                rows,
            },
        );
    }
}

/// The Noesis collection behind one [`NoesisItems`] list, with a collection
/// view over it for current-item read-back.
///
/// Public for tests that drive the collection directly. Apps use
/// [`NoesisItems`].
pub struct ItemsBinding {
    coll: ObservableCollection,
    // Declared after `coll` so it drops first; it holds a ref to `coll`.
    cvs: CollectionViewSource,
    // Held for the binding's lifetime: dropping it lets Noesis rebuild the view
    // on the next `GetView`, resetting the current position.
    view: Option<CollectionView>,
    bound_for_uri: Option<String>,
    // Skips an unchanged source: a clear resets the control's selection and scroll.
    // `None` after any other mutation of `coll`.
    applied_typed: Option<Vec<ItemValue>>,
    applied_objects: Option<ObjectSource>,
    desired_select: Option<i32>,
    applied_select: Option<i32>,
    desired_nav: Option<CollectionViewOp>,
    // Re-armed on every component change so `Next`/`Previous` step again.
    nav_pending: bool,
    last_readback: Option<(usize, i32, i32, Option<ItemValue>)>,
    // Declared before `obj_registration`: instances must release before the
    // class unregisters.
    obj_instances: Vec<ClassInstance>,
    obj_schema: Vec<String>,
    // Declared last so it drops after `coll` and `obj_instances` release their refs.
    obj_registration: Option<ClassRegistration>,
}

impl Default for ItemsBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl ItemsBinding {
    /// An empty, unbound collection.
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
            applied_typed: None,
            applied_objects: None,
            desired_select: None,
            applied_select: None,
            desired_nav: None,
            nav_pending: false,
            last_readback: None,
            obj_instances: Vec::new(),
            obj_schema: Vec::new(),
            obj_registration: None,
        }
    }

    /// Replaces the whole list with string items.
    pub fn set<I, S>(&mut self, items: I)
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        self.coll.clear();
        for item in items {
            self.coll.push_string(item.as_ref());
        }
        self.applied_typed = None;
        self.applied_objects = None;
        self.applied_select = None;
    }

    /// Replaces the whole list with typed items. Does nothing when `items` equals
    /// the last call's, so the control keeps its selection and scroll.
    pub fn set_typed(&mut self, items: &[ItemValue]) {
        if self.applied_typed.as_deref() == Some(items) {
            return;
        }
        self.coll.clear();
        for item in items {
            item.push_into(&mut self.coll);
        }
        self.applied_typed = Some(items.to_vec());
        self.applied_objects = None;
        self.applied_select = None;
    }

    /// Replaces the whole list with object items, registering the class on first
    /// non-empty use (see [`ObjectSource`]).
    pub(crate) fn set_objects(&mut self, src: &ObjectSource) {
        if self.applied_objects.as_ref() == Some(src) {
            return;
        }
        if self.obj_registration.is_none() {
            let Some(first) = src.rows.first() else {
                self.coll.clear();
                self.obj_instances.clear();
                self.applied_typed = None;
                self.applied_objects = Some(src.clone());
                self.applied_select = None;
                return;
            };
            let mut builder =
                ClassBuilder::new(&src.class_name, ClassBase::Freezable, NoopChangeHandler);
            let mut schema = Vec::with_capacity(first.len());
            for (name, value) in first {
                builder.add_property(name, value.prop_type());
                schema.push(name.clone());
            }
            match builder.register() {
                Some(reg) => {
                    self.obj_registration = Some(reg);
                    self.obj_schema = schema;
                }
                None => {
                    warn!(
                        "NoesisItems: failed to register item class {:?} (duplicate name?)",
                        src.class_name,
                    );
                    return;
                }
            }
        }
        let Some(reg) = self.obj_registration.as_ref() else {
            return;
        };
        self.coll.clear();
        self.obj_instances.clear();
        for row in &src.rows {
            let Some(instance) = reg.create_instance() else {
                continue;
            };
            let handle = instance.handle();
            for (name, value) in row {
                if let Some(index) = self.obj_schema.iter().position(|n| n == name) {
                    value.set_on(handle, index as u32);
                }
            }
            self.coll.push_object(&instance);
            self.obj_instances.push(instance);
        }
        self.applied_typed = None;
        self.applied_objects = Some(src.clone());
        self.applied_select = None;
    }

    /// Appends one string item.
    pub fn push(&mut self, item: &str) {
        self.coll.push_string(item);
        self.invalidate_applied();
    }

    /// Appends one typed item.
    pub fn push_value(&mut self, item: &ItemValue) {
        item.push_into(&mut self.coll);
        self.invalidate_applied();
    }

    /// Removes the item at `index`; out of range does nothing.
    pub fn remove_at(&mut self, index: usize) {
        self.coll.remove_at(index);
        self.invalidate_applied();
    }

    /// Empties the list.
    pub fn clear(&mut self) {
        self.coll.clear();
        self.invalidate_applied();
    }

    fn invalidate_applied(&mut self) {
        self.applied_typed = None;
        self.applied_objects = None;
    }

    /// The backing collection, for
    /// [`FrameworkElement::set_items_source`](noesis_runtime::view::FrameworkElement::set_items_source).
    #[must_use]
    pub fn collection(&self) -> &ObservableCollection {
        &self.coll
    }

    pub(crate) fn set_desired_select(&mut self, index: Option<i32>) {
        if self.desired_select != index {
            self.desired_select = index;
            self.applied_select = None;
        }
    }

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

    /// Called from scene teardown so the next pass binds to the rebuilt scene.
    pub(crate) fn reset_bind(&mut self) {
        self.bound_for_uri = None;
        self.applied_select = None;
    }

    /// Re-fetched lazily if construction produced no view.
    fn view(&mut self) -> Option<&CollectionView> {
        if self.view.is_none() {
            self.view = self.cvs.view();
        }
        self.view.as_ref()
    }

    /// Pushes the desired index onto the control and the view's current item,
    /// once per change.
    pub(crate) fn drive_selection(&mut self, element: &mut FrameworkElement) {
        let Some(index) = self.desired_select else {
            return;
        };
        if self.applied_select == Some(index) {
            return;
        }
        let ok = element.set_selected_index(index);
        if let Some(view) = self.view() {
            view.move_current_to_position(index);
        }
        if ok {
            self.applied_select = Some(index);
        }
    }

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

    /// Applies `op` to the current item and returns the raw Noesis result, or
    /// `false` when there is no view. Read [`Self::current_position`] for the
    /// outcome.
    pub fn navigate(&mut self, op: CollectionViewOp) -> bool {
        self.view().is_some_and(|view| op.apply(view))
    }

    /// The current item's index: `-1` before the first item or when there is no
    /// view, `count` after the last.
    #[must_use]
    pub fn current_position(&mut self) -> i32 {
        self.view().map_or(-1, CollectionView::current_position)
    }

    /// The current item as an [`ItemValue`], or `None` when the position is
    /// off either end or the item is an object.
    #[must_use]
    pub fn current_item_value(&mut self) -> Option<ItemValue> {
        self.view()
            .and_then(CollectionView::current_item)
            .and_then(|item| current_item_value(&item))
    }

    /// `(count, selected_index, current_position, current)`, or `None` when
    /// unchanged since the last call.
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

/// A [`NoesisItems`] list control's count, selection or current item changed,
/// whether from the component or from user input. The first message for a
/// list arrives once its control is found in the scene.
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
    /// The current item, or `None` when the position is off either end or the
    /// item is an object.
    pub current: Option<ItemValue>,
}

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
            &items.objects,
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

impl ReapOnRemove for NoesisItems {
    fn reap(state: &mut NoesisRenderState, entity: Entity) {
        state.reap_items_for(entity);
    }
}

/// Runs the [`NoesisItems`] bridge in [`NoesisSet::Apply`] and registers
/// [`NoesisItemsCurrent`]. [`NoesisPlugin`](crate::NoesisPlugin) adds it.
pub struct NoesisItemsPlugin;

impl Plugin for NoesisItemsPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisItemsCurrent>()
            .add_systems(PostUpdate, sync_items_bridge.in_set(NoesisSet::Apply));
        add_bridge_reap::<NoesisItems>(app);
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

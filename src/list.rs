//! List controls whose rows are Bevy entities.
//!
//! Spawn a [`UiList`] entity naming a list control in a
//! [`NoesisView`]'s scene, then spawn row entities carrying a registered row
//! component and a [`ListedIn`] pointing at the list. Each row appears in the
//! control; despawn it and the row leaves. The bridge diffs the rows against
//! the bound collection every frame and applies only Add, Remove, Update and
//! Move operations, never a reset, so the control keeps its selection and
//! scroll position through edits.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use noesis_bevy::{ListedIn, NoesisListAppExt, NoesisViewModel, UiList};
//!
//! // Bound in the item template as `{Binding name}` and `{Binding qty}`.
//! #[derive(Component, NoesisViewModel)]
//! struct Item { name: String, qty: i32 }
//!
//! # fn register(app: &mut App) {
//! app.add_noesis_list::<Item>();
//! # }
//! # fn spawn(mut commands: Commands, view: Entity) {
//! // `view` is a NoesisView whose scene has a ListBox with x:Name="Inventory".
//! let list = commands.spawn(UiList::new(view, "Inventory")).id();
//! commands.spawn((Item { name: "Potion".into(), qty: 3 }, ListedIn(list)));
//! commands.spawn((Item { name: "Sword".into(), qty: 1 }, ListedIn(list)));
//! # }
//! ```
//!
//! # Rows
//!
//! - A row is keyed by its [`Entity`]. Changing a row component writes only
//!   the changed fields onto that row's existing item.
//! - Rows appear in the order their [`ListedIn`] was inserted, unless the list
//!   has a [`UiList::sorted_by`] key. Noesis-side sorting and filtering are not
//!   available.
//! - A pure reorder moves the fewest rows needed. When rows are added in the
//!   same frame, more rows may move than strictly necessary.
//! - Left-clicking a row raises [`UiClicked`](crate::UiClicked) targeting the
//!   row entity, with the list's `x:Name`.
//!
//! # Selection
//!
//! For a `Selector` control (`ListBox`, `ComboBox`, ...), the [`Selected`]
//! marker on a row entity mirrors the control's selected item:
//!
//! - When the user selects a row, the bridge moves [`Selected`] to it and
//!   writes a [`NoesisListSelection`] message and a [`NoesisRowSelected`]
//!   event. Removing the selected row also reports a cleared selection.
//! - When you insert or remove [`Selected`], the bridge sets the control's
//!   `SelectedIndex` to match, without echoing a message.
//! - If both change in the same frame, the user's selection wins.
//!
//! For a plain `ItemsControl`, [`Selected`] is yours alone; the bridge never
//! sets or reads it.
//!
//! # Scheduling
//!
//! Each registered row type gets a diff system in [`NoesisListSet::Diff`] that
//! touches no Noesis state, so it isn't pinned to the main thread. One system in
//! [`NoesisSet::Apply`] then applies the result to Noesis. A list's binding
//! is released when its [`UiList`] is removed or despawned, and list entities
//! are despawned with their view.

use std::collections::HashSet;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

use bevy::ecs::component::Mutable;
use bevy::ecs::relationship::RelationshipTarget;
use bevy::prelude::*;
use indexmap::IndexMap;
use noesis_runtime::binding::ObservableCollection;
use noesis_runtime::classes::{
    ClassBuilder, ClassInstance, ClassRegistration, Instance, PropertyChangeHandler, PropertyValue,
};
use noesis_runtime::ffi::{ClassBase, PropType};
use noesis_runtime::view::FrameworkElement;

use crate::plain_vm::{NoesisViewModel, PlainType, PlainValue};
use crate::render::{NoesisRenderState, NoesisSet, NoesisView, ReapOnRemove, add_bridge_reap};

/// Hidden trailing `u64` row property holding [`Entity::to_bits`]; the row
/// click handler reads it back from the clicked element's `DataContext`.
pub(crate) const ENTITY_FIELD: &str = "__entity";

/// Makes this entity a row of the [`UiList`] entity it points at (not the view).
///
/// Insert it together with a row component registered with
/// [`add_noesis_list`](NoesisListAppExt::add_noesis_list). Despawn the entity
/// or remove this component and the row leaves the list. This is a Bevy
/// relationship; [`ListRows`] on the list entity is its target.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
#[relationship(relationship_target = ListRows)]
pub struct ListedIn(
    /// The [`UiList`] entity this row belongs to.
    #[entities]
    pub Entity,
);

/// The rows [`ListedIn`] a [`UiList`] entity, in insertion order. Bevy
/// maintains it; edit [`ListedIn`] on the rows instead.
///
/// Despawning the list entity doesn't despawn its rows; Bevy removes their
/// [`ListedIn`].
#[derive(Component, Default, Debug)]
#[relationship_target(relationship = ListedIn)]
pub struct ListRows(Vec<Entity>);

/// Marks the selected row of a [`UiList`]. Insert or remove it to drive the
/// control's selection; the bridge moves it when the user selects. See
/// [Selection](self#selection).
///
/// If several rows of one list carry it, the lowest `Entity` wins and a
/// warning is logged.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Selected;

/// Sort key for a [`UiList`], set with [`UiList::sorted_by`].
///
/// The sort is stable, so equal keys keep insertion order. Values of
/// different types, `Null`s and NaNs compare equal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListSort {
    /// Index into the row type's [`NoesisViewModel::noesis_properties`]. An
    /// out-of-range index leaves the order unchanged.
    pub field: u32,
    /// Sort descending instead of ascending.
    pub descending: bool,
}

// Noesis class names are process-global, so every list needs its own.
static LIST_CLASS_SEQ: AtomicU64 = AtomicU64::new(0);

/// Binds the `ItemsControl` named `name` in `view`'s scene to the rows
/// [`ListedIn`] this entity.
///
/// Spawn one `UiList` entity per control; a view can host any number of them.
/// It works only on a [`NoesisView`], not a [`UiPanel`](crate::UiPanel). The
/// list despawns with its view. An unknown name or a control that isn't an
/// `ItemsControl` logs a warning.
///
/// The row objects are instances of a generated Noesis class whose properties
/// are the row type's [`NoesisViewModel::noesis_properties`], so an item
/// template binds them with `{Binding <field>}`. The class is registered once,
/// when the first row appears, and its name and layout are fixed from then on.
///
/// A list holds one row type. Rows of two different registered types in one
/// list are unsupported: a debug build panics and a release build warns once.
#[derive(Component, Clone, Debug)]
#[require(ListDesired)]
pub struct UiList {
    /// The [`NoesisView`] entity whose scene hosts the control.
    pub view: Entity,
    /// `x:Name` of the list control.
    pub name: String,
    /// Noesis class name for the row objects. See [`with_class`](Self::with_class).
    pub class: String,
    /// Row order, or `None` for [`ListedIn`] insertion order.
    pub sort: Option<ListSort>,
}

impl UiList {
    /// A list bound to the control `name` in `view`'s scene, with a unique
    /// generated row class name (`DmList.{n}`).
    #[must_use]
    pub fn new(view: Entity, name: impl Into<String>) -> Self {
        let seq = LIST_CLASS_SEQ.fetch_add(1, Ordering::Relaxed);
        Self {
            view,
            name: name.into(),
            class: format!("DmList.{seq}"),
            sort: None,
        }
    }

    /// Replaces the generated row class name, for a `DataTemplate` keyed on a
    /// specific `DataType`. The name must be unique among registered Noesis
    /// classes; on a collision an error is logged and no rows appear.
    #[must_use]
    pub fn with_class(mut self, class: impl Into<String>) -> Self {
        self.class = class.into();
        self
    }

    /// Orders rows by the field at index `field` of the row type's
    /// [`NoesisViewModel::noesis_properties`]. Reorders move the affected rows
    /// without resetting the list, so the selection survives.
    #[must_use]
    pub fn sorted_by(mut self, field: u32, descending: bool) -> Self {
        self.sort = Some(ListSort { field, descending });
        self
    }
}

/// A list's selection changed on the UI side: the user selected a row, or the
/// selected row was removed. [`Selected`] already matches when you read it.
/// Changing [`Selected`] yourself doesn't produce one.
#[derive(Message, Debug, Clone)]
pub struct NoesisListSelection {
    /// The [`NoesisView`] entity hosting the list.
    pub view: Entity,
    /// `x:Name` of the list control.
    pub list: String,
    /// The newly selected row entity, or `None` when the selection cleared.
    pub selected: Option<Entity>,
}

/// Entity event targeting the row the user selected, triggered alongside
/// [`NoesisListSelection`]. Observe it on the row entity or globally. A cleared
/// selection has no row to target and produces only the message.
#[derive(EntityEvent, Debug, Clone)]
pub struct NoesisRowSelected {
    /// The selected row entity (the event target).
    pub entity: Entity,
    /// The [`NoesisView`] entity hosting the list.
    pub view: Entity,
    /// `x:Name` of the list control.
    pub list: String,
}

/// Counts of the collection operations a list applied this frame, written only
/// when at least one is non-zero. Mostly useful in tests and diagnostics.
#[derive(Message, Debug, Clone)]
pub struct NoesisListOps {
    /// The [`NoesisView`] entity hosting the list.
    pub view: Entity,
    /// `x:Name` of the list control.
    pub list: String,
    /// Rows inserted this frame (`Add`).
    pub adds: usize,
    /// Rows removed this frame (`Remove`).
    pub removes: usize,
    /// Existing rows with at least one field rewritten this frame (`Update`).
    pub updates: usize,
    /// Existing rows moved this frame (`Move`).
    pub moves: usize,
}

/// `fields` is the row's [`NoesisViewModel::noesis_snapshot`] plus the
/// trailing [`ENTITY_FIELD`] value.
#[derive(Clone, Debug)]
pub(crate) struct DesiredRow {
    pub(crate) entity: Entity,
    pub(crate) fields: Vec<PlainValue>,
}

/// Written by [`diff_list`] each frame and read by [`sync_lists`]; holds no
/// Noesis handles so the diff can run in parallel.
#[derive(Component, Default)]
pub(crate) struct ListDesired {
    pub(crate) rows: Vec<DesiredRow>,
    pub(crate) schema: &'static [(&'static str, PlainType)],
    pub(crate) selected: Option<Entity>,
    /// Row type that owns this slot. Two types targeting one list race here, so
    /// it is stamped to detect that.
    pub(crate) row_type: Option<core::any::TypeId>,
}

/// Row properties are only written from Rust, so changes need no forwarding.
struct NoopRowHandler;

impl PropertyChangeHandler for NoopRowHandler {
    fn on_changed(&self, _instance: Instance, _prop_index: u32, _value: PropertyValue<'_>) {}
}

struct RowSlot {
    instance: ClassInstance,
    last_fields: Vec<PlainValue>,
}

pub(crate) enum SelectionOutcome {
    /// No UI-side change; the app may have driven the selection.
    Unchanged,
    /// The UI changed the selection; mirror it onto [`Selected`].
    UiSelected(Option<Entity>),
}

#[derive(Default, Clone, Copy)]
pub(crate) struct ListOps {
    pub(crate) adds: usize,
    pub(crate) removes: usize,
    pub(crate) updates: usize,
    pub(crate) moves: usize,
}

impl ListOps {
    fn touched(self) -> bool {
        self.adds + self.removes + self.updates + self.moves > 0
    }
}

/// One `(view, x:Name)` list's collection. `rows` is kept in the same order as
/// `coll`.
///
/// Field order is drop order: `coll`, then `rows`, then `registration`, so the
/// class unregisters only after every instance is released.
pub(crate) struct ListBinding {
    coll: ObservableCollection,
    /// Resolved when the `ItemsSource` binds; selection is read and driven on it.
    control: Option<FrameworkElement>,
    /// `false` for a plain `ItemsControl`: [`Selected`] is then left to the app.
    is_selector: bool,
    rows: IndexMap<Entity, RowSlot>,
    bound_for_uri: Option<String>,
    entity_field_index: u32,
    /// Set after the first registration attempt, even a failed one.
    class_ready: bool,
    /// Control selection as of the last poll or drive.
    last_currency: Option<Entity>,
    /// The first poll adopts the control's initial selection without reporting it.
    selection_primed: bool,
    registration: Option<ClassRegistration>,
}

impl Default for ListBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl ListBinding {
    pub(crate) fn new() -> Self {
        Self {
            coll: ObservableCollection::new(),
            control: None,
            is_selector: false,
            rows: IndexMap::new(),
            bound_for_uri: None,
            entity_field_index: 0,
            class_ready: false,
            last_currency: None,
            selection_primed: false,
            registration: None,
        }
    }

    /// Registers the row class from `schema` plus [`ENTITY_FIELD`]. Only the
    /// first call does anything, even if it failed.
    fn ensure_class(&mut self, class_name: &str, schema: &[(&'static str, PlainType)]) {
        if self.class_ready {
            return;
        }
        self.class_ready = true;
        let mut builder = ClassBuilder::new(class_name, ClassBase::Freezable, NoopRowHandler);
        for (name, kind) in schema {
            builder.add_property(name, plain_to_prop_type(*kind));
        }
        self.entity_field_index = schema.len() as u32;
        builder.add_property(ENTITY_FIELD, PropType::UInt64);
        match builder.register() {
            Some(reg) => self.registration = Some(reg),
            // Generated names never collide, so this is a duplicate `with_class`.
            None => error!(
                "UiList: failed to register row class {class_name:?} \
                 (duplicate `with_class` name?); rows will not realize",
            ),
        }
    }

    pub(crate) fn reconcile_into(
        &mut self,
        class_name: &str,
        schema: &[(&'static str, PlainType)],
        desired: &[DesiredRow],
    ) -> ListOps {
        self.ensure_class(class_name, schema);
        self.reconcile(desired)
    }

    /// Applies Remove, then Update, then Add/Move to match `desired`. Never clears.
    fn reconcile(&mut self, desired: &[DesiredRow]) -> ListOps {
        let mut ops = ListOps::default();
        if self.registration.is_none() {
            return ops;
        }
        let desired_set: HashSet<Entity> = desired.iter().map(|d| d.entity).collect();

        // High index first so earlier indices stay valid. The collection releases
        // its ref before the slot drops ours.
        let stale: Vec<usize> = self
            .rows
            .keys()
            .enumerate()
            .filter(|(_, e)| !desired_set.contains(e))
            .map(|(i, _)| i)
            .collect();
        for i in stale.into_iter().rev() {
            self.coll.remove_at(i);
            self.rows.shift_remove_index(i);
            ops.removes += 1;
        }

        for dr in desired {
            if let Some(slot) = self.rows.get_mut(&dr.entity) {
                let handle = slot.instance.handle();
                let mut changed = false;
                for (idx, value) in dr.fields.iter().enumerate() {
                    let differs = slot
                        .last_fields
                        .get(idx)
                        .is_none_or(|old| !values_eq(old, value));
                    if differs {
                        set_field(handle, idx as u32, value);
                        changed = true;
                    }
                }
                if changed {
                    slot.last_fields = dr.fields.clone();
                    ops.updates += 1;
                }
            }
        }

        // Only the no-add path is a minimal Move set; with adds, rows may move
        // more than strictly needed.
        let has_adds = desired.iter().any(|d| !self.rows.contains_key(&d.entity));
        if has_adds {
            self.place_with_adds(desired, &mut ops);
        } else {
            self.reorder_minimal(desired, &mut ops);
        }
        ops
    }

    /// Invariant: `rows[0..t]` equals `desired[0..t]` before step `t`, so each
    /// step is one insert or one move.
    fn place_with_adds(&mut self, desired: &[DesiredRow], ops: &mut ListOps) {
        for (t, dr) in desired.iter().enumerate() {
            if let Some(cur) = self.rows.get_index_of(&dr.entity) {
                if cur != t {
                    self.coll.move_item(cur, t);
                    self.rows.move_index(cur, t);
                    ops.moves += 1;
                }
            } else if let Some(slot) = self.realize(dr) {
                self.coll.insert_object(t, &slot.instance);
                self.rows.shift_insert(t, dr.entity, slot);
                ops.adds += 1;
            }
        }
    }

    /// Reorders a fixed row set with the fewest moves: rows on the longest
    /// increasing subsequence stay put. Right-to-left, so the suffix is settled
    /// and each unanchored row is in the prefix, landing with one move.
    fn reorder_minimal(&mut self, desired: &[DesiredRow], ops: &mut ListOps) {
        let n = desired.len();
        if n < 2 {
            return;
        }
        // seq[i] = desired index of the row currently at collection position i.
        let mut desired_pos = std::collections::HashMap::with_capacity(n);
        for (i, dr) in desired.iter().enumerate() {
            desired_pos.insert(dr.entity, i);
        }
        let cur: Vec<Entity> = self.rows.keys().copied().collect();
        let seq: Vec<usize> = cur.iter().map(|e| desired_pos[e]).collect();
        let anchored_positions = longest_increasing_subsequence(&seq);
        let anchored: HashSet<Entity> = anchored_positions.iter().map(|&i| cur[i]).collect();

        for t in (0..n).rev() {
            let entity = desired[t].entity;
            if anchored.contains(&entity) {
                continue;
            }
            let cur = self.rows.get_index_of(&entity).expect("survivor present");
            if cur != t {
                self.coll.move_item(cur, t);
                self.rows.move_index(cur, t);
                ops.moves += 1;
            }
        }
    }

    /// `None` if the class isn't registered or instantiation failed.
    fn realize(&self, dr: &DesiredRow) -> Option<RowSlot> {
        let reg = self.registration.as_ref()?;
        let instance = reg.create_instance()?;
        let handle = instance.handle();
        for (idx, value) in dr.fields.iter().enumerate() {
            set_field(handle, idx as u32, value);
        }
        Some(RowSlot {
            instance,
            last_fields: dr.fields.clone(),
        })
    }

    pub(crate) fn collection(&self) -> &ObservableCollection {
        &self.coll
    }

    pub(crate) fn needs_bind(&self, uri: &str) -> bool {
        self.bound_for_uri.as_deref() != Some(uri)
    }

    pub(crate) fn mark_bound(&mut self, uri: &str) {
        self.bound_for_uri = Some(uri.to_owned());
    }

    /// Called on scene rebuild: the cached control belongs to the old scene, so
    /// the next bind re-resolves it and re-baselines selection.
    pub(crate) fn reset_bind(&mut self) {
        self.bound_for_uri = None;
        self.control = None;
        self.is_selector = false;
        self.last_currency = None;
        self.selection_primed = false;
    }

    /// Clears the control's `ItemsSource` so it releases the collection before
    /// this binding's `ClassRegistration` drops.
    pub(crate) fn detach(&mut self) {
        if let Some(control) = self.control.as_mut() {
            control.clear_items_source();
        }
    }

    /// `selected_index()` is `Some` only for a `Selector`, which is how a plain
    /// `ItemsControl` is detected.
    pub(crate) fn set_control(&mut self, control: FrameworkElement) {
        self.is_selector = control.selected_index().is_some();
        self.control = Some(control);
    }

    /// Matches the control's selected item to a row by pointer identity.
    fn current_entity(&self) -> Option<Entity> {
        let ptr: *mut c_void = self.control.as_ref()?.selected_item()?.as_ptr();
        self.rows
            .iter()
            .find(|(_, slot)| std::ptr::eq(slot.instance.raw(), ptr))
            .map(|(e, _)| *e)
    }

    /// A control-side change since the last poll is reported and wins; otherwise
    /// the control is driven to `desired_selected`. A `Selector` tracks the
    /// selected item, not its index, so Add, Remove and Move need no handling.
    pub(crate) fn poll_selection(&mut self, desired_selected: Option<Entity>) -> SelectionOutcome {
        if !self.is_selector {
            return SelectionOutcome::Unchanged;
        }
        let current = self.current_entity();
        if !self.selection_primed {
            // Falls through so an app-set selection still applies this frame.
            self.selection_primed = true;
            self.last_currency = current;
        } else if current != self.last_currency {
            self.last_currency = current;
            return SelectionOutcome::UiSelected(current);
        }
        if desired_selected != current {
            let index = match desired_selected {
                Some(e) => self.rows.get_index_of(&e).map_or(-1, |i| i as i32),
                None => -1,
            };
            if let Some(control) = self.control.as_mut() {
                let _ = control.set_selected_index(index);
            }
            // Read back, not `desired_selected`: a rejected drive would otherwise
            // look like a UI change next poll.
            self.last_currency = self.current_entity();
        }
        SelectionOutcome::Unchanged
    }
}

/// Longest strictly increasing subsequence, as ascending positions in `seq`.
fn longest_increasing_subsequence(seq: &[usize]) -> Vec<usize> {
    let n = seq.len();
    if n == 0 {
        return Vec::new();
    }
    // tails[k] = position (in seq) of the smallest tail of an increasing
    // subsequence of length k+1; prev links each position to its predecessor.
    let mut tails: Vec<usize> = Vec::new();
    let mut prev = vec![usize::MAX; n];
    for i in 0..n {
        let mut lo = 0usize;
        let mut hi = tails.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            if seq[tails[mid]] < seq[i] {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        if lo > 0 {
            prev[i] = tails[lo - 1];
        }
        if lo == tails.len() {
            tails.push(i);
        } else {
            tails[lo] = i;
        }
    }
    let mut out = Vec::with_capacity(tails.len());
    let mut k = *tails.last().expect("non-empty seq has a tail");
    loop {
        out.push(k);
        if prev[k] == usize::MAX {
            break;
        }
        k = prev[k];
    }
    out.reverse();
    out
}

fn plain_to_prop_type(kind: PlainType) -> PropType {
    match kind {
        PlainType::Int32 => PropType::Int32,
        PlainType::Double => PropType::Double,
        PlainType::Bool => PropType::Bool,
        PlainType::String => PropType::String,
        PlainType::U64 => PropType::UInt64,
        PlainType::BaseComponent => PropType::BaseComponent,
    }
}

/// `Null` leaves the property untouched.
fn set_field(handle: Instance, index: u32, value: &PlainValue) {
    match value {
        PlainValue::Int32(v) => handle.set_int32(index, *v),
        PlainValue::Double(v) => handle.set_double(index, *v),
        PlainValue::Bool(v) => handle.set_bool(index, *v),
        PlainValue::String(v) => handle.set_string(index, v),
        PlainValue::U64(v) => handle.set_u64(index, *v),
        PlainValue::Null => {}
    }
}

/// Change-cache equality; [`PlainValue`] isn't `PartialEq`.
fn values_eq(a: &PlainValue, b: &PlainValue) -> bool {
    match (a, b) {
        (PlainValue::Int32(x), PlainValue::Int32(y)) => x == y,
        // NaN equals NaN here, or an unchanged NaN field would Update every frame.
        (PlainValue::Double(x), PlainValue::Double(y)) => x == y || (x.is_nan() && y.is_nan()),
        (PlainValue::Bool(x), PlainValue::Bool(y)) => x == y,
        (PlainValue::String(x), PlainValue::String(y)) => x == y,
        (PlainValue::U64(x), PlainValue::U64(y)) => x == y,
        (PlainValue::Null, PlainValue::Null) => true,
        _ => false,
    }
}

/// Mixed variants, `Null` and NaN compare equal.
fn compare_values(a: &PlainValue, b: &PlainValue) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (PlainValue::Int32(x), PlainValue::Int32(y)) => x.cmp(y),
        (PlainValue::Double(x), PlainValue::Double(y)) => {
            x.partial_cmp(y).unwrap_or(Ordering::Equal)
        }
        (PlainValue::Bool(x), PlainValue::Bool(y)) => x.cmp(y),
        (PlainValue::String(x), PlainValue::String(y)) => x.cmp(y),
        (PlainValue::U64(x), PlainValue::U64(y)) => x.cmp(y),
        _ => Ordering::Equal,
    }
}

/// System set for the per-row-type list diff systems.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum NoesisListSet {
    /// Reads rows into each list's desired state. Runs in `PostUpdate` before
    /// [`NoesisSet::Apply`], so row changes made before then show the same frame.
    Diff,
}

/// Snapshots each list's `T` rows in order and records its [`Selected`] row.
#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
fn diff_list<T: NoesisViewModel + Component>(
    lists: Query<(Entity, &UiList, Option<&ListRows>)>,
    rows: Query<(&T, Has<Selected>)>,
    mut desired: Query<&mut ListDesired>,
) {
    for (list_ent, list, list_rows) in &lists {
        let Ok(mut slot) = desired.get_mut(list_ent) else {
            continue;
        };

        // Bevy removes an emptied `ListRows`.
        let mut gathered: Vec<(Entity, Vec<PlainValue>, bool)> = list_rows
            .into_iter()
            .flat_map(RelationshipTarget::iter)
            .filter_map(|entity| {
                let (data, selected) = rows.get(entity).ok()?;
                let mut fields = data.noesis_snapshot();
                fields.push(PlainValue::U64(entity.to_bits()));
                Some((entity, fields, selected))
            })
            .collect();

        // Every registered `T` visits every list; a non-owning type with no rows
        // here must not clobber the owner's slot or freeze the class with its schema.
        let this = core::any::TypeId::of::<T>();
        if gathered.is_empty() && slot.row_type != Some(this) {
            continue;
        }

        if let Some(sort) = list.sort {
            let field = sort.field as usize;
            gathered.sort_by(|(_, a, _), (_, b, _)| {
                let ord = match (a.get(field), b.get(field)) {
                    (Some(x), Some(y)) => compare_values(x, y),
                    _ => std::cmp::Ordering::Equal,
                };
                if sort.descending { ord.reverse() } else { ord }
            });
        }

        // A different recorded type here means two types both hold rows in this list.
        if let Some(prev) = slot.row_type
            && prev != this
        {
            debug_assert!(
                false,
                "UiList {list_ent:?}: two row component types target one list \
                 (last-writer-wins); use one row type per UiList",
            );
            bevy::log::warn_once!(
                "UiList: multiple row component types target list {list_ent:?}; \
                 only one row type per UiList is supported (last-writer-wins)",
            );
        }

        // Stamped together: `sync_lists` registers the class once `row_type` is set.
        slot.schema = T::noesis_properties();
        slot.row_type = Some(this);

        // Lowest entity, not first found, so the winner doesn't flip between frames.
        let selected: Vec<Entity> = gathered
            .iter()
            .filter(|(_, _, selected)| *selected)
            .map(|(e, _, _)| *e)
            .collect();
        if selected.len() > 1 {
            bevy::log::warn_once!(
                "UiList {list_ent:?}: {} rows carry Selected; a list has one \
                 selection — driving the lowest entity",
                selected.len(),
            );
        }
        slot.selected = selected.into_iter().min();
        slot.rows = gathered
            .into_iter()
            .map(|(entity, fields, _)| DesiredRow { entity, fields })
            .collect();
    }
}

/// The only list system that touches Noesis.
#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
fn sync_lists(
    lists: Query<(Entity, &UiList, &ListDesired)>,
    alive_views: Query<(), With<NoesisView>>,
    selected_rows: Query<(Entity, &ListedIn), With<Selected>>,
    state: Option<NonSendMut<NoesisRenderState>>,
    click_queue: Res<crate::events::SharedClickQueue>,
    mut commands: Commands,
    mut ops_writer: MessageWriter<NoesisListOps>,
    mut sel_writer: MessageWriter<NoesisListSelection>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (list_ent, list, desired) in &lists {
        // No rows yet: registering now would freeze an empty class layout.
        if desired.row_type.is_none() {
            continue;
        }
        // View teardown already reaped the binding; `apply_list_for` would recreate
        // it before `despawn_orphan_lists` takes this entity.
        if alive_views.get(list.view).is_err() {
            continue;
        }
        let (ops, selection) = state.apply_list_for(
            list_ent,
            list.view,
            &list.name,
            &list.class,
            desired.schema,
            &desired.rows,
            desired.selected,
            &click_queue,
        );
        if ops.touched() {
            ops_writer.write(NoesisListOps {
                view: list.view,
                list: list.name.clone(),
                adds: ops.adds,
                removes: ops.removes,
                updates: ops.updates,
                moves: ops.moves,
            });
        }
        if let SelectionOutcome::UiSelected(selected) = selection {
            // Commands apply in order, so re-selecting the same row leaves it marked.
            for (entity, listed) in &selected_rows {
                if listed.0 == list_ent {
                    commands.entity(entity).remove::<Selected>();
                }
            }
            if let Some(entity) = selected {
                commands.entity(entity).insert(Selected);
                commands.trigger(NoesisRowSelected {
                    entity,
                    view: list.view,
                    list: list.name.clone(),
                });
            }
            sel_writer.write(NoesisListSelection {
                view: list.view,
                list: list.name.clone(),
                selected,
            });
        }
    }
}

/// Despawns lists whose [`NoesisView`] was removed. The view teardown already
/// reaped their bindings, so the `UiList` removal reap is a no-op.
#[allow(clippy::needless_pass_by_value)]
fn despawn_orphan_lists(
    mut removed: RemovedComponents<NoesisView>,
    lists: Query<(Entity, &UiList)>,
    mut commands: Commands,
) {
    let gone: HashSet<Entity> = removed.read().collect();
    if gone.is_empty() {
        return;
    }
    for (list_ent, list) in &lists {
        if gone.contains(&list.view) {
            commands.entity(list_ent).despawn();
        }
    }
}

/// Registers list row types on an [`App`].
pub trait NoesisListAppExt {
    /// Registers `T` as a row type: entities with `T` and [`ListedIn`] become
    /// rows, and `T`'s [`NoesisViewModel`] fields become the row properties.
    /// Call it once per type.
    fn add_noesis_list<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
    ) -> &mut Self;
}

impl NoesisListAppExt for App {
    fn add_noesis_list<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
    ) -> &mut Self {
        self.add_systems(PostUpdate, diff_list::<T>.in_set(NoesisListSet::Diff));
        self
    }
}

impl ReapOnRemove for UiList {
    fn reap(state: &mut NoesisRenderState, entity: Entity) {
        state.reap_list_for(entity);
    }
}

/// Runs the [`UiList`] bridge and registers its messages.
/// [`NoesisPlugin`](crate::NoesisPlugin) adds it; register row types with
/// [`NoesisListAppExt::add_noesis_list`].
#[derive(Default)]
pub struct NoesisListPlugin;

impl Plugin for NoesisListPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisListOps>();
        app.add_message::<NoesisListSelection>();
        app.configure_sets(PostUpdate, NoesisListSet::Diff.before(NoesisSet::Apply));
        app.add_systems(PostUpdate, sync_lists.in_set(NoesisSet::Apply));
        app.add_systems(PostUpdate, despawn_orphan_lists.in_set(NoesisSet::Ensure));
        add_bridge_reap::<UiList>(app);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lis_picks_longest_run() {
        // 2,3 are already increasing; the minimal-move anchor.
        let positions = longest_increasing_subsequence(&[2, 3, 1, 0]);
        let values: Vec<usize> = positions.iter().map(|&i| [2, 3, 1, 0][i]).collect();
        assert_eq!(values, vec![2, 3]);
    }

    #[test]
    fn lis_identity_anchors_everything() {
        let positions = longest_increasing_subsequence(&[0, 1, 2, 3]);
        assert_eq!(positions, vec![0, 1, 2, 3]);
    }

    #[test]
    fn lis_full_reverse_anchors_one() {
        let positions = longest_increasing_subsequence(&[3, 2, 1, 0]);
        assert_eq!(positions.len(), 1);
    }

    #[test]
    fn compare_orders_primitives() {
        use std::cmp::Ordering;
        assert_eq!(
            compare_values(&PlainValue::Int32(1), &PlainValue::Int32(2)),
            Ordering::Less,
        );
        assert_eq!(
            compare_values(
                &PlainValue::String("b".into()),
                &PlainValue::String("a".into())
            ),
            Ordering::Greater,
        );
    }

    #[test]
    fn ui_list_builder_sets_sort() {
        let list = UiList::new(Entity::PLACEHOLDER, "Inv").sorted_by(1, true);
        assert_eq!(list.name, "Inv");
        assert_eq!(
            list.sort,
            Some(ListSort {
                field: 1,
                descending: true
            })
        );
    }

    #[test]
    fn ui_list_auto_class_is_unique() {
        // Two lists of the "same" declaration get distinct auto-generated classes,
        // so two instances "just work" without hand-picked names.
        let a = UiList::new(Entity::PLACEHOLDER, "Inv");
        let b = UiList::new(Entity::PLACEHOLDER, "Inv");
        assert_ne!(
            a.class, b.class,
            "auto-generated row classes must be unique"
        );
        assert!(a.class.starts_with("DmList."), "got {:?}", a.class);

        let c = UiList::new(Entity::PLACEHOLDER, "Inv").with_class("Game.Row");
        assert_eq!(c.class, "Game.Row");
    }
}

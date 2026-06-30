//! **Primitive 2 — list = query.** The rows of a XAML list control *are* Bevy
//! entities: spawn an entity carrying a row-data component and a [`ListedIn`]
//! membership, and it appears as a row; despawn it and the row leaves. The bound
//! `ObservableCollection` is reconciled from the query keyed by [`Entity`],
//! emitting the **minimal** Add / Remove / Update / Move op sequence — never a
//! `Clear`/`Reset` in steady state — so a control's selection and scroll position
//! survive every edit.
//!
//! ```ignore
//! use bevy::prelude::*;
//! use noesis_bevy::{NoesisViewModel, NoesisListAppExt, UiList, ListedIn, Selected};
//!
//! // The row's bound fields: `{Binding name}` / `{Binding qty}` in the item template.
//! #[derive(Component, NoesisViewModel)]
//! struct Item { name: String, qty: i32 }
//!
//! fn setup(app: &mut App) {
//!     app.add_noesis_list::<Item>(); // register the row type once
//! }
//!
//! // …with a NoesisView entity `view` whose scene has an `x:Name="Inventory"` ListBox:
//! // commands.entity(view).insert(UiList::new("Inventory"));
//! // commands.spawn((Item { name: "Potion".into(), qty: 3 }, ListedIn(view)));
//! // commands.spawn((Item { name: "Sword".into(),  qty: 1 }, ListedIn(view)));
//! ```
//!
//! # The contract
//!
//! - **[`Entity`] is the stable key.** A row is identified by its entity, not its
//!   position or its field values. Mutating a row's component updates *only* that
//!   row's existing realized container (an in-place DP write, no collection op);
//!   adding / removing entities touches only the affected rows.
//! - **Row order = query order.** Rows appear in ECS iteration order, optionally
//!   re-ordered by a Rust-side [`UiList::sorted_by`] key. There is *no* live
//!   Noesis sort/filter (the SDK exposes none); ordering is entirely Rust-side and
//!   reconciled with `Move` ops, so a reorder keeps the moved container — and its
//!   selection — alive. "Reset is the enemy."
//! - **Currency *is* selection.** The control's `ICollectionView` current item is
//!   the single source of selection truth (no parallel channel). A UI selection
//!   surfaces as a [`Selected`] marker on the row entity (and a
//!   [`NoesisListSelection`] message); setting / clearing [`Selected`] from a
//!   system drives the current item the other way. Within a frame the **UI wins**
//!   (record-then-apply), so the two authorities never oscillate.
//!
//! # Threading & lifetime
//!
//! Mirrors [`crate::reconcile`]: a parallel `PostUpdate` diff system
//! ([`NoesisListSet::Diff`]) builds the desired ordered `Vec` of `(Entity, field
//! snapshot)` into a plain `Send` [`ListDesired`] component — no Noesis handles in
//! sight — and the single serial [`sync_lists`] system in
//! [`NoesisSet::Apply`](crate::NoesisSet::Apply) drains it through FFI against the
//! view's live `ObservableCollection`, which is owned by
//! [`NoesisRenderState`](crate::render) (thread-affine to the `View`) and released
//! before `noesis_runtime::shutdown`.
//!
//! One [`UiList`] declares one list per owning [`NoesisView`](crate::NoesisView)
//! entity, of one registered row type. Several lists = several owner entities (the
//! same "one instance = one entity" stance as [`crate::panel`]).

use std::collections::HashSet;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU64, Ordering};

use bevy::ecs::component::Mutable;
use bevy::prelude::*;
use indexmap::IndexMap;
use noesis_runtime::binding::ObservableCollection;
use noesis_runtime::classes::{
    ClassBuilder, ClassInstance, ClassRegistration, Instance, PropertyChangeHandler, PropertyValue,
};
use noesis_runtime::collection_view::{CollectionView, CollectionViewSource};
use noesis_runtime::ffi::{ClassBase, PropType};

use crate::plain_vm::{NoesisViewModel, PlainType, PlainValue};
use crate::render::{NoesisRenderState, NoesisSet};

/// Name of the hidden trailing `u64` row property that stores each row's stable
/// [`Entity`] bits (via [`Entity::to_bits`]). The per-row click handler recovers
/// the originating row from a clicked element's `DataContext` through this field
/// (see [`NoesisRenderState::install_row_click_sub`](crate::render)).
pub(crate) const ENTITY_FIELD: &str = "__entity";

// ─────────────────────────────────────────────────────────────────────────────
// Public components & messages
// ─────────────────────────────────────────────────────────────────────────────

/// Row membership: tags an entity into the list owned by `owner` (the
/// [`NoesisView`](crate::NoesisView) entity carrying the [`UiList`]). Spawn it
/// alongside a registered row-data component to make the entity a row; despawn the
/// entity (or remove this component) and the row leaves the list next frame.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListedIn(
    /// The list-owning [`NoesisView`](crate::NoesisView) entity.
    pub Entity,
);

/// Marker placed on the row entity that is currently selected in the bound
/// control. **Currency is selection**: the bridge sets / clears this from a UI
/// selection change, and an app may set / clear it to drive the selection the
/// other way (the current item moves to that row). At most one row per list
/// carries it in steady state.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Selected;

/// A Rust-side row ordering key for a [`UiList`]: sort by the row component's
/// field at `field` (its index in [`NoesisViewModel::noesis_properties`]),
/// ascending or `descending`. This is the *only* sanctioned reordering — Noesis
/// exposes no programmatic sort/filter — and it reconciles with `Move` ops, so the
/// selected row survives the reorder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ListSort {
    /// Property index (into [`NoesisViewModel::noesis_properties`]) to sort on.
    pub field: u32,
    /// Sort descending instead of ascending.
    pub descending: bool,
}

/// Process-global counter handing each [`UiList`] a unique row-class name. Noesis
/// registers reflected classes globally by name, so every list — even two of the
/// same row type on two views — needs a distinct class; an auto-generated token
/// guarantees that without the user inventing one. Mirrors `PANEL_CLASS_SEQ` in
/// [`crate::render`]. (The render path realizes rows via the control's
/// `ItemTemplate` / `{Binding <field>}` regardless of the class name; the name
/// only has to be unique, never meaningful — see [`UiList::with_class`].)
static LIST_CLASS_SEQ: AtomicU64 = AtomicU64::new(0);

/// Per-view list declaration: bind the `ObservableCollection` of entity-rows to
/// the `ItemsControl` / `ListBox` named `name` (`x:Name`). Add it to the
/// [`NoesisView`](crate::NoesisView) entity whose rows reference it via
/// [`ListedIn`].
///
/// Each list auto-generates a unique Noesis `class` for its row objects (so two
/// lists of the same row type "just work", no hand-picked names); the class is
/// registered once on first reconcile and held for the binding's lifetime. Its
/// properties are the row type's [`NoesisViewModel::noesis_properties`]; bind an
/// item `DataTemplate` against them with `{Binding <field>}`. Override the name
/// with [`with_class`](Self::with_class) only for a typed `DataTemplate` keyed on
/// a specific class.
#[derive(Component, Clone, Debug)]
#[require(ListDesired)]
pub struct UiList {
    /// `x:Name` of the list control to bind in the owner view's scene.
    pub name: String,
    /// Noesis class name the row objects register under. Auto-generated unique by
    /// [`new`](Self::new); override via [`with_class`](Self::with_class).
    pub class: String,
    /// Optional Rust-side row ordering (default: ECS query order).
    pub sort: Option<ListSort>,
}

impl UiList {
    /// Declare a list bound to the `x:Name` control `name`. The row-object class is
    /// auto-generated unique (`DmList.{seq}`), so nothing has to be globally
    /// hand-named. Rows appear in ECS query order; add
    /// [`sorted_by`](Self::sorted_by) for a Rust-side order, or
    /// [`with_class`](Self::with_class) to bind a typed `DataTemplate`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        let seq = LIST_CLASS_SEQ.fetch_add(1, Ordering::Relaxed);
        Self {
            name: name.into(),
            class: format!("DmList.{seq}"),
            sort: None,
        }
    }

    /// Override the auto-generated row-object class name. Only needed when the
    /// scene's `ItemTemplate` is a typed `DataTemplate` keyed on a specific class
    /// (`DataType="local:Foo"`); the default `{Binding <field>}` templates don't
    /// care about the name, only its uniqueness. The override must still be unique
    /// among registered Noesis classes.
    #[must_use]
    pub fn with_class(mut self, class: impl Into<String>) -> Self {
        self.class = class.into();
        self
    }

    /// Order rows by the row component's field at `field` (its index in
    /// [`NoesisViewModel::noesis_properties`]), `descending` or ascending.
    #[must_use]
    pub fn sorted_by(mut self, field: u32, descending: bool) -> Self {
        self.sort = Some(ListSort { field, descending });
        self
    }
}

/// Emitted when a bound list's selection (its `ICollectionView` current item)
/// changes **from the UI side** — the user clicked a row, or the cursor moved off
/// the ends. The bridge has already reconciled the [`Selected`] marker to match;
/// read this to react to a selection. App-driven selection (setting [`Selected`])
/// does *not* echo a message — it is the cause, not an effect.
#[derive(Message, Debug, Clone)]
pub struct NoesisListSelection {
    /// The list-owning [`NoesisView`](crate::NoesisView) entity.
    pub view: Entity,
    /// `x:Name` of the list control.
    pub list: String,
    /// The newly-selected row entity, or `None` when the selection cleared.
    pub selected: Option<Entity>,
}

/// Emitted each frame a list's reconcile actually touched the collection, with the
/// minimal op tally for that frame. There is deliberately no "reset" field — the
/// reconciler has no `Clear` path in steady state. Primarily a test / diagnostic
/// surface (assert `moves > 0` on a reorder, `adds`/`removes` on membership
/// change, and that a pure field edit produces only `updates`).
#[derive(Message, Debug, Clone)]
pub struct NoesisListOps {
    /// The list-owning [`NoesisView`](crate::NoesisView) entity.
    pub view: Entity,
    /// `x:Name` of the list control.
    pub list: String,
    /// Rows realized + inserted this frame (`Add`).
    pub adds: usize,
    /// Rows removed + released this frame (`Remove`).
    pub removes: usize,
    /// Surviving rows whose fields changed in place this frame (`Update`).
    pub updates: usize,
    /// Surviving rows relocated this frame (`Move`).
    pub moves: usize,
}

// ─────────────────────────────────────────────────────────────────────────────
// Send-side desired state (the parallel "diff" half)
// ─────────────────────────────────────────────────────────────────────────────

/// One desired row computed by the parallel diff: its stable [`Entity`] key and
/// the field snapshot to push (the row type's [`NoesisViewModel::noesis_snapshot`],
/// with the entity's 64-bit identity appended as the hidden trailing field).
#[derive(Clone, Debug)]
pub(crate) struct DesiredRow {
    pub(crate) entity: Entity,
    pub(crate) fields: Vec<PlainValue>,
}

/// The parallel→serial hand-off for one view's list: the desired ordered rows,
/// the row schema, and which row (if any) the app marked [`Selected`]. A plain
/// `Send` component (auto-required by [`UiList`]); holds no Noesis handles. Rebuilt
/// every frame by [`diff_list`] and drained by [`sync_lists`].
#[derive(Component, Default)]
pub(crate) struct ListDesired {
    /// Desired rows in final order (query order, optionally sorted Rust-side).
    pub(crate) rows: Vec<DesiredRow>,
    /// Row property schema (`(name, type)`), set from the row type's metadata. The
    /// reconciler appends a hidden `u64` entity-identity field after these.
    pub(crate) schema: &'static [(&'static str, PlainType)],
    /// The row currently carrying [`Selected`] (app-side selection authority).
    pub(crate) selected: Option<Entity>,
}

// ─────────────────────────────────────────────────────────────────────────────
// Render-side binding (the serial "push" half — NonSend)
// ─────────────────────────────────────────────────────────────────────────────

/// No-op property-change handler for row objects: their dependency properties are
/// written from Rust (never edited UI-side back into the field — selection rides
/// currency, not a DP writeback), so changes need no forwarding.
struct NoopRowHandler;

impl PropertyChangeHandler for NoopRowHandler {
    fn on_changed(&self, _instance: Instance, _prop_index: u32, _value: PropertyValue<'_>) {}
}

/// One realized row: its owning [`ClassInstance`] (`+1` ref) and the last field
/// values pushed onto it, so an unchanged field skips its DP write.
struct RowSlot {
    instance: ClassInstance,
    last_fields: Vec<PlainValue>,
}

/// What a selection poll concluded for one frame.
pub(crate) enum SelectionOutcome {
    /// No UI-side selection change to report (selection idle, or it was the app
    /// driving currency this frame).
    Unchanged,
    /// The UI moved the current item this frame; reconcile [`Selected`] to this
    /// row (or clear it for `None`) and emit a [`NoesisListSelection`].
    UiSelected(Option<Entity>),
}

/// The minimal op tally a single reconcile produced (see [`NoesisListOps`]).
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

/// One list's Rust-owned, entity-keyed `ObservableCollection`, owned per `(view,
/// x:Name)` by [`NoesisRenderState`](crate::render). Maintains an
/// insertion-ordered [`IndexMap<Entity, RowSlot>`] whose order mirrors the live
/// collection, and reconciles it against the desired rows with minimal ops.
///
/// **Field/drop order matters.** `coll` drops first (releasing the collection's
/// refs to the row instances), then `rows` (releasing our `+1` per instance),
/// and `registration` **last** (unregistering the class only once every instance
/// of it is gone) — the Noesis refcount rule mirrored from
/// [`crate::items::ItemsBinding`].
pub(crate) struct ListBinding {
    /// Backing collection bound as the control's `ItemsSource`.
    coll: ObservableCollection,
    /// Source of the `ICollectionView` over `coll`; drops after `coll`.
    cvs: CollectionViewSource,
    /// Cached `ICollectionView`, held for the binding's lifetime so currency
    /// (selection) is not reset by a fresh `GetView`.
    view: Option<CollectionView>,
    /// Realized rows in collection order. Drops after `coll`, before
    /// `registration`.
    rows: IndexMap<Entity, RowSlot>,
    /// The URI of the scene we last bound the `ItemsSource` into (`None` until
    /// bound; reset on a scene rebuild to force a re-bind).
    bound_for_uri: Option<String>,
    /// DP index of the hidden trailing `u64` entity-identity field.
    entity_field_index: u32,
    /// Whether [`Self::ensure_class`] has run (success or permanent failure).
    class_ready: bool,
    /// Last currency we observed/drove, as a row entity — the record half of the
    /// record-then-apply selection authority.
    last_currency: Option<Entity>,
    /// The row-object class registration. **Last field**: drops after every
    /// instance, so the class outlives its instances.
    registration: Option<ClassRegistration>,
}

impl Default for ListBinding {
    fn default() -> Self {
        Self::new()
    }
}

impl ListBinding {
    /// A fresh, empty, unbound list (with its collection view over it).
    pub(crate) fn new() -> Self {
        let coll = ObservableCollection::new();
        let mut cvs = CollectionViewSource::new();
        cvs.set_source(&coll);
        let view = cvs.view();
        Self {
            coll,
            cvs,
            view,
            rows: IndexMap::new(),
            bound_for_uri: None,
            entity_field_index: 0,
            class_ready: false,
            last_currency: None,
            registration: None,
        }
    }

    /// Register the row-object class once, from the row `schema` plus an appended
    /// hidden `u64` entity-identity field. Idempotent: a no-op after the first
    /// call (successful or not).
    fn ensure_class(&mut self, class_name: &str, schema: &[(&'static str, PlainType)]) {
        if self.class_ready {
            return;
        }
        self.class_ready = true;
        let mut builder = ClassBuilder::new(class_name, ClassBase::Freezable, NoopRowHandler);
        for (name, kind) in schema {
            builder.add_property(name, plain_to_prop_type(*kind));
        }
        // Hidden trailing field: the row's stable Entity bits, so a per-row event
        // can recover the originating Entity (Phase 3).
        self.entity_field_index = schema.len() as u32;
        builder.add_property(ENTITY_FIELD, PropType::UInt64);
        match builder.register() {
            Some(reg) => self.registration = Some(reg),
            // Auto-generated class names never collide; reaching here means an
            // explicit `with_class` override duplicated an already-registered
            // name. That is a hard contract violation (rows silently won't
            // realize), so surface it at `error!`, not a swallowable `warn!`.
            None => error!(
                "UiList: failed to register row class {class_name:?} \
                 (duplicate `with_class` name?); rows will not realize",
            ),
        }
    }

    /// Ensure the row class, then reconcile to `desired` — the single entry point
    /// the render state drives each frame.
    pub(crate) fn reconcile_into(
        &mut self,
        class_name: &str,
        schema: &[(&'static str, PlainType)],
        desired: &[DesiredRow],
    ) -> ListOps {
        self.ensure_class(class_name, schema);
        self.reconcile(desired)
    }

    /// Reconcile the live collection to `desired`, emitting the minimal op
    /// sequence (Remove → Update → Add/Move) keyed by [`Entity`]. Never clears.
    fn reconcile(&mut self, desired: &[DesiredRow]) -> ListOps {
        let mut ops = ListOps::default();
        if self.registration.is_none() {
            return ops;
        }
        let desired_set: HashSet<Entity> = desired.iter().map(|d| d.entity).collect();

        // 1. Remove rows whose entity is no longer desired (despawned / unlisted),
        //    high index first so earlier indices stay valid. Dropping the RowSlot
        //    releases our +1 ref after the collection released its own.
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

        // 2. Update surviving rows in place: write only the fields that changed
        //    onto the *existing* instance — no new instance, no collection op.
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

        // 3. Bring the order in line with `desired`, inserting new rows. If nothing
        //    is being added, a keyed LIS pass moves only the rows that must move
        //    (the minimal Move set, so anchored containers — and their selection —
        //    never relocate). With adds, a left-to-right placement pass keeps the
        //    prefix correct as it inserts.
        let has_adds = desired.iter().any(|d| !self.rows.contains_key(&d.entity));
        if has_adds {
            self.place_with_adds(desired, &mut ops);
        } else {
            self.reorder_minimal(desired, &mut ops);
        }
        ops
    }

    /// Left-to-right placement: at each target index, insert a new row or move an
    /// existing one into position. Maintains the invariant that `rows[0..t]`
    /// already equals `desired[0..t]`, so each step is a single insert or move.
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

    /// Pure reorder of a fixed row set: keep the longest run already in the right
    /// relative order (the LIS) anchored, and move only the rest into place. The
    /// minimal `Move` set — anchored containers (and their selection / scroll)
    /// never relocate.
    ///
    /// Processed **right-to-left** so that, at each step, the suffix is already
    /// correct and the row being placed lives somewhere in the unfixed prefix: a
    /// single `move_item(cur, target)` lands it without disturbing the settled
    /// tail.
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

    /// Realize a new row: create an instance and write all of its fields (the
    /// visible schema plus the hidden entity-identity field). `None` if the class
    /// failed to instantiate.
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

    /// The backing collection, for binding as a control's `ItemsSource`.
    pub(crate) fn collection(&self) -> &ObservableCollection {
        &self.coll
    }

    pub(crate) fn needs_bind(&self, uri: &str) -> bool {
        self.bound_for_uri.as_deref() != Some(uri)
    }

    pub(crate) fn mark_bound(&mut self, uri: &str) {
        self.bound_for_uri = Some(uri.to_owned());
    }

    /// Detach (logically) so the next bind pass re-binds against a rebuilt scene.
    pub(crate) fn reset_bind(&mut self) {
        self.bound_for_uri = None;
    }

    /// Lazily (re-)fetch the `ICollectionView` over the collection.
    fn live_view(&mut self) -> Option<&CollectionView> {
        if self.view.is_none() {
            self.view = self.cvs.view();
        }
        self.view.as_ref()
    }

    /// The row entity matching the collection view's current item by pointer
    /// identity, or `None` when the cursor is off the ends.
    fn current_entity(&mut self) -> Option<Entity> {
        let ptr: *mut c_void = {
            let view = self.live_view()?;
            view.current_item()?.raw()
        };
        self.rows
            .iter()
            .find(|(_, slot)| std::ptr::eq(slot.instance.raw(), ptr))
            .map(|(e, _)| *e)
    }

    /// Reconcile selection (currency). **UI wins within a frame**: if the current
    /// item changed since last poll, report it (the caller sets [`Selected`]).
    /// Otherwise, if the app's [`Selected`] differs from currency, drive the
    /// current item to it (record-then-apply). See the module docs.
    ///
    /// `structurally_changed` flags an Add/Remove/Move this frame. Noesis keeps the
    /// `ICollectionView` cursor pinned to an *ordinal position*, not the moved
    /// item, so after a reorder we re-anchor the cursor onto the selected row's new
    /// index — otherwise a reorder would masquerade as a UI selection change. This
    /// is what makes `Selected` (and scroll) ride a `Move`, per the contract.
    pub(crate) fn poll_selection(
        &mut self,
        desired_selected: Option<Entity>,
        structurally_changed: bool,
    ) -> SelectionOutcome {
        if structurally_changed && let Some(sel) = self.last_currency {
            match self.rows.get_index_of(&sel).map(|p| p as i32) {
                Some(position) => {
                    if let Some(view) = self.live_view() {
                        view.move_current_to_position(position);
                    }
                }
                None => {
                    // The selected row was removed; clear the cursor and let the
                    // normal path below settle on "no selection".
                    if let Some(view) = self.live_view() {
                        view.move_current_to_position(-1);
                    }
                    self.last_currency = None;
                }
            }
        }
        let current = self.current_entity();
        if current != self.last_currency {
            self.last_currency = current;
            return SelectionOutcome::UiSelected(current);
        }
        if desired_selected != current {
            let position = match desired_selected {
                Some(e) => self.rows.get_index_of(&e).map_or(-1, |i| i as i32),
                None => -1,
            };
            if let Some(view) = self.live_view() {
                view.move_current_to_position(position);
            }
            self.last_currency = desired_selected;
        }
        SelectionOutcome::Unchanged
    }
}

/// Longest strictly-increasing subsequence of `seq`, returned as the list of
/// **positions** in `seq` (ascending). Used to anchor the rows already in correct
/// relative order during a reorder, so only the rest move.
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
        // Binary search for the first tail whose value is >= seq[i] (strict LIS).
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
    // Reconstruct from the last tail back through `prev`.
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

/// Map a plain-VM field type to the dependency-property type backing it on a row
/// object.
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

/// Write one snapshot value into a row instance's dependency property. `Null`
/// leaves the property untouched (rows have no clear semantics).
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

/// Whether two snapshot values are equal (for the per-row change cache —
/// [`PlainValue`] isn't `PartialEq` across the crate boundary). Differing variants
/// are unequal; `Null` equals only `Null`.
fn values_eq(a: &PlainValue, b: &PlainValue) -> bool {
    match (a, b) {
        (PlainValue::Int32(x), PlainValue::Int32(y)) => x == y,
        (PlainValue::Double(x), PlainValue::Double(y)) => x == y,
        (PlainValue::Bool(x), PlainValue::Bool(y)) => x == y,
        (PlainValue::String(x), PlainValue::String(y)) => x == y,
        (PlainValue::U64(x), PlainValue::U64(y)) => x == y,
        (PlainValue::Null, PlainValue::Null) => true,
        _ => false,
    }
}

/// Compare two snapshot values for the optional Rust-side sort. Mixed / `Null`
/// variants compare equal (the row type is homogeneous, so this only bites on a
/// `Null` field, which then keeps query order).
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

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Ordering for the list diff system relative to the serial push.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum NoesisListSet {
    /// Per-row-type desired-order diff (parallel); runs before [`NoesisSet::Apply`].
    Diff,
}

/// Build the desired ordered rows for each list whose row type is `T`: gather the
/// `T` rows that name each view via [`ListedIn`], snapshot them (appending the
/// entity identity), apply the optional Rust-side sort, and record which row is
/// [`Selected`]. Pure ECS, no Noesis state — parallelizes freely.
#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
fn diff_list<T: NoesisViewModel + Component>(
    lists: Query<(Entity, &UiList)>,
    rows: Query<(Entity, &T, &ListedIn, Has<Selected>)>,
    mut desired: Query<&mut ListDesired>,
) {
    for (view, list) in &lists {
        let Ok(mut slot) = desired.get_mut(view) else {
            continue;
        };
        slot.schema = T::noesis_properties();

        let mut gathered: Vec<(Entity, Vec<PlainValue>, bool)> = rows
            .iter()
            .filter(|(_, _, listed, _)| listed.0 == view)
            .map(|(entity, data, _, selected)| {
                let mut fields = data.noesis_snapshot();
                fields.push(PlainValue::U64(entity.to_bits()));
                (entity, fields, selected)
            })
            .collect();

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

        slot.selected = gathered
            .iter()
            .find(|(_, _, selected)| *selected)
            .map(|(e, _, _)| *e);
        slot.rows = gathered
            .into_iter()
            .map(|(entity, fields, _)| DesiredRow { entity, fields })
            .collect();
    }
}

/// Serial push: drain each view's [`ListDesired`] through the reconciler, bind the
/// `ItemsSource` once the control exists, reconcile the [`Selected`] marker to any
/// UI-driven selection, and emit [`NoesisListOps`] / [`NoesisListSelection`]. The
/// only list system that touches Noesis state.
#[allow(clippy::needless_pass_by_value, clippy::type_complexity)]
fn sync_lists(
    views: Query<(Entity, &UiList, &ListDesired)>,
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
    for (view, list, desired) in &views {
        let (ops, selection) = state.apply_list_for(
            view,
            &list.name,
            &list.class,
            desired.schema,
            &desired.rows,
            desired.selected,
            &click_queue,
        );
        if ops.touched() {
            ops_writer.write(NoesisListOps {
                view,
                list: list.name.clone(),
                adds: ops.adds,
                removes: ops.removes,
                updates: ops.updates,
                moves: ops.moves,
            });
        }
        if let SelectionOutcome::UiSelected(selected) = selection {
            // UI authority: clear every Selected in this list, then mark the new
            // one (deferred commands apply in order, so a re-select nets out to
            // the row staying marked).
            for (entity, listed) in &selected_rows {
                if listed.0 == view {
                    commands.entity(entity).remove::<Selected>();
                }
            }
            if let Some(entity) = selected {
                commands.entity(entity).insert(Selected);
            }
            sel_writer.write(NoesisListSelection {
                view,
                list: list.name.clone(),
                selected,
            });
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// App extension & plugin
// ─────────────────────────────────────────────────────────────────────────────

/// `App` methods to register a list row type. Add [`crate::NoesisPlugin`] first,
/// then register each row component; attach a [`UiList`] to a view and spawn rows
/// with [`ListedIn`] to populate it.
pub trait NoesisListAppExt {
    /// Register `T` as a list row type: its [`NoesisViewModel`] fields become the
    /// bound row-object properties, and `T` rows tagged with [`ListedIn`] are
    /// reconciled into the owner view's [`UiList`].
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

/// Installs the entity-keyed list reconcile pipeline: orders the parallel
/// [`NoesisListSet::Diff`] before [`NoesisSet::Apply`] and adds the serial
/// [`sync_lists`] push. Added by [`crate::NoesisPlugin`]; register row types with
/// [`NoesisListAppExt::add_noesis_list`].
#[derive(Default)]
pub struct NoesisListPlugin;

impl Plugin for NoesisListPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisListOps>();
        app.add_message::<NoesisListSelection>();
        app.configure_sets(PostUpdate, NoesisListSet::Diff.before(NoesisSet::Apply));
        app.add_systems(PostUpdate, sync_lists.in_set(NoesisSet::Apply));
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
        let list = UiList::new("Inv").sorted_by(1, true);
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
        let a = UiList::new("Inv");
        let b = UiList::new("Inv");
        assert_ne!(
            a.class, b.class,
            "auto-generated row classes must be unique"
        );
        assert!(a.class.starts_with("DmList."), "got {:?}", a.class);

        // An explicit override wins.
        let c = UiList::new("Inv").with_class("Game.Row");
        assert_eq!(c.class, "Game.Row");
    }
}

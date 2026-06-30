//! **Primitive 1: panel = entity.** A [`UiPanel`] is a Bevy entity that mounts a
//! sub-XAML fragment as a hosted child of a [`NoesisView`](crate::NoesisView)
//! scene; the *bound components* on that same entity **are** its `DataContext`.
//!
//! ```ignore
//! use bevy::prelude::*;
//! use noesis_bevy::{NoesisViewModel, NoesisPanelAppExt, UiPanel};
//!
//! // Type-named newtype components: `Health(f32)` binds `{Binding Health}`.
//! #[derive(Component, NoesisViewModel)]
//! struct Health(f32);
//! #[derive(Component, NoesisViewModel)]
//! struct Score(i32);
//!
//! fn setup(app: &mut App) {
//!     app.add_noesis_panel_field::<Health>()
//!        .add_noesis_panel_field::<Score>();
//! }
//!
//! // …then, with a host NoesisView entity carrying an x:Name="Hud" Panel:
//! // commands.spawn((
//! //     UiPanel::new("hud.xaml").mount_into(host_view, "Hud"),
//! //     Health(100.0), Score(0),
//! // ));
//! ```
//!
//! Ordinary systems drive the UI: `Query<&mut Health, With<UiPanel>>` mutates the
//! component, change detection re-snapshots it into the bound `{Binding Health}`,
//! and a `TwoWay` edit rides back into the component. Two `UiPanel` entities of
//! the same component set bind **independently**: each loads its own fragment
//! with its own aggregated `DataContext`, isolated by Noesis namescope.
//!
//! # How the aggregation works
//!
//! A panel entity may carry *several* bound components (`Health`, `Score`, …). They
//! must collapse into **one** `DataContext`, not fight over it. So the bridge
//! builds a single synthetic plain-VM class whose properties are the *union* of
//! every bound component's [`NoesisViewModel::noesis_properties`], concatenated at
//! stable index offsets:
//!
//! 1. **Collect (parallel).** A per-type `collect_panel_field` system notes each
//!    registered field type present on the panel and, when the component changed,
//!    stashes its snapshot, all into a plain `Send` `PanelAggregate` component.
//! 2. **Push (serial).** `sync_panels` freezes the layout on first sight, then
//!    drives [`NoesisRenderState::sync_panel`](crate::render): build the class +
//!    fragment, push the changed properties, mount the fragment into the host
//!    scene, and drain UI→Rust writebacks (keyed by *global* property index).
//! 3. **Writeback (parallel).** A per-type `apply_panel_writeback` system routes
//!    each writeback back to the originating component by its frozen index range.
//!
//! This mirrors the "diff in parallel, push serially" convention in
//! [`crate::reconcile`]: only the FFI pushes run on the serial Noesis thread.

use std::any::TypeId;
use std::collections::HashMap;

use bevy::ecs::component::Mutable;
use bevy::prelude::*;

use crate::plain_vm::{NoesisViewModel, PlainType, PlainValue};
use crate::render::{NoesisRenderState, NoesisSet};

/// A panel entity: a sub-XAML fragment mounted as a hosted child of a host
/// [`NoesisView`](crate::NoesisView) scene, with the entity's bound components as
/// its `DataContext`. See the [module docs](self).
///
/// Add the bound components (each a `#[derive(Component, NoesisViewModel)]`
/// registered via [`NoesisPanelAppExt::add_noesis_panel_field`]) to the same
/// entity. The set of bound components is captured the first frame the panel is
/// reconciled and fixed thereafter.
#[derive(Component, Clone, Debug)]
#[require(PanelAggregate)]
pub struct UiPanel {
    uri: String,
    host: Entity,
    host_name: String,
}

impl UiPanel {
    /// Begin a panel that loads `uri` (a key into [`XamlRegistry`](crate::XamlRegistry)).
    /// Call [`mount_into`](Self::mount_into) to choose where it mounts; until then
    /// it has no host and never mounts.
    #[must_use]
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            host: Entity::PLACEHOLDER,
            host_name: String::new(),
        }
    }

    /// Mount this panel's fragment into the `Panel` named `host_name` (an
    /// `x:Name`) within `host`'s scene. `host` is the [`NoesisView`](crate::NoesisView)
    /// entity. Two panels can target the same host (siblings in one collection) or
    /// different hosts; each binds independently.
    #[must_use]
    pub fn mount_into(mut self, host: Entity, host_name: impl Into<String>) -> Self {
        self.host = host;
        self.host_name = host_name.into();
        self
    }

    /// The sub-XAML URI this panel loads.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// The host [`NoesisView`](crate::NoesisView) entity this panel mounts into.
    #[must_use]
    pub fn host(&self) -> Entity {
        self.host
    }

    /// The `x:Name` of the host `Panel` this panel mounts into.
    #[must_use]
    pub fn host_name(&self) -> &str {
        &self.host_name
    }
}

/// One bound component's contribution, collected each frame before the serial push.
struct FieldContribution {
    /// Stable registration order (decides the field's offset in the union).
    reg_index: u32,
    /// `(name, type)` metadata; the union of these across fields is the class.
    props: &'static [(&'static str, PlainType)],
}

/// Per-panel aggregation buffer (a plain `Send` component, auto-required by
/// [`UiPanel`]). Bridges the parallel collect/writeback systems and the serial
/// push: it holds the frozen union layout, this frame's changed snapshots, and the
/// drained UI→Rust writebacks. Holds no Noesis handles.
#[derive(Component, Default)]
pub(crate) struct PanelAggregate {
    /// Field types present on the panel, by `TypeId`. Collected until the layout
    /// is frozen, then left untouched.
    present: HashMap<TypeId, FieldContribution>,
    /// This frame's changed snapshots, per field type. Expanded into global-index
    /// pushes (and cleared) by [`Self::take_pushes`].
    pending: HashMap<TypeId, Vec<PlainValue>>,
    /// Frozen union: `(name, type)` in global index order. Built once.
    layout: Vec<(String, PlainType)>,
    /// Frozen per-type slot: `TypeId` → `(offset, len)` into [`Self::layout`], for
    /// routing writebacks back to the originating component.
    slots: HashMap<TypeId, (u32, u32)>,
    /// UI→Rust writebacks drained from the instance last push (global index +
    /// value); consumed by the per-type writeback systems this frame.
    writebacks: Vec<(u32, PlainValue)>,
    /// Whether [`Self::layout`]/[`Self::slots`] are frozen.
    built: bool,
}

impl PanelAggregate {
    /// Record that field type `tid` is present on the panel (pre-freeze only).
    fn note_present(
        &mut self,
        tid: TypeId,
        reg_index: u32,
        props: &'static [(&'static str, PlainType)],
    ) {
        self.present
            .insert(tid, FieldContribution { reg_index, props });
    }

    /// Stash field type `tid`'s changed snapshot for this frame's push.
    fn set_pending(&mut self, tid: TypeId, snapshot: Vec<PlainValue>) {
        self.pending.insert(tid, snapshot);
    }

    /// Freeze the union layout from the collected fields, ordered by registration
    /// index so the offsets are stable. Called once, when the panel is first
    /// reconciled with at least one field present.
    fn freeze(&mut self) {
        let mut fields: Vec<(&TypeId, &FieldContribution)> = self.present.iter().collect();
        fields.sort_by_key(|(_, c)| c.reg_index);
        let mut offset: u32 = 0;
        for (tid, contrib) in fields {
            let len = contrib.props.len() as u32;
            self.slots.insert(*tid, (offset, len));
            for (name, kind) in contrib.props {
                self.layout.push(((*name).to_owned(), *kind));
            }
            offset += len;
        }
        self.built = true;
    }

    /// Expand this frame's pending snapshots into `(global_index, value)` pushes,
    /// clearing pending. Skips any type not in the frozen layout (added after
    /// freeze).
    fn take_pushes(&mut self) -> Vec<(u32, PlainValue)> {
        let mut out = Vec::new();
        for (tid, snapshot) in self.pending.drain() {
            let Some((offset, len)) = self.slots.get(&tid).copied() else {
                continue;
            };
            for (i, value) in snapshot.into_iter().enumerate() {
                if (i as u32) < len {
                    out.push((offset + i as u32, value));
                }
            }
        }
        out
    }

    /// The frozen `(offset, len)` slot for field type `tid`, if any.
    fn slot(&self, tid: TypeId) -> Option<(u32, u32)> {
        self.slots.get(&tid).copied()
    }
}

/// Stable registration order for panel field types, assigned by
/// [`NoesisPanelAppExt::add_noesis_panel_field`]. Decides each field's offset in
/// the aggregated union so the layout is deterministic across frames.
#[derive(Resource, Default)]
pub(crate) struct PanelFieldOrder {
    indices: HashMap<TypeId, u32>,
    next: u32,
}

impl PanelFieldOrder {
    /// Assign (or return the existing) registration index for `tid`.
    fn register(&mut self, tid: TypeId) -> u32 {
        if let Some(i) = self.indices.get(&tid) {
            return *i;
        }
        let i = self.next;
        self.next += 1;
        self.indices.insert(tid, i);
        i
    }

    fn index_of(&self, tid: TypeId) -> u32 {
        self.indices.get(&tid).copied().unwrap_or(u32::MAX)
    }
}

/// Ordering for the panel collect/writeback systems relative to the serial push
/// in [`NoesisSet::Apply`].
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum NoesisPanelSet {
    /// Per-type field collection (parallel); runs before [`NoesisSet::Apply`].
    Collect,
    /// Per-type writeback routing (parallel); runs after [`NoesisSet::Apply`].
    Writeback,
}

/// Collect one registered field type `T` into each panel's `PanelAggregate`:
/// note it present (pre-freeze) and, when it changed, stash its snapshot. Pure
/// ECS, no Noesis state; parallelizes freely.
#[allow(clippy::needless_pass_by_value)]
fn collect_panel_field<T: NoesisViewModel + Component>(
    order: Res<PanelFieldOrder>,
    mut panels: Query<(&mut PanelAggregate, Ref<T>), With<UiPanel>>,
) {
    let tid = TypeId::of::<T>();
    let reg = order.index_of(tid);
    for (mut agg, field) in &mut panels {
        if !agg.built {
            agg.note_present(tid, reg, T::noesis_properties());
        } else if !agg.slots.contains_key(&tid) {
            // Layout froze on the panel's first reconcile; a component added
            // afterward can't join the DataContext (its bindings stay empty). Ref<T>
            // filters to panels that have T, so this is a genuine late add.
            bevy::log::warn_once!(
                "NoesisPanel: bound component `{}` was inserted after the panel's \
                 DataContext froze on its first reconcile; its fields are not bound. \
                 Insert every bound component before the panel's first frame (e.g. in \
                 the same spawn bundle).",
                std::any::type_name::<T>(),
            );
        }
        if field.is_changed() {
            agg.set_pending(tid, field.noesis_snapshot());
        }
    }
}

/// Serial push: freeze each panel's layout on first sight, then drive
/// [`NoesisRenderState::sync_panel`] (build/mount the fragment, push changed
/// properties, drain writebacks). The only system here that touches Noesis state.
#[allow(clippy::needless_pass_by_value)]
fn sync_panels(
    mut panels: Query<(Entity, &UiPanel, &mut PanelAggregate)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, panel, mut agg) in &mut panels {
        if !agg.built {
            if agg.present.is_empty() {
                continue;
            }
            agg.freeze();
        }
        let pushes = agg.take_pushes();
        let writebacks = state.sync_panel(
            entity,
            &panel.uri,
            panel.host,
            &panel.host_name,
            &agg.layout,
            &pushes,
        );
        agg.writebacks = writebacks;
    }
}

/// Route this frame's UI→Rust writebacks for field type `T` back into the
/// originating component, by `T`'s frozen index range. Only deref-mutates the
/// component when a writeback actually lands (so an idle frame doesn't trip change
/// detection and echo the value back). Pure ECS, no Noesis state.
#[allow(clippy::needless_pass_by_value)]
fn apply_panel_writeback<T: NoesisViewModel + Component<Mutability = Mutable>>(
    mut panels: Query<(&PanelAggregate, &mut T), With<UiPanel>>,
) {
    let tid = TypeId::of::<T>();
    for (agg, mut field) in &mut panels {
        let Some((offset, len)) = agg.slot(tid) else {
            continue;
        };
        for (gi, value) in &agg.writebacks {
            if *gi >= offset && *gi < offset + len {
                field.noesis_apply(*gi - offset, value);
            }
        }
    }
}

/// Observe the `Text` of named elements **inside a panel's fragment**. Add to a
/// [`UiPanel`] entity; changes surface as [`NoesisPanelTextChanged`].
///
/// A mounted fragment keeps a private namescope (its inner `x:Name`s are
/// invisible to a host-root lookup), so this read-back resolves names against the
/// fragment's *own* scope. List fragment-local names like `"HealthText"`. This is
/// the panel counterpart of [`NoesisText`](crate::NoesisText)'s watch list, and
/// the supported way to observe a binding's effect on a panel's UI.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisPanelText {
    /// Fragment-scope element `x:Name`s whose `Text` to observe.
    pub watch: Vec<String>,
}

impl NoesisPanelText {
    /// An empty watch list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: observe these fragment-scope elements' `Text`.
    #[must_use]
    pub fn watching(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.watch.extend(names.into_iter().map(Into::into));
        self
    }
}

/// Emitted when a watched fragment element's `Text` differs from the previous
/// frame. Carries the originating panel entity.
#[derive(Message, Debug, Clone)]
pub struct NoesisPanelTextChanged {
    /// The [`UiPanel`] entity whose fragment element changed.
    pub panel: Entity,
    /// Fragment-scope `x:Name` of the element.
    pub name: String,
    /// Current `Text`. Empty for an unset / cleared DP.
    pub text: String,
}

/// Poll each panel's [`NoesisPanelText`] watch list and emit
/// [`NoesisPanelTextChanged`]. Runs after `sync_panels` so a freshly-pushed
/// value is observable.
#[allow(clippy::needless_pass_by_value)]
fn poll_panel_text(
    panels: Query<(Entity, &NoesisPanelText), With<UiPanel>>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisPanelTextChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, watch) in &panels {
        for (name, text) in state.poll_panel_text_for(entity, &watch.watch) {
            changed.write(NoesisPanelTextChanged {
                panel: entity,
                name,
                text,
            });
        }
    }
}

/// `App` methods to register a panel field type. Add [`NoesisPlugin`](crate::NoesisPlugin)
/// (which installs [`NoesisPanelPlugin`]) first, then register each
/// `#[derive(Component, NoesisViewModel)]` type you bind on a panel.
pub trait NoesisPanelAppExt {
    /// Register `T` as a panel field: spawning `T` on a [`UiPanel`] entity adds
    /// its property/properties to that panel's aggregated `DataContext`, two-way.
    fn add_noesis_panel_field<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
    ) -> &mut Self;
}

impl NoesisPanelAppExt for App {
    fn add_noesis_panel_field<T: NoesisViewModel + Component<Mutability = Mutable>>(
        &mut self,
    ) -> &mut Self {
        {
            let mut order = self
                .world_mut()
                .get_resource_or_insert_with(PanelFieldOrder::default);
            order.register(TypeId::of::<T>());
        }
        self.add_systems(
            PostUpdate,
            collect_panel_field::<T>.in_set(NoesisPanelSet::Collect),
        );
        self.add_systems(
            PostUpdate,
            apply_panel_writeback::<T>.in_set(NoesisPanelSet::Writeback),
        );
        self
    }
}

/// Installs the panel-reconcile pipeline: orders the collect/writeback sets around
/// [`NoesisSet::Apply`] and adds the serial `sync_panels` push. Added by
/// [`NoesisPlugin`](crate::NoesisPlugin); register field types with
/// [`NoesisPanelAppExt::add_noesis_panel_field`].
#[derive(Default)]
pub struct NoesisPanelPlugin;

impl Plugin for NoesisPanelPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PanelFieldOrder>();
        app.configure_sets(
            PostUpdate,
            (
                NoesisPanelSet::Collect.before(NoesisSet::Apply),
                NoesisPanelSet::Writeback.after(NoesisSet::Apply),
            ),
        );
        app.add_message::<NoesisPanelTextChanged>();
        app.add_systems(
            PostUpdate,
            (sync_panels, poll_panel_text.after(sync_panels)).in_set(NoesisSet::Apply),
        );
    }
}

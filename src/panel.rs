//! Panels: XAML fragments mounted into a view, with ECS components as their
//! `DataContext`.
//!
//! A [`UiPanel`] entity loads a sub-XAML fragment and mounts it as a child of a
//! named `Panel` in a [`NoesisView`](crate::NoesisView)'s scene. The bound
//! components on the same entity together form the fragment's `DataContext`.
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
//! Ordinary systems drive the UI. Mutating `Health` through a
//! `Query<&mut Health, With<UiPanel>>` pushes the new value to `{Binding Health}`
//! through change detection, and a `TwoWay` edit in the UI writes back into the
//! component. Two panels with the same components bind independently: each
//! loads its own fragment with its own `DataContext` and its own namescope.
//!
//! To show or hide a fragment element, give a component a `String` field, bind
//! it with `Visibility="{Binding MyVis}"`, and set it to
//! [`visibility::VISIBLE`](crate::visibility::VISIBLE),
//! [`COLLAPSED`](crate::visibility::COLLAPSED) or
//! [`HIDDEN`](crate::visibility::HIDDEN). Noesis's enum converter parses the
//! string.
//!
//! Despawning the entity or removing [`UiPanel`] unmounts the fragment and
//! releases it.
//!
//! # How the components combine
//!
//! The bridge builds one class whose properties are every bound component's
//! [`NoesisViewModel::noesis_properties`], concatenated in registration order.
//! Property names must be unique across a panel's components; duplicates are not
//! detected. Each frame, a per-type system in [`NoesisPanelSet::Collect`]
//! records changed components, the serial push in [`NoesisSet::Apply`] builds
//! and mounts the fragment and pushes the changes, and a per-type system in
//! [`NoesisPanelSet::Writeback`] routes UI edits back to the component they came
//! from. See [`crate::reconcile`] for why the work is split this way.
//!
//! # Layout freeze
//!
//! The `DataContext` layout freezes the first time the panel is reconciled,
//! with whatever bound components are present then. A component inserted later
//! is never bound (a warning is logged once). Spawning the panel and all its
//! components in one bundle is the reliable shape: everything is present when
//! the push runs in `PostUpdate`, so the panel binds on its first frame.
//!
//! If several modules contribute fields, prefer one owning component that holds
//! all of them, with the other modules writing into it. When that isn't
//! possible (one module spawns the panel and another adds its component a frame
//! later), use [`UiPanel::deferred_seal`] and insert [`SealPanel`] from a system
//! ordered after every contributor.

use std::any::TypeId;
use std::collections::HashMap;

use bevy::ecs::component::Mutable;
use bevy::prelude::*;

use crate::plain_vm::{NoesisViewModel, PlainType, PlainValue};
use crate::render::{NoesisRenderState, NoesisSet};

/// A XAML fragment mounted into a host [`NoesisView`](crate::NoesisView)'s
/// scene, with this entity's bound components as its `DataContext`. See the
/// [module docs](self).
///
/// Add the bound components (each a `#[derive(Component, NoesisViewModel)]`
/// registered with [`NoesisPanelAppExt::add_noesis_panel_field`]) to the same
/// entity. The set of bound components is fixed the first frame the panel is
/// reconciled.
///
/// If the fragment fails to load, an error is logged once and the panel retries
/// each frame. If the host element is missing or not a `Panel`, the fragment
/// stays unmounted and a warning is logged each frame.
#[derive(Component, Clone, Debug)]
#[require(PanelAggregate)]
pub struct UiPanel {
    uri: String,
    host: Entity,
    host_name: String,
    static_data: bool,
    deferred: bool,
}

impl UiPanel {
    /// A panel that loads `uri` (a key into [`XamlRegistry`](crate::XamlRegistry)).
    /// Call [`mount_into`](Self::mount_into) as well: without a host the panel
    /// never mounts, and nothing warns.
    #[must_use]
    pub fn new(uri: impl Into<String>) -> Self {
        Self {
            uri: uri.into(),
            host: Entity::PLACEHOLDER,
            host_name: String::new(),
            static_data: false,
            deferred: false,
        }
    }

    /// Mounts the fragment as a child of the `Panel` with `x:Name` `host_name`
    /// in the scene of `host`, a [`NoesisView`](crate::NoesisView) entity.
    /// `host_name` may be scope-qualified (`"Host/Leaf"`). Several panels can
    /// share one host `Panel`; each binds independently. The fragment remounts
    /// after the host scene rebuilds.
    #[must_use]
    pub fn mount_into(mut self, host: Entity, host_name: impl Into<String>) -> Self {
        self.host = host;
        self.host_name = host_name.into();
        self
    }

    /// Mounts the panel even with no bound components, for a static fragment
    /// such as a button-only toolbar or a help overlay. Without this, a panel
    /// waits for its first bound component before it mounts, so a panel that
    /// never gets one never appears.
    #[must_use]
    pub fn static_context(mut self) -> Self {
        self.static_data = true;
        self
    }

    /// Delays the `DataContext` freeze, and so the mount, until a [`SealPanel`]
    /// is inserted on this entity. By default the layout freezes as soon as one
    /// bound component is present.
    ///
    /// Use this when one module spawns the panel and another adds a bound
    /// component a frame later; without it that component would miss the freeze
    /// and stay unbound. Insert [`SealPanel`] from a system ordered after every
    /// contributor. Most panels don't need this; see the [module docs](self).
    /// Takes precedence over [`static_context`](Self::static_context).
    #[must_use]
    pub fn deferred_seal(mut self) -> Self {
        self.deferred = true;
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

/// Freezes a [`deferred_seal`](UiPanel::deferred_seal) panel's `DataContext`
/// with the bound components present now. Insert it from a system ordered after
/// every contributing module. The bridge removes it once applied. Has no effect
/// on a panel without `deferred_seal`.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct SealPanel;

struct FieldContribution {
    /// Registration order; decides the field's offset in the layout.
    reg_index: u32,
    props: &'static [(&'static str, PlainType)],
}

/// Per-panel state shared by the parallel collect/writeback systems and the
/// serial push. Holds no Noesis handles.
#[derive(Component, Default)]
pub(crate) struct PanelAggregate {
    /// Only collected until the layout freezes.
    present: HashMap<TypeId, FieldContribution>,
    /// This frame's changed snapshots; cleared by [`Self::take_pushes`].
    pending: HashMap<TypeId, Vec<PlainValue>>,
    /// Frozen layout in global property-index order.
    layout: Vec<(String, PlainType)>,
    /// `(offset, len)` of each field type in [`Self::layout`].
    slots: HashMap<TypeId, (u32, u32)>,
    /// UI edits from this frame's push, keyed by global property index.
    writebacks: Vec<(u32, PlainValue)>,
    built: bool,
}

impl PanelAggregate {
    fn note_present(
        &mut self,
        tid: TypeId,
        reg_index: u32,
        props: &'static [(&'static str, PlainType)],
    ) {
        self.present
            .insert(tid, FieldContribution { reg_index, props });
    }

    fn set_pending(&mut self, tid: TypeId, snapshot: Vec<PlainValue>) {
        self.pending.insert(tid, snapshot);
    }

    /// Orders fields by registration index so offsets don't depend on `HashMap`
    /// iteration order.
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

    /// Drains pending snapshots into `(global_index, value)` pushes, skipping
    /// field types that arrived after the freeze.
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

    fn slot(&self, tid: TypeId) -> Option<(u32, u32)> {
        self.slots.get(&tid).copied()
    }
}

/// Registration order of panel field types; decides each type's offset in a
/// panel's layout.
#[derive(Resource, Default)]
pub(crate) struct PanelFieldOrder {
    indices: HashMap<TypeId, u32>,
    next: u32,
}

impl PanelFieldOrder {
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

/// System sets for the per-type panel systems, ordered around the serial push
/// in [`NoesisSet::Apply`] (all in `PostUpdate`). Order your own systems
/// against these to see a panel's UI edits in the same frame.
#[derive(SystemSet, Debug, Clone, PartialEq, Eq, Hash)]
pub enum NoesisPanelSet {
    /// Collects changed bound components; runs before [`NoesisSet::Apply`].
    Collect,
    /// Writes UI edits back into the bound components; runs after
    /// [`NoesisSet::Apply`].
    Writeback,
}

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
            // Ref<T> filters to panels that have T, so this is a genuine late add.
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

/// Freezes each panel's layout when it becomes ready, then builds, mounts and
/// pushes it through [`NoesisRenderState::sync_panel`]. The only panel system
/// that touches Noesis.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_panels(
    mut commands: Commands,
    mut panels: Query<(Entity, &UiPanel, &mut PanelAggregate, Has<SealPanel>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, panel, mut agg, sealed) in &mut panels {
        if !agg.built {
            let freeze = if panel.deferred {
                sealed
            } else {
                panel.static_data || !agg.present.is_empty()
            };
            if !freeze {
                continue;
            }
            agg.freeze();
            if panel.deferred {
                commands.entity(entity).remove::<SealPanel>();
            }
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

/// Mutably derefs `T` only when an edit lands, so an idle frame doesn't trip
/// change detection and echo the value back to the UI.
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

/// Watches the `Text` of named elements inside a panel's fragment. Add it to a
/// [`UiPanel`] entity; changes arrive as [`NoesisPanelTextChanged`].
///
/// A mounted fragment has its own namescope, so list fragment-local names such
/// as `"HealthText"`. This is the panel counterpart of
/// [`NoesisText`](crate::NoesisText)'s watch list.
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

    /// Adds `names` to the watch list.
    #[must_use]
    pub fn watching(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.watch.extend(names.into_iter().map(Into::into));
        self
    }
}

/// A watched fragment element's `Text` changed since the last poll. Polled in
/// [`NoesisSet::Apply`] after the panel push, so a value pushed this frame is
/// visible.
#[derive(Message, Debug, Clone)]
pub struct NoesisPanelTextChanged {
    /// The [`UiPanel`] entity whose fragment element changed.
    pub panel: Entity,
    /// Fragment-scope `x:Name` of the element.
    pub name: String,
    /// Current `Text`; empty when unset.
    pub text: String,
}

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

/// Registers component types that can be bound on a [`UiPanel`]. Add
/// [`NoesisPlugin`](crate::NoesisPlugin) first.
pub trait NoesisPanelAppExt {
    /// Registers `T` as a panel field: a `T` on a [`UiPanel`] entity adds its
    /// properties to that panel's `DataContext`, two-way.
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

/// Installs the panel bridge. [`NoesisPlugin`](crate::NoesisPlugin) adds it;
/// register field types with [`NoesisPanelAppExt::add_noesis_panel_field`].
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

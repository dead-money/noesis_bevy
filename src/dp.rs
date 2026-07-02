//! Per-view generic dependency-property get/set bridge, keyed by
//! `(x:Name, property)`.
//!
//! Where [`crate::text`] reads and writes the one fixed `Text` property, this
//! reads and writes *any* named dependency property by name and value type. It's
//! the binding-free fallback (poke `Slider.Value`, read `CheckBox.IsChecked`,
//! flip `Button.IsEnabled`) for when wiring a full
//! [`ViewModel`](crate::viewmodel) would be overkill.
//!
//! Add a [`NoesisDp`] component to the view's camera entity. Its `set` map is the
//! desired value per `(x:Name, property)`, applied to the view's elements
//! whenever the component changes (Bevy change detection). Its `watch` list names
//! `(x:Name, property)` pairs to observe; changes surface as a
//! [`NoesisDpChanged`] message carrying the originating `view` entity.
//!
//! # Value types
//!
//! Noesis is a float engine: many "numeric" properties (`Slider.Value`,
//! `Width`, `Opacity`) are **`f32`**, not `f64`, so reach for [`DpKind::F32`] /
//! [`NoesisDp::set_f32`] there; a `get_f64` against an `f32` property
//! type-mismatches and reads nothing. `CheckBox.IsChecked` is `Nullable<bool>`
//! and is *not* reachable through [`DpKind::Bool`]; bind it through a
//! [`ViewModel`](crate::viewmodel) instead.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisDp::new()
//!         .set_f32("VolumeSlider", "Value", 0.8)              // Rust -> UI
//!         .watch("VolumeSlider", "Value", DpKind::F32),       // subscribe to reads
//! );
//!
//! fn on_change(mut changed: MessageReader<NoesisDpChanged>) {
//!     for ev in changed.read() {
//!         if let ("VolumeSlider", DpValue::F32(v)) = (ev.name.as_str(), &ev.value) { /* ... */ }
//!     }
//! }
//! ```
//!
//! Each `x:Name` may be **scope-qualified** with `/` (e.g.
//! `"Settings/VolumeSlider"`) to reach a property on an element inside a composed
//! control, whose private namescope a root-level lookup can't see. Plain names
//! are unchanged.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component, applies writes + polls the
//! watch list against that view's live scene, and emits messages directly, with
//! no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::view::FrameworkElement;

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Value + kind
// ─────────────────────────────────────────────────────────────────────────────

/// A typed dependency-property value crossing the bridge in either direction.
/// The variant selects the runtime getter/setter, so it must match the
/// property's actual Noesis type (see the module docs on `f32` vs `f64`).
#[derive(Debug, Clone, PartialEq)]
pub enum DpValue {
    /// A 32-bit float, for Noesis's float-typed properties (`Slider.Value`, `Width`, `Opacity`).
    F32(f32),
    /// A 64-bit `Double`.
    F64(f64),
    /// A 32-bit signed integer (`Int32`).
    I32(i32),
    /// A plain `Boolean` (not the `Nullable<bool>` of `CheckBox.IsChecked`).
    Bool(bool),
    /// A UTF-8 string.
    Str(String),
}

impl DpValue {
    /// Write this value into `element`'s `property` dependency property. Returns
    /// `false` on unknown property or type mismatch.
    #[must_use]
    pub fn write_to(&self, element: &mut FrameworkElement, property: &str) -> bool {
        match self {
            Self::F32(v) => element.set_f32(property, *v),
            Self::F64(v) => element.set_f64(property, *v),
            Self::I32(v) => element.set_i32(property, *v),
            Self::Bool(v) => element.set_bool(property, *v),
            Self::Str(v) => element.set_string(property, v),
        }
    }

    /// Box this value as a `Noesis::BoxedValue<T>` for the code-built style /
    /// trigger setter path (`Setter.Value`, `Trigger.Value`). The boxed variant
    /// must match the target property's runtime type, exactly as for
    /// [`write_to`](Self::write_to) (see the module docs on `f32` vs `f64`).
    #[must_use]
    pub fn to_boxed(&self) -> noesis_runtime::binding::Boxed {
        use noesis_runtime::binding::{box_bool, box_f32, box_f64, box_i32, box_string};
        match self {
            Self::F32(v) => box_f32(*v),
            Self::F64(v) => box_f64(*v),
            Self::I32(v) => box_i32(*v),
            Self::Bool(v) => box_bool(*v),
            Self::Str(v) => box_string(v),
        }
    }
}

/// Which value type to read a watched property as. Picks the runtime getter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpKind {
    /// Read as a 32-bit float, yielding [`DpValue::F32`].
    F32,
    /// Read as a 64-bit `Double`, yielding [`DpValue::F64`].
    F64,
    /// Read as a 32-bit signed integer, yielding [`DpValue::I32`].
    I32,
    /// Read as a plain `Boolean`, yielding [`DpValue::Bool`].
    Bool,
    /// Read as a string, yielding [`DpValue::Str`].
    Str,
}

impl DpKind {
    /// Read `element`'s `property` as this kind. `None` on unknown property or
    /// type mismatch.
    #[must_use]
    pub fn read_from(self, element: &FrameworkElement, property: &str) -> Option<DpValue> {
        match self {
            Self::F32 => element.get_f32(property).map(DpValue::F32),
            Self::F64 => element.get_f64(property).map(DpValue::F64),
            Self::I32 => element.get_i32(property).map(DpValue::I32),
            Self::Bool => element.get_bool(property).map(DpValue::Bool),
            Self::Str => element.get_string(property).map(DpValue::Str),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Watch subscription
// ─────────────────────────────────────────────────────────────────────────────

/// One subscription: an element's `x:Name`, the `property` to read, and the
/// [`DpKind`] to read it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpWatch {
    /// `x:Name` of the element to observe.
    pub name: String,
    /// The dependency property on that element to read.
    pub property: String,
    /// The value type to read the property as.
    pub kind: DpKind,
}

impl DpWatch {
    /// Builds a watch on `name`'s `property`, read as `kind`.
    pub fn new(name: impl Into<String>, property: impl Into<String>, kind: DpKind) -> Self {
        Self {
            name: name.into(),
            property: property.into(),
            kind,
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Component
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view generic DP bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisDp {
    /// Desired value per `(x:Name, property)`. Written to the view's elements
    /// whenever this component changes. Writes to the same key apply last-wins.
    pub set: HashMap<(String, String), DpValue>,
    /// `(x:Name, property)` pairs (with read kind) to observe. A change (vs. the
    /// previous frame) emits a [`NoesisDpChanged`]; the first poll after a watch
    /// is added always reports, so callers see the current value.
    pub watch: Vec<DpWatch>,
}

impl NoesisDp {
    /// Creates an empty bridge with no writes or watches queued. Chain the
    /// `set_*` and [`watch`](Self::watch) builders to populate it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: queue an `f32` write, the right choice for Noesis's float-typed
    /// properties (`Slider.Value`, `Width`, `Opacity`, …).
    #[must_use]
    pub fn set_f32(self, name: impl Into<String>, property: impl Into<String>, value: f32) -> Self {
        self.insert(name, property, DpValue::F32(value))
    }

    /// Builder: queue an `f64` (`Double`) write.
    #[must_use]
    pub fn set_f64(self, name: impl Into<String>, property: impl Into<String>, value: f64) -> Self {
        self.insert(name, property, DpValue::F64(value))
    }

    /// Builder: queue an `i32` write.
    #[must_use]
    pub fn set_i32(self, name: impl Into<String>, property: impl Into<String>, value: i32) -> Self {
        self.insert(name, property, DpValue::I32(value))
    }

    /// Builder: queue a `bool` write (plain `Boolean` DPs; not
    /// `CheckBox.IsChecked`).
    #[must_use]
    pub fn set_bool(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: bool,
    ) -> Self {
        self.insert(name, property, DpValue::Bool(value))
    }

    /// Builder: queue a `String` write.
    #[must_use]
    pub fn set_string(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.insert(name, property, DpValue::Str(value.into()))
    }

    /// Builder: observe `name`'s `property`, read as `kind`.
    #[must_use]
    pub fn watch(
        mut self,
        name: impl Into<String>,
        property: impl Into<String>,
        kind: DpKind,
    ) -> Self {
        self.watch.push(DpWatch::new(name, property, kind));
        self
    }

    /// Queue an `f32` write from a system holding `&mut NoesisDp`. The runtime
    /// counterpart of [`set_f32`](Self::set_f32): the next reconcile applies it
    /// to the live element.
    pub fn write_f32(&mut self, name: impl Into<String>, property: impl Into<String>, value: f32) {
        self.write(name, property, DpValue::F32(value));
    }

    /// Queue an `f64` (`Double`) write from a system holding `&mut NoesisDp`.
    /// The runtime counterpart of [`set_f64`](Self::set_f64).
    pub fn write_f64(&mut self, name: impl Into<String>, property: impl Into<String>, value: f64) {
        self.write(name, property, DpValue::F64(value));
    }

    /// Queue an `i32` write from a system holding `&mut NoesisDp`. The runtime
    /// counterpart of [`set_i32`](Self::set_i32).
    pub fn write_i32(&mut self, name: impl Into<String>, property: impl Into<String>, value: i32) {
        self.write(name, property, DpValue::I32(value));
    }

    /// Queue a `bool` write from a system holding `&mut NoesisDp` (plain
    /// `Boolean` DPs; not `CheckBox.IsChecked`). The runtime counterpart of
    /// [`set_bool`](Self::set_bool).
    pub fn write_bool(
        &mut self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: bool,
    ) {
        self.write(name, property, DpValue::Bool(value));
    }

    /// Queue a `String` write from a system holding `&mut NoesisDp`. The runtime
    /// counterpart of [`set_string`](Self::set_string).
    pub fn write_string(
        &mut self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: impl Into<String>,
    ) {
        self.write(name, property, DpValue::Str(value.into()));
    }

    /// Observe `name`'s `property`, read as `kind`, from a system holding
    /// `&mut NoesisDp`. No-op if that exact subscription is already watched. The
    /// runtime counterpart of [`watch`](Self::watch).
    pub fn observe(&mut self, name: impl Into<String>, property: impl Into<String>, kind: DpKind) {
        let watch = DpWatch::new(name, property, kind);
        if !self.watch.contains(&watch) {
            self.watch.push(watch);
        }
    }

    fn insert(
        mut self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: DpValue,
    ) -> Self {
        self.set.insert((name.into(), property.into()), value);
        self
    }

    fn write(&mut self, name: impl Into<String>, property: impl Into<String>, value: DpValue) {
        self.set.insert((name.into(), property.into()), value);
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Read-back message
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when a watched property differs from the previous frame's snapshot.
/// Read with `MessageReader<NoesisDpChanged>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisDpChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose property changed.
    pub view: Entity,
    /// `x:Name` of the element whose property changed.
    pub name: String,
    /// The property that changed.
    pub property: String,
    /// Current value, read as the [`DpWatch::kind`] requested.
    pub value: DpValue,
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisDp`]: apply desired writes when the component
/// changed, then poll its watch list and emit [`NoesisDpChanged`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_dp_bridge(
    views: Query<(Entity, Ref<NoesisDp>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisDpChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, dp) in &views {
        if dp.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_dp_for(entity, &dp.set);
        }
        for (name, property, value) in state.poll_dp_reads_for(entity, &dp.watch) {
            changed.write(NoesisDpChanged {
                view: entity,
                name,
                property,
                value,
            });
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the per-view generic DP bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisDpPlugin;

impl Plugin for NoesisDpPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisDpChanged>()
            .add_systems(PostUpdate, sync_dp_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_set_and_watch() {
        let dp = NoesisDp::new()
            .set_f32("S", "Value", 0.5)
            .set_f64("S", "Double", 1.5)
            .set_i32("C", "SelectedIndex", 2)
            .set_bool("B", "IsEnabled", false)
            .set_string("T", "Text", "hi")
            .watch("S", "Value", DpKind::F32)
            .watch("C", "SelectedIndex", DpKind::I32);

        assert_eq!(
            dp.set.get(&("S".into(), "Value".into())),
            Some(&DpValue::F32(0.5)),
        );
        assert_eq!(
            dp.set.get(&("S".into(), "Double".into())),
            Some(&DpValue::F64(1.5)),
        );
        assert_eq!(
            dp.set.get(&("C".into(), "SelectedIndex".into())),
            Some(&DpValue::I32(2)),
        );
        assert_eq!(
            dp.set.get(&("B".into(), "IsEnabled".into())),
            Some(&DpValue::Bool(false)),
        );
        assert_eq!(
            dp.set.get(&("T".into(), "Text".into())),
            Some(&DpValue::Str("hi".into())),
        );
        assert_eq!(dp.watch.len(), 2);
        assert_eq!(dp.watch[0], DpWatch::new("S", "Value", DpKind::F32));
        assert_eq!(dp.watch[1].kind, DpKind::I32);
    }
}

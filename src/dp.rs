//! Per-view generic dependency-property get/set bridge, keyed by
//! `(x:Name, property)`.
//!
//! A near-mechanical generalization of [`crate::text`]: where the text bridge
//! reads/writes the one fixed `Text` property, this reads/writes *any* named
//! dependency property by name and value type. It's the binding-free fallback —
//! poke `Slider.Value`, read `CheckBox.IsChecked`, flip `Button.IsEnabled` —
//! when wiring a full [`ViewModel`](crate::viewmodel) would be overkill.
//!
//! Add a [`NoesisDp`] component to the view's camera entity. Its `set` map is the
//! desired value per `(x:Name, property)` — applied to the view's elements
//! whenever the component changes (Bevy change detection). Its `watch` list names
//! `(x:Name, property)` pairs to observe; changes surface as a
//! [`NoesisDpChanged`] message carrying the originating `view` entity.
//!
//! # Value types
//!
//! Noesis is a float engine — many "numeric" properties (`Slider.Value`,
//! `Width`, `Opacity`) are **`f32`**, not `f64`, so reach for [`DpKind::F32`] /
//! [`NoesisDp::set_f32`] there; a `get_f64` against an `f32` property
//! type-mismatches and reads nothing. `CheckBox.IsChecked` is `Nullable<bool>`
//! and is *not* reachable through [`DpKind::Bool`] — bind it through a
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
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component, applies writes + polls the
//! watch list against that view's live scene, and emits messages directly — no
//! cross-world queues.

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
    F32(f32),
    F64(f64),
    I32(i32),
    Bool(bool),
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
}

/// Which value type to read a watched property as — picks the runtime getter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DpKind {
    F32,
    F64,
    I32,
    Bool,
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
    pub name: String,
    pub property: String,
    pub kind: DpKind,
}

impl DpWatch {
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
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: queue an `f32` write — the right choice for Noesis's float-typed
    /// properties (`Slider.Value`, `Width`, `Opacity`, …).
    #[must_use]
    pub fn set_f32(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: f32,
    ) -> Self {
        self.insert(name, property, DpValue::F32(value))
    }

    /// Builder: queue an `f64` (`Double`) write.
    #[must_use]
    pub fn set_f64(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: f64,
    ) -> Self {
        self.insert(name, property, DpValue::F64(value))
    }

    /// Builder: queue an `i32` write.
    #[must_use]
    pub fn set_i32(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: i32,
    ) -> Self {
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

    fn insert(
        mut self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: DpValue,
    ) -> Self {
        self.set.insert((name.into(), property.into()), value);
        self
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
        if dp.is_changed() {
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

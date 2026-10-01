//! Read and write any dependency property on a named element, by name.
//!
//! [`crate::text`] covers the `Text` property; this bridge reaches any property
//! whose value type it supports. Use it for one-off pokes and reads (set
//! `Slider.Value`, flip `Button.IsEnabled`, watch `ComboBox.SelectedIndex`) when
//! a [`ViewModel`](crate::viewmodel) would be overkill.
//!
//! Add a [`NoesisDp`] component to the [`NoesisView`](crate::NoesisView) camera
//! entity. Its `set` map holds the desired value per `(x:Name, property)`; when
//! the component changes, or the scene is rebuilt, the reconcile system in
//! [`NoesisSet::Apply`] writes every entry. Its `watch` list names properties to
//! read each frame; a changed value arrives as a [`NoesisDpChanged`] message.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisDp::new()
//!         .set_f32("VolumeSlider", "Value", 0.8)
//!         .watch("VolumeSlider", "Value", DpKind::F32),
//! );
//!
//! fn on_change(mut changed: MessageReader<NoesisDpChanged>) {
//!     for ev in changed.read() {
//!         if let ("VolumeSlider", DpValue::F32(v)) = (ev.name.as_str(), &ev.value) { /* ... */ }
//!     }
//! }
//! ```
//!
//! # Value types
//!
//! The [`DpValue`] variant must match the property's Noesis type, or the write
//! fails with a warning and the read returns nothing. Many numeric properties
//! (`Slider.Value`, `Width`, `Opacity`) are `f32`, not `f64`, so use
//! [`DpKind::F32`] and [`NoesisDp::set_f32`] for them. `CheckBox.IsChecked` is a
//! `Nullable<bool>` and can't be reached with [`DpKind::Bool`]; bind it through
//! a [`ViewModel`](crate::viewmodel) instead.
//!
//! # Semantics
//!
//! - Writes are one-way: removing an entry from `set` leaves the property at
//!   its last value.
//! - Your own writes don't come back as [`NoesisDpChanged`]; only changes made
//!   by the UI or by XAML (bindings, animations, triggers) do.
//! - An `x:Name` can be scope-qualified with `/` (`"Settings/VolumeSlider"`) to
//!   reach an element inside a composed control's private namescope.
//! - A missing name logs a warning on write; a watch on a missing name stays
//!   silent until the element appears.
//! - The bridge acts on view entities only.

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::view::FrameworkElement;

use crate::render::{NoesisRenderState, NoesisSet};

/// A typed property value, written or read. The variant must match the
/// property's Noesis type (see the [module docs](self#value-types)).
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
    /// Write this value into `element`'s `property`. `false` on an unknown
    /// property or a type mismatch.
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

    /// Box this value for a style `Setter.Value` or `Trigger.Value`. As with
    /// [`write_to`](Self::write_to), the variant must match the target property's
    /// type.
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

/// The value type to read a watched property as.
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
    /// Read `element`'s `property` as this kind. `None` on an unknown property or
    /// a type mismatch.
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

/// One watched property: an element's `x:Name`, the `property`, and the
/// [`DpKind`] to read it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DpWatch {
    /// `x:Name` of the element; may be scope-qualified (`"Host/Leaf"`).
    pub name: String,
    /// The dependency property to read.
    pub property: String,
    /// The value type to read it as.
    pub kind: DpKind,
}

impl DpWatch {
    /// A watch on `name`'s `property`, read as `kind`.
    pub fn new(name: impl Into<String>, property: impl Into<String>, kind: DpKind) -> Self {
        Self {
            name: name.into(),
            property: property.into(),
            kind,
        }
    }
}

/// Per-view property bridge. Add it to a [`NoesisView`](crate::NoesisView)
/// entity; see the [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisDp {
    /// Desired value per `(x:Name, property)`. Every entry is rewritten whenever
    /// this component changes; removing an entry does not reset the property.
    pub set: HashMap<(String, String), DpValue>,
    /// Properties read every frame. A value that differs from the last read
    /// sends a [`NoesisDpChanged`]. The first read after a watch is added always
    /// reports, so you see the starting value, unless your own `set` wrote that
    /// property.
    pub watch: Vec<DpWatch>,
}

impl NoesisDp {
    /// An empty bridge. Chain the `set_*` and [`watch`](Self::watch) builders.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: write an `f32`, the type of most numeric properties
    /// (`Slider.Value`, `Width`, `Opacity`).
    #[must_use]
    pub fn set_f32(self, name: impl Into<String>, property: impl Into<String>, value: f32) -> Self {
        self.insert(name, property, DpValue::F32(value))
    }

    /// Builder: write an `f64` (`Double`).
    #[must_use]
    pub fn set_f64(self, name: impl Into<String>, property: impl Into<String>, value: f64) -> Self {
        self.insert(name, property, DpValue::F64(value))
    }

    /// Builder: write an `i32`.
    #[must_use]
    pub fn set_i32(self, name: impl Into<String>, property: impl Into<String>, value: i32) -> Self {
        self.insert(name, property, DpValue::I32(value))
    }

    /// Builder: write a plain `Boolean` (not `CheckBox.IsChecked`).
    #[must_use]
    pub fn set_bool(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: bool,
    ) -> Self {
        self.insert(name, property, DpValue::Bool(value))
    }

    /// Builder: write a `String`.
    #[must_use]
    pub fn set_string(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.insert(name, property, DpValue::Str(value.into()))
    }

    /// Builder: watch `name`'s `property`, read as `kind`. Duplicates are not
    /// removed; use [`observe`](Self::observe) to add a watch only once.
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

    /// In-place form of [`set_f32`](Self::set_f32), for a system holding
    /// `&mut NoesisDp`. Applied by the next reconcile.
    pub fn write_f32(&mut self, name: impl Into<String>, property: impl Into<String>, value: f32) {
        self.write(name, property, DpValue::F32(value));
    }

    /// In-place form of [`set_f64`](Self::set_f64).
    pub fn write_f64(&mut self, name: impl Into<String>, property: impl Into<String>, value: f64) {
        self.write(name, property, DpValue::F64(value));
    }

    /// In-place form of [`set_i32`](Self::set_i32).
    pub fn write_i32(&mut self, name: impl Into<String>, property: impl Into<String>, value: i32) {
        self.write(name, property, DpValue::I32(value));
    }

    /// In-place form of [`set_bool`](Self::set_bool).
    pub fn write_bool(
        &mut self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: bool,
    ) {
        self.write(name, property, DpValue::Bool(value));
    }

    /// In-place form of [`set_string`](Self::set_string).
    pub fn write_string(
        &mut self,
        name: impl Into<String>,
        property: impl Into<String>,
        value: impl Into<String>,
    ) {
        self.write(name, property, DpValue::Str(value.into()));
    }

    /// In-place form of [`watch`](Self::watch). No-op if the same watch already
    /// exists.
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

/// Sent when a watched property's value differs from the last read. Not sent
/// for values written through [`NoesisDp::set`].
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

/// Apply each view's [`NoesisDp`] writes when it changed or the scene was
/// rebuilt, then poll its watches and send [`NoesisDpChanged`].
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

/// Registers the [`NoesisDp`] reconcile system and [`NoesisDpChanged`]. Added
/// by [`crate::NoesisPlugin`].
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

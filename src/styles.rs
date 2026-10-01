//! Restyles named elements of a [`NoesisView`](crate::NoesisView) with a
//! `Style` built in Rust.
//!
//! Add a [`NoesisStyles`] to the view's camera entity. Its
//! [`styles`](NoesisStyles::styles) map gives a [`StyleSpec`] for each
//! `x:Name`. A spec has a target type (a registered type name such as
//! `"Border"`), unconditional [setters](StyleSpec::setter), and optional
//! triggers whose setters apply while a condition holds.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisStyles::new().apply(
//!         "Panel",
//!         StyleSpec::new("Border")
//!             .setter("Opacity", DpValue::F32(0.5))
//!             .setter("Width", DpValue::F32(40.0)),
//!     ),
//! );
//! ```
//!
//! Noesis seals a `Style` the first time it is applied, so whenever the
//! component changes, or the view's scene is rebuilt, every entry is built into
//! a new `Style` and assigned with
//! [`FrameworkElement::set_style`](noesis_runtime::view::FrameworkElement::set_style).
//! Removing an entry leaves the last style on the element. Setters follow the
//! usual dependency-property precedence: a value set locally on the element (in
//! XAML or through [`NoesisDp`](crate::dp::NoesisDp)) beats a style setter.
//! The bridge has no read-back; watch the affected property with
//! [`NoesisDp`](crate::dp::NoesisDp) to observe it.
//!
//! Unknown target types, names, and properties are skipped with a warning.
//!
//! # Scope
//!
//! Supported: `BasedOn` chains ([`StyleSpec::based_on`]), property
//! [triggers](PropertyTrigger), [data triggers](DataTriggerSpec), and
//! [multi triggers](MultiTriggerSpec). `EventTrigger`, `ControlTemplate` /
//! `DataTemplate` assignment, and resource dictionaries are not covered here;
//! use [`noesis_runtime::styles`] and [`noesis_runtime::resources`] directly,
//! or [`NoesisResources`](crate::resources::NoesisResources) for application
//! resources.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::dp::DpValue;
use crate::render::{NoesisRenderState, NoesisSet};

/// A property `Trigger`: while `property` equals `value`, its `setters` apply.
/// The code form of `<Trigger Property="..." Value="...">`. Property names
/// resolve on the owning [`StyleSpec`]'s target type.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyTrigger {
    /// Property the trigger watches.
    pub property: String,
    /// Value that activates the trigger.
    pub value: DpValue,
    /// `(property, value)` setters applied while the trigger is active.
    pub setters: Vec<(String, DpValue)>,
}

impl PropertyTrigger {
    /// Start a trigger that fires while `property == value`.
    #[must_use]
    pub fn new(property: impl Into<String>, value: DpValue) -> Self {
        Self {
            property: property.into(),
            value,
            setters: Vec::new(),
        }
    }

    /// Builder: append a setter applied while the trigger is active.
    #[must_use]
    pub fn setter(mut self, property: impl Into<String>, value: DpValue) -> Self {
        self.setters.push((property.into(), value));
        self
    }
}

/// A `DataTrigger`: while a binding's value equals `value`, its `setters`
/// apply. The code form of `<DataTrigger Binding="{Binding ...}" Value="...">`.
///
/// The binding reads the element's `DataContext` by default.
/// [`relative_source_self`](Self::relative_source_self) binds to the styled
/// element itself instead (`RelativeSource Self`), which works in scenes with
/// no view model.
#[derive(Debug, Clone, PartialEq)]
pub struct DataTriggerSpec {
    /// The binding's property path (e.g. `"IsActive"`, `"Tag"`). Empty binds to
    /// the whole `DataContext` (`{Binding}`).
    pub binding_path: String,
    /// When `true`, bind relative to the styled element itself
    /// (`RelativeSource Self`) instead of its `DataContext`.
    pub relative_source_self: bool,
    /// Value that activates the trigger.
    pub value: DpValue,
    /// `(property, value)` setters applied while the trigger is active.
    pub setters: Vec<(String, DpValue)>,
}

impl DataTriggerSpec {
    /// Start a data trigger that is active while the value at `binding_path`
    /// in the `DataContext` equals `value`.
    #[must_use]
    pub fn new(binding_path: impl Into<String>, value: DpValue) -> Self {
        Self {
            binding_path: binding_path.into(),
            relative_source_self: false,
            value,
            setters: Vec::new(),
        }
    }

    /// Builder: bind relative to the styled element itself (`RelativeSource
    /// Self`) rather than its `DataContext`.
    #[must_use]
    pub fn relative_source_self(mut self) -> Self {
        self.relative_source_self = true;
        self
    }

    /// Builder: append a setter applied while the trigger is active.
    #[must_use]
    pub fn setter(mut self, property: impl Into<String>, value: DpValue) -> Self {
        self.setters.push((property.into(), value));
        self
    }
}

/// A `MultiTrigger`: while every condition holds, its `setters` apply.
/// Conditions and setters resolve on the owning [`StyleSpec`]'s target type.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MultiTriggerSpec {
    /// `(property, value)` conditions; all must hold for the trigger to fire.
    pub conditions: Vec<(String, DpValue)>,
    /// `(property, value)` setters applied while all conditions hold.
    pub setters: Vec<(String, DpValue)>,
}

impl MultiTriggerSpec {
    /// Start an empty multi-trigger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: add a `property == value` condition.
    #[must_use]
    pub fn condition(mut self, property: impl Into<String>, value: DpValue) -> Self {
        self.conditions.push((property.into(), value));
        self
    }

    /// Builder: append a setter applied while all conditions hold.
    #[must_use]
    pub fn setter(mut self, property: impl Into<String>, value: DpValue) -> Self {
        self.setters.push((property.into(), value));
        self
    }
}

/// A `Style` described as plain data; the bridge builds the live `Style` when
/// it applies it. Values are boxed from [`DpValue`], whose variant must match
/// the property's type (see [`crate::dp`]).
#[derive(Debug, Clone, PartialEq)]
pub struct StyleSpec {
    /// Registered type name the style targets (`"Border"`, `"TextBlock"`).
    /// Setter and trigger property names resolve on this type. An unknown type
    /// skips the whole style with a warning.
    pub target_type: String,
    /// Base style to inherit setters and triggers from (`Style.BasedOn`). May
    /// itself have a base.
    pub based_on: Option<Box<StyleSpec>>,
    /// `(property, value)` setters applied unconditionally.
    pub setters: Vec<(String, DpValue)>,
    /// Property triggers in the style's `Triggers` collection.
    pub triggers: Vec<PropertyTrigger>,
    /// Data triggers in the style's `Triggers` collection.
    pub data_triggers: Vec<DataTriggerSpec>,
    /// Multi triggers in the style's `Triggers` collection.
    pub multi_triggers: Vec<MultiTriggerSpec>,
}

impl StyleSpec {
    /// Start a style targeting the registered type `target_type`.
    #[must_use]
    pub fn new(target_type: impl Into<String>) -> Self {
        Self {
            target_type: target_type.into(),
            based_on: None,
            setters: Vec::new(),
            triggers: Vec::new(),
            data_triggers: Vec::new(),
            multi_triggers: Vec::new(),
        }
    }

    /// Builder: inherit setters and triggers from `base` (`Style.BasedOn`).
    #[must_use]
    pub fn based_on(mut self, base: StyleSpec) -> Self {
        self.based_on = Some(Box::new(base));
        self
    }

    /// Builder: append an unconditional setter.
    #[must_use]
    pub fn setter(mut self, property: impl Into<String>, value: DpValue) -> Self {
        self.setters.push((property.into(), value));
        self
    }

    /// Builder: append a property trigger to the style's `Triggers` collection.
    #[must_use]
    pub fn trigger(mut self, trigger: PropertyTrigger) -> Self {
        self.triggers.push(trigger);
        self
    }

    /// Builder: append a [`DataTriggerSpec`] to the style's `Triggers`
    /// collection.
    #[must_use]
    pub fn data_trigger(mut self, trigger: DataTriggerSpec) -> Self {
        self.data_triggers.push(trigger);
        self
    }

    /// Builder: append a [`MultiTriggerSpec`] to the style's `Triggers`
    /// collection.
    #[must_use]
    pub fn multi_trigger(mut self, trigger: MultiTriggerSpec) -> Self {
        self.multi_triggers.push(trigger);
        self
    }
}

/// Code-built styles for named elements. Add to a
/// [`NoesisView`](crate::NoesisView) camera entity; see the
/// [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisStyles {
    /// Style per element `x:Name` (may be scope-qualified, `"Host/Leaf"`).
    pub styles: HashMap<String, StyleSpec>,
}

impl NoesisStyles {
    /// Start with an empty style map. Chain [`apply`](Self::apply) to style
    /// elements by name.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: style element `name` with `spec`.
    #[must_use]
    pub fn apply(mut self, name: impl Into<String>, spec: StyleSpec) -> Self {
        self.styles.insert(name.into(), spec);
        self
    }

    /// Style element `name` with `spec`. The `&mut` form of
    /// [`apply`](Self::apply), for systems that update the component.
    pub fn restyle(&mut self, name: impl Into<String>, spec: StyleSpec) {
        self.styles.insert(name.into(), spec);
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_styles_bridge(
    views: Query<(Entity, Ref<NoesisStyles>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, styles) in &views {
        if styles.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_styles_for(entity, &styles.styles);
        }
    }
}

/// Registers the styles bridge. Added by [`crate::NoesisPlugin`].
pub struct NoesisStylesPlugin;

impl Plugin for NoesisStylesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_styles_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_styles() {
        let s = NoesisStyles::new().apply(
            "Panel",
            StyleSpec::new("Border")
                .setter("Opacity", DpValue::F32(0.5))
                .trigger(
                    PropertyTrigger::new("IsEnabled", DpValue::Bool(false))
                        .setter("Opacity", DpValue::F32(0.25)),
                ),
        );
        let spec = s.styles.get("Panel").expect("Panel styled");
        assert_eq!(spec.target_type, "Border");
        assert_eq!(
            spec.setters,
            vec![("Opacity".to_string(), DpValue::F32(0.5))]
        );
        assert_eq!(spec.triggers.len(), 1);
        assert_eq!(spec.triggers[0].property, "IsEnabled");
        assert_eq!(
            spec.triggers[0].setters,
            vec![("Opacity".to_string(), DpValue::F32(0.25))],
        );
    }

    #[test]
    fn builder_collects_based_on_and_extended_triggers() {
        let spec = StyleSpec::new("Border")
            .based_on(StyleSpec::new("Border").setter("Width", DpValue::F32(40.0)))
            .data_trigger(
                DataTriggerSpec::new("Tag", DpValue::Str("active".into()))
                    .relative_source_self()
                    .setter("Opacity", DpValue::F32(0.5)),
            )
            .multi_trigger(
                MultiTriggerSpec::new()
                    .condition("IsEnabled", DpValue::Bool(true))
                    .condition("IsHitTestVisible", DpValue::Bool(true))
                    .setter("Opacity", DpValue::F32(0.25)),
            );

        let base = spec.based_on.as_deref().expect("based_on set");
        assert_eq!(
            base.setters,
            vec![("Width".to_string(), DpValue::F32(40.0))]
        );

        assert_eq!(spec.data_triggers.len(), 1);
        let dt = &spec.data_triggers[0];
        assert_eq!(dt.binding_path, "Tag");
        assert!(dt.relative_source_self);
        assert_eq!(dt.value, DpValue::Str("active".into()));
        assert_eq!(dt.setters, vec![("Opacity".to_string(), DpValue::F32(0.5))]);

        assert_eq!(spec.multi_triggers.len(), 1);
        let mt = &spec.multi_triggers[0];
        assert_eq!(mt.conditions.len(), 2);
        assert_eq!(
            mt.setters,
            vec![("Opacity".to_string(), DpValue::F32(0.25))]
        );
    }
}

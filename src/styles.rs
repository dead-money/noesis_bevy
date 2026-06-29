//! Per-view code-built `Style` bridge: restyle named XAML elements with a
//! `Noesis::Style` constructed in Rust, no XAML authoring required. The style
//! counterpart of the [`crate::dp`] / [`crate::brushes`] write bridges.
//!
//! Add a [`NoesisStyles`] component to the view's camera entity. Its `styles`
//! map is the desired [`StyleSpec`] per `x:Name`, built into a fresh
//! `Noesis::Style` and assigned to that element via
//! [`FrameworkElement::set_style`](noesis_runtime::view::FrameworkElement::set_style)
//! whenever the component changes (Bevy change detection). A [`StyleSpec`]
//! carries a `TargetType` (the registered type name the style applies to, e.g.
//! `"Border"`), a list of [setters](StyleSpec::setter) (each a `(property,
//! value)` resolved on that target type), and optional property
//! [triggers](StyleSpec::trigger) (apply extra setters while a watched property
//! equals a value).
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
//! This is a **write-only** bridge (like [`crate::focus`] / [`crate::layout`]):
//! it pushes the built style into the live view and emits no read-back of its
//! own. A `Noesis::Style` is *sealed* the first time it is applied, so the
//! bridge builds a brand-new style on every change rather than mutating a
//! retained one. Re-inserting a changed [`NoesisStyles`] re-styles the element.
//! Observe a setter's effect through a [`NoesisDp`](crate::dp::NoesisDp) watch on
//! the property the setter drives (the element's default value is the negative
//! control).
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and, when it changed, builds
//! and applies the styles against that view's live scene.
//!
//! # Scope
//!
//! This bridge covers per-element style application, including the deeper styling
//! the runtime wraps: `BasedOn` inheritance ([`StyleSpec::based_on`], built into a
//! chain of `Noesis::Style`s linked by `Style.BasedOn`), property
//! [triggers](PropertyTrigger), [data triggers](DataTriggerSpec) (a binding's
//! value drives the setters), and [multi triggers](MultiTriggerSpec) (all
//! property conditions must hold). `EventTrigger` (needs `Storyboard` authoring),
//! `ControlTemplate` / `DataTemplate` assignment, and `ResourceDictionary`
//! get/add/merge are *not* wired here; reach for the runtime API
//! ([`noesis_runtime::styles`] / [`noesis_runtime::resources`]) directly for
//! those.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::dp::DpValue;
use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Spec
// ─────────────────────────────────────────────────────────────────────────────

/// A code-built property `Trigger`: while the dependency property `property`
/// (resolved on the owning [`StyleSpec`]'s target type) equals `value`, the
/// trigger's `setters` are applied to the styled element. The programmatic
/// equivalent of a XAML `<Style.Triggers><Trigger Property=… Value=…>`.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyTrigger {
    /// Property the trigger watches (resolved on the style's target type).
    pub property: String,
    /// Value the property is compared against to activate the trigger.
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

    /// Builder: append a setter applied while the trigger is active. The
    /// property resolves on the owning [`StyleSpec`]'s target type.
    #[must_use]
    pub fn setter(mut self, property: impl Into<String>, value: DpValue) -> Self {
        self.setters.push((property.into(), value));
        self
    }
}

/// A code-built `DataTrigger`: while the value produced by a `Binding` equals
/// `value`, the trigger's `setters` are applied to the styled element. The
/// programmatic equivalent of a XAML
/// `<Style.Triggers><DataTrigger Binding="{Binding …}" Value=…>`.
///
/// The binding resolves against the element's `DataContext` by default. Call
/// [`relative_source_self`](Self::relative_source_self) to instead bind a
/// property on the styled element itself (`{Binding Path=…,
/// RelativeSource={RelativeSource Self}}`). Useful for code-only scenes with no
/// view model.
#[derive(Debug, Clone, PartialEq)]
pub struct DataTriggerSpec {
    /// The binding's property path (e.g. `"IsActive"`, `"Tag"`). Empty binds to
    /// the whole `DataContext` (`{Binding}`).
    pub binding_path: String,
    /// When `true`, bind relative to the styled element itself
    /// (`RelativeSource Self`) instead of its `DataContext`.
    pub relative_source_self: bool,
    /// Value the bound value is compared against to activate the trigger.
    pub value: DpValue,
    /// `(property, value)` setters applied while the trigger is active
    /// (resolved on the owning [`StyleSpec`]'s target type).
    pub setters: Vec<(String, DpValue)>,
}

impl DataTriggerSpec {
    /// Start a data trigger that fires while the value at `binding_path` equals
    /// `value`. The binding resolves against the element's `DataContext`.
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

/// A code-built `MultiTrigger`: while **every** property `condition` holds, the
/// trigger's `setters` are applied. The programmatic equivalent of a XAML
/// `<Style.Triggers><MultiTrigger><MultiTrigger.Conditions>…`. Conditions and
/// setters both resolve on the owning [`StyleSpec`]'s target type.
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

    /// Builder: add a `property == value` condition (resolved on the target
    /// type). The trigger fires only when every condition holds.
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

/// A code-built `Noesis::Style`, declarative side. Resolved into a live
/// `Noesis::Style` only at apply time (on the Noesis thread), so the component
/// stays plain data. `setters` apply unconditionally; each trigger applies its
/// own setters while its condition holds. An optional [`based_on`](Self::based_on)
/// style is built and linked via `Style.BasedOn` so this style inherits its
/// setters and triggers.
#[derive(Debug, Clone, PartialEq)]
pub struct StyleSpec {
    /// Registered type name the style targets (e.g. `"Border"`, `"TextBlock"`).
    /// Setter / trigger property names resolve as DPs on this type.
    pub target_type: String,
    /// Optional base style this style inherits from (`Style.BasedOn`). Built
    /// into its own `Noesis::Style` and linked before this style's own setters /
    /// triggers; chains arbitrarily deep.
    pub based_on: Option<Box<StyleSpec>>,
    /// `(property, value)` setters applied unconditionally.
    pub setters: Vec<(String, DpValue)>,
    /// Property triggers in the style's `Triggers` collection.
    pub triggers: Vec<PropertyTrigger>,
    /// Data triggers (binding-value driven) in the style's `Triggers`
    /// collection.
    pub data_triggers: Vec<DataTriggerSpec>,
    /// Multi triggers (all-conditions-hold) in the style's `Triggers`
    /// collection.
    pub multi_triggers: Vec<MultiTriggerSpec>,
}

impl StyleSpec {
    /// Start a style targeting `target_type` (the registered type name whose DPs
    /// the setters resolve against).
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

    /// Builder: set the base style this style inherits setters and triggers from
    /// (`Style.BasedOn`). Chains: the base may itself carry a `based_on`.
    #[must_use]
    pub fn based_on(mut self, base: StyleSpec) -> Self {
        self.based_on = Some(Box::new(base));
        self
    }

    /// Builder: append an unconditional setter (`property` resolved on the
    /// target type) with the boxed `value`.
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

/// Per-view code-built style bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisStyles {
    /// Desired [`StyleSpec`] per `x:Name`. Built and assigned to the view's
    /// elements whenever this component changes. Re-applying the same key
    /// rebuilds and replaces the element's style (Noesis seals a style on first
    /// apply, so each apply is a fresh style).
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

    /// Style element `name` with `spec` from a system holding `&mut NoesisStyles`.
    /// The runtime counterpart of [`apply`](Self::apply): the next reconcile builds
    /// and assigns it to the live element.
    pub fn restyle(&mut self, name: impl Into<String>, spec: StyleSpec) {
        self.styles.insert(name.into(), spec);
    }
}

/// Reconcile every view's [`NoesisStyles`]: build and apply the desired styles
/// when the component changed. Write-only: styles are re-applied once per
/// change.
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

/// Wires the per-view code-built style bridge. Added transitively by
/// [`crate::NoesisPlugin`].
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

//! Per-view code-built `Style` bridge — restyle named XAML elements with a
//! `Noesis::Style` constructed in Rust, no XAML authoring required. The style
//! counterpart of the [`crate::dp`] / [`crate::brushes`] write bridges.
//!
//! Add a [`NoesisStyles`] component to the view's camera entity. Its `styles`
//! map is the desired [`StyleSpec`] per `x:Name` — built into a fresh
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
//! retained one — re-inserting a changed [`NoesisStyles`] re-styles the element.
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
//! This bridge covers the *per-element style application* slice. The runtime
//! ([`noesis_runtime::styles`] / [`noesis_runtime::resources`]) also exposes
//! `BasedOn`, data / multi / event triggers, parsed `ControlTemplate` /
//! `DataTemplate`, and `ResourceDictionary` get/add/merge — none of which are
//! wired here; reach for the runtime API directly for those.

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

/// A code-built `Noesis::Style`, declarative side. Resolved into a live
/// `Noesis::Style` only at apply time (on the Noesis thread), so the component
/// stays plain data. `setters` apply unconditionally; each [`PropertyTrigger`]
/// applies its own setters while its condition holds.
#[derive(Debug, Clone, PartialEq)]
pub struct StyleSpec {
    /// Registered type name the style targets (e.g. `"Border"`, `"TextBlock"`).
    /// Setter / trigger property names resolve as DPs on this type.
    pub target_type: String,
    /// `(property, value)` setters applied unconditionally.
    pub setters: Vec<(String, DpValue)>,
    /// Property triggers in the style's `Triggers` collection.
    pub triggers: Vec<PropertyTrigger>,
}

impl StyleSpec {
    /// Start a style targeting `target_type` (the registered type name whose DPs
    /// the setters resolve against).
    #[must_use]
    pub fn new(target_type: impl Into<String>) -> Self {
        Self {
            target_type: target_type.into(),
            setters: Vec::new(),
            triggers: Vec::new(),
        }
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
}

// ─────────────────────────────────────────────────────────────────────────────
// Component
// ─────────────────────────────────────────────────────────────────────────────

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
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisStyles`]: build and apply the desired styles
/// when the component changed. Write-only — styles are re-applied once per
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
        if styles.is_changed() {
            state.apply_styles_for(entity, &styles.styles);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

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
}

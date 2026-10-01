//! Code-built property animations: start a `DoubleAnimation` on a named
//! element's scalar property from Rust, without a XAML `<Storyboard>`.
//!
//! Add a [`NoesisAnimation`] component to the [`NoesisView`](crate::NoesisView)
//! camera entity. Its `animations` map holds one [`AnimationSpec`] per element
//! `x:Name`. When the component changes, or the view's scene is rebuilt, the
//! reconcile system in [`NoesisSet::Apply`] begins every listed animation through
//! [`Animation::begin_on`](noesis_runtime::animation::Animation::begin_on). The
//! animation then runs on the view's own clock and holds its `to` value when it
//! finishes.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisAnimation::new().animate("Panel", "Opacity", 1.0, 0.25),
//! );
//! ```
//!
//! Each change restarts every entry in the map, not only the one you edited, and
//! replaces any animation already running on that property
//! (`HandoffBehavior::SnapshotAndReplace`). Remove entries you don't want
//! replayed. An unknown `x:Name`, or a property that isn't a `float` dependency
//! property, logs a warning each time the map is applied.
//!
//! The bridge has no read-back message; watch the animated value with
//! [`NoesisDp`](crate::dp::NoesisDp). It acts on view entities only; on a
//! [`UiPanel`](crate::UiPanel) entity it does nothing.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// One `float` animation: interpolate `property` to `to` over `duration_secs`,
/// starting from `from` or, when `None`, from the property's current value.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimationSpec {
    /// The element's scalar dependency property to drive (e.g. `"Width"`,
    /// `"Height"`, `"Opacity"`).
    pub property: String,
    /// Starting value, or `None` to animate from the property's current value.
    pub from: Option<f32>,
    /// Ending value, held after the duration elapses (`HoldEnd`).
    pub to: f32,
    /// Single-pass duration in seconds. `0.0` snaps to `to` on the next tick.
    pub duration_secs: f64,
}

/// Per-view animation bridge. Add it to a [`NoesisView`](crate::NoesisView)
/// entity; see the [module docs](self) for when animations start.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisAnimation {
    /// [`AnimationSpec`] per element `x:Name`, one animation per element. Every
    /// entry restarts whenever this component changes.
    pub animations: HashMap<String, AnimationSpec>,
}

impl NoesisAnimation {
    /// Empty bridge with no animations. Chain [`animate`](Self::animate) or
    /// [`animate_from`](Self::animate_from) to add specs.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: animate element `name`'s `property` to `to` over `duration_secs`,
    /// starting from its current value. Use [`animate_from`](Self::animate_from)
    /// to pin an explicit start.
    #[must_use]
    pub fn animate(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        to: f32,
        duration_secs: f64,
    ) -> Self {
        self.insert(name, property, None, to, duration_secs)
    }

    /// Builder: animate element `name`'s `property` from `from` to `to` over
    /// `duration_secs`.
    #[must_use]
    pub fn animate_from(
        self,
        name: impl Into<String>,
        property: impl Into<String>,
        from: f32,
        to: f32,
        duration_secs: f64,
    ) -> Self {
        self.insert(name, property, Some(from), to, duration_secs)
    }

    fn insert(
        mut self,
        name: impl Into<String>,
        property: impl Into<String>,
        from: Option<f32>,
        to: f32,
        duration_secs: f64,
    ) -> Self {
        self.animations.insert(
            name.into(),
            AnimationSpec {
                property: property.into(),
                from,
                to,
                duration_secs,
            },
        );
        self
    }
}

/// Begin each view's animations when its [`NoesisAnimation`] changed or its
/// scene was rebuilt.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_animation_bridge(
    views: Query<(Entity, Ref<NoesisAnimation>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, animation) in &views {
        if animation.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.begin_animations_for(entity, &animation.animations);
        }
    }
}

/// Registers the [`NoesisAnimation`] reconcile system. Added by
/// [`crate::NoesisPlugin`].
pub struct NoesisAnimationPlugin;

impl Plugin for NoesisAnimationPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_animation_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_animations() {
        let a = NoesisAnimation::new()
            .animate("Panel", "Opacity", 0.0, 0.25)
            .animate_from("Box", "Width", 10.0, 50.0, 0.1);
        assert_eq!(
            a.animations.get("Panel"),
            Some(&AnimationSpec {
                property: "Opacity".to_string(),
                from: None,
                to: 0.0,
                duration_secs: 0.25,
            }),
        );
        assert_eq!(
            a.animations.get("Box"),
            Some(&AnimationSpec {
                property: "Width".to_string(),
                from: Some(10.0),
                to: 50.0,
                duration_secs: 0.1,
            }),
        );
    }
}

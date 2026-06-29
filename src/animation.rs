//! Per-view code-built property animations: the runtime-driven counterpart to a
//! XAML `<Storyboard>`/`<DoubleAnimation>` declared inline in a `ControlTemplate`
//! or `Style.Triggers`.
//!
//! Noesis owns the whole animation system (timelines, easing, the per-view
//! `TimeManager` clock); the SDK's one entry point for starting a single
//! animation against an element's dependency property from code is
//! `BeginAnimation` / `ApplyAnimationClock`, surfaced by the runtime as
//! [`Animation::begin_on`](noesis_runtime::animation::Animation::begin_on). This
//! bridge builds a [`DoubleAnimation`](noesis_runtime::animation::DoubleAnimation)
//! per element `x:Name` and begins it on a named scalar property (e.g. `Width`,
//! `Height`, `Opacity`), so gameplay code can pulse a HUD element or slide a panel
//! without authoring a Storyboard in XAML or routing a fake trigger.
//!
//! Add a [`NoesisAnimation`] component to the view's camera entity. Its
//! `animations` map is the desired [`AnimationSpec`] per `x:Name` (each
//! `(From?, To, Duration)` on a target property), begun against the view's
//! elements whenever the component changes (Bevy change detection). The animation
//! then advances off the view clock pumped by `View::Update`; with the default
//! `HoldEnd` fill behavior the property holds its `To` value after the duration
//! elapses. This is a write-only bridge: there is no read-back message. Observe
//! the animated value through a [`NoesisDp`](crate::dp::NoesisDp) watch.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisAnimation::new().animate("Panel", "Opacity", 1.0, 0.25),
//! );
//! ```
//!
//! Re-begin is the update model: assigning the component again (Bevy change
//! detection) restarts every animation it lists, replacing any clock already
//! running on that property (`HandoffBehavior::SnapshotAndReplace`). Naming an
//! element that doesn't exist, or a property the element doesn't expose as a
//! `float` dependency property, is a no-op and warns once per apply.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and begins the animations
//! against that view's live scene, no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// A single code-built `float` animation: interpolate `property` to `to` over
/// `duration_secs`, optionally starting from an explicit `from` (otherwise from
/// the property's current base value).
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

/// Per-view animation bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisAnimation {
    /// Desired [`AnimationSpec`] per element `x:Name`. Begun against the view's
    /// elements whenever this component changes.
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

/// Reconcile every view's [`NoesisAnimation`]: begin the requested animations
/// when the component changed. Write-only: no read-back message.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_animation_bridge(
    views: Query<(Entity, Ref<NoesisAnimation>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, animation) in &views {
        if animation.is_changed() {
            state.begin_animations_for(entity, &animation.animations);
        }
    }
}

/// Wires the per-view animation bridge. Added transitively by
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

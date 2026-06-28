//! Per-view `Visibility` bridge — show / hide named XAML elements on a single
//! [`NoesisView`](crate::NoesisView).
//!
//! The class-FFI custom-control path covers a lot of ground, but the
//! "show / hide a panel that already exists in the scene" case doesn't benefit
//! from it: the panel is just a plain `<Border>` / `<UserControl>` /
//! `<aor:GamePanel>` whose visibility we want to flip from gameplay code.
//! Registering a Rust class purely to drive an `IsOpen` bool DP and a Style
//! trigger would be heavier than the primitive itself.
//!
//! Add a [`NoesisVisibility`] component to the view's camera entity. Its `set`
//! map is the desired visibility per `x:Name` (`true` = `Visible`,
//! `false` = `Collapsed`) — applied to the view's elements whenever the
//! component changes (Bevy change detection).
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisVisibility::new()
//!         .show("QuitConfirmOverlay")
//!         .hide("LoadingSpinner"),
//! );
//! ```
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives
//! there): the reconcile system reads each view's component and applies the
//! writes against that view's live scene — no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view visibility bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisVisibility {
    /// Desired visibility per element `x:Name` (`true` = `Visible`,
    /// `false` = `Collapsed`). Written to the view's elements whenever this
    /// component changes.
    pub set: HashMap<String, bool>,
}

impl NoesisVisibility {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name` to `Visible`.
    #[must_use]
    pub fn show(self, name: impl Into<String>) -> Self {
        self.set(name, true)
    }

    /// Builder: set element `name` to `Collapsed`.
    #[must_use]
    pub fn hide(self, name: impl Into<String>) -> Self {
        self.set(name, false)
    }

    /// Builder: set element `name`'s visibility. `visible = true` → `Visible`;
    /// `false` → `Collapsed`.
    #[must_use]
    pub fn set(mut self, name: impl Into<String>, visible: bool) -> Self {
        self.set.insert(name.into(), visible);
        self
    }
}

/// Reconcile every view's [`NoesisVisibility`]: apply desired visibility writes
/// when the component changed.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_visibility_bridge(
    views: Query<(Entity, Ref<NoesisVisibility>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, vis) in &views {
        if vis.is_changed() {
            state.apply_visibility_for(entity, &vis.set);
        }
    }
}

/// Wires the per-view visibility bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisVisibilityPlugin;

impl Plugin for NoesisVisibilityPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_visibility_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_set() {
        let v = NoesisVisibility::new()
            .show("QuitConfirmOverlay")
            .hide("LoadingSpinner")
            .set("Hud", true);
        assert_eq!(v.set.get("QuitConfirmOverlay"), Some(&true));
        assert_eq!(v.set.get("LoadingSpinner"), Some(&false));
        assert_eq!(v.set.get("Hud"), Some(&true));
    }
}

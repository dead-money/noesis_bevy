//! Per-view visibility bridge: show or hide named XAML elements on a single
//! [`NoesisView`](crate::NoesisView).
//!
//! Use this to flip a panel that already exists in the scene (a plain
//! `<Border>`, `<UserControl>`, or `<aor:GamePanel>`) from gameplay code.
//! Registering a Rust class purely to drive an `IsOpen` bool DP and a Style
//! trigger would be heavier than the primitive itself.
//!
//! Add a [`NoesisVisibility`] component to the view's camera entity. Its `set`
//! map is the desired visibility per `x:Name` (`true` = `Visible`,
//! `false` = `Collapsed`), applied to the view's elements whenever the
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
//! Everything runs on the main thread (Noesis is thread-affine): the reconcile
//! system reads each view's component and applies the writes against that view's
//! live scene, with no cross-world queues.

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
    /// Starts an empty visibility set. Chain [`show`](Self::show),
    /// [`hide`](Self::hide), or [`set`](Self::set) to fill it, then insert the
    /// result on the [`NoesisView`](crate::NoesisView) camera.
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

    /// Reveal element `name` from a system holding `&mut NoesisVisibility`. The
    /// runtime counterpart of [`show`](Self::show): the next reconcile sets it
    /// to `Visible` on the live element.
    pub fn reveal(&mut self, name: impl Into<String>) {
        self.set.insert(name.into(), true);
    }

    /// Collapse element `name` from a system holding `&mut NoesisVisibility`. The
    /// runtime counterpart of [`hide`](Self::hide): the next reconcile sets it
    /// to `Collapsed` on the live element.
    pub fn collapse(&mut self, name: impl Into<String>) {
        self.set.insert(name.into(), false);
    }

    /// Set element `name`'s visibility from a system holding
    /// `&mut NoesisVisibility`. `visible = true` → `Visible`; `false` →
    /// `Collapsed`. The runtime counterpart of [`set`](Self::set).
    pub fn write(&mut self, name: impl Into<String>, visible: bool) {
        self.set.insert(name.into(), visible);
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
        if vis.is_changed() || state.scene_rebuilt_this_frame(entity) {
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

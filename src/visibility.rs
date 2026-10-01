//! Show or hide named XAML elements on a [`NoesisView`](crate::NoesisView).
//!
//! Use this to toggle a panel that already exists in the scene (a `<Border>`,
//! `<UserControl>`, or custom control) from gameplay code, without a view model.
//! Add a [`NoesisVisibility`] to the view's camera entity. Its
//! [`set`](NoesisVisibility::set) map holds the desired visibility per
//! `x:Name`: `true` is `Visible`, `false` is `Collapsed`.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use noesis_bevy::visibility::NoesisVisibility;
//!
//! fn show_quit_overlay(commands: &mut Commands, view: Entity) {
//!     commands.entity(view).insert(
//!         NoesisVisibility::new()
//!             .show("QuitConfirmOverlay")
//!             .hide("LoadingSpinner"),
//!     );
//! }
//! ```
//!
//! The whole map is written to the scene in [`NoesisSet::Apply`] whenever the
//! component changes and after the scene is rebuilt. The map is write-through:
//! removing an entry leaves the element as it was. Names that aren't found log a
//! warning. This bridge can't express `Hidden`; bind `Visibility` to a view-model
//! string with [`HIDDEN`] for that.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// `Visibility="Visible"`: show an element.
///
/// These three strings are for showing and hiding through a binding instead of
/// [`NoesisVisibility`]: give a [`NoesisViewModel`](crate::plain_vm::NoesisViewModel)
/// component a `String` field, bind it with `Visibility="{Binding panel_vis}"`,
/// and set the field to one of these. Noesis converts the string to the enum, so
/// no `bool`-to-`Visibility` converter is needed.
///
/// ```ignore
/// #[derive(Component, NoesisViewModel)]
/// struct Hud { panel_vis: String } // Visibility="{Binding panel_vis}"
///
/// hud.panel_vis = noesis_bevy::visibility::COLLAPSED.to_string();
/// ```
pub const VISIBLE: &str = "Visible";
/// `Visibility="Collapsed"`: hide an element and remove it from layout. See [`VISIBLE`].
pub const COLLAPSED: &str = "Collapsed";
/// `Visibility="Hidden"`: hide an element but keep its layout space. See [`VISIBLE`].
pub const HIDDEN: &str = "Hidden";

/// Desired visibility of named elements in one view. Add it to a
/// [`NoesisView`](crate::NoesisView) camera entity. See the
/// [module docs](crate::visibility) for when it applies.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisVisibility {
    /// Desired visibility per `x:Name` (`true` = `Visible`, `false` =
    /// `Collapsed`). Names may be scope-qualified (`"Host/Leaf"`).
    pub set: HashMap<String, bool>,
}

impl NoesisVisibility {
    /// Starts an empty map. Chain [`show`](Self::show), [`hide`](Self::hide) or
    /// [`set`](Self::set), then insert the result on the view camera.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: makes element `name` `Visible`.
    #[must_use]
    pub fn show(self, name: impl Into<String>) -> Self {
        self.set(name, true)
    }

    /// Builder: makes element `name` `Collapsed`.
    #[must_use]
    pub fn hide(self, name: impl Into<String>) -> Self {
        self.set(name, false)
    }

    /// Builder: makes element `name` `Visible` (`true`) or `Collapsed` (`false`).
    #[must_use]
    pub fn set(mut self, name: impl Into<String>, visible: bool) -> Self {
        self.set.insert(name.into(), visible);
        self
    }

    /// Makes element `name` `Visible` on the next apply. The in-place form of
    /// [`show`](Self::show), for systems holding `&mut NoesisVisibility`.
    pub fn reveal(&mut self, name: impl Into<String>) {
        self.set.insert(name.into(), true);
    }

    /// Makes element `name` `Collapsed` on the next apply. The in-place form of
    /// [`hide`](Self::hide).
    pub fn collapse(&mut self, name: impl Into<String>) {
        self.set.insert(name.into(), false);
    }

    /// Makes element `name` `Visible` (`true`) or `Collapsed` (`false`) on the
    /// next apply. The in-place form of [`set`](Self::set).
    pub fn write(&mut self, name: impl Into<String>, visible: bool) {
        self.set.insert(name.into(), visible);
    }
}

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

/// Wires the [`NoesisVisibility`] bridge. Added by [`crate::NoesisPlugin`].
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

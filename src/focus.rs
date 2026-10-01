//! Give a named element keyboard focus from Rust, e.g. focus a console's input
//! box when it opens.
//!
//! Add a [`NoesisFocus`] component to a [`NoesisView`](crate::NoesisView) camera
//! entity or a [`UiPanel`](crate::UiPanel) entity and set its `target`. Focus is
//! an action, not a state: the reconcile system in [`NoesisSet::Apply`] focuses
//! the target when the component changes, the scene is rebuilt, or the panel
//! mounts. If focus later moves elsewhere, the bridge does not take it back
//! until the next change.
//!
//! ```ignore
//! commands.entity(view).insert(NoesisFocus::new().focus("CommandInput"));
//! ```
//!
//! An unknown name, or an element that can't take focus, logs a warning.

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view focus bridge. Add it to a [`NoesisView`](crate::NoesisView) or
/// [`UiPanel`](crate::UiPanel) entity; see the [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisFocus {
    /// `x:Name` of the element to focus, applied once per change. `None` does
    /// nothing.
    pub target: Option<String>,
}

impl NoesisFocus {
    /// A bridge with no target. Chain [`focus`](Self::focus), or insert it as-is
    /// and set a target later.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: focus the element named `name`.
    #[must_use]
    pub fn focus(mut self, name: impl Into<String>) -> Self {
        self.target = Some(name.into());
        self
    }

    /// In-place form of [`focus`](Self::focus), for a system holding
    /// `&mut NoesisFocus`. Applied by the next reconcile, even when `name` is
    /// already the target.
    pub fn focus_on(&mut self, name: impl Into<String>) {
        self.target = Some(name.into());
    }

    /// Clear the target. Focus stays where it is.
    pub fn clear(&mut self) {
        self.target = None;
    }
}

/// Apply each entity's [`NoesisFocus`] when it changed, its scene was rebuilt,
/// or its panel mounted.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_focus_bridge(
    views: Query<(Entity, Ref<NoesisFocus>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, focus) in &views {
        if focus.is_changed()
            || state.scene_rebuilt_this_frame(entity)
            || state.panel_mounted_this_frame(entity)
        {
            state.apply_focus_for(entity, focus.target.as_deref());
        }
    }
}

/// Registers the [`NoesisFocus`] reconcile system. Added by
/// [`crate::NoesisPlugin`].
pub struct NoesisFocusPlugin;

impl Plugin for NoesisFocusPlugin {
    fn build(&self, app: &mut App) {
        // After `sync_panels`, which sets `panel_mounted_this_frame`, so a panel
        // applies the same frame it mounts.
        app.add_systems(
            PostUpdate,
            sync_focus_bridge
                .in_set(NoesisSet::Apply)
                .after(crate::panel::sync_panels),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_sets_target() {
        let f = NoesisFocus::new().focus("CommandInput");
        assert_eq!(f.target.as_deref(), Some("CommandInput"));
        assert!(NoesisFocus::new().target.is_none());
    }
}

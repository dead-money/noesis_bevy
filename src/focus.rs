//! Per-view keyboard-focus bridge: give a named XAML element keyboard
//! focus on a single [`crate::NoesisView`].
//!
//! Add a [`NoesisFocus`] component to the view's camera entity. Its
//! `target` is the `x:Name` to focus, applied to the view's element
//! whenever the component changes (Bevy change detection). Focus is an
//! action: it fires once per change, not continuously. Drives the
//! "open the console, give the input box keyboard focus" flow without a
//! class registration or custom DP, with just a name and one FFI call.
//!
//! ```ignore
//! commands.entity(view).insert(NoesisFocus::new().focus("CommandInput"));
//! ```
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives
//! there): the reconcile system reads each view's component and, when it
//! changed, applies the focus against that view's live scene.

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view focus bridge. Attach to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisFocus {
    /// `x:Name` of the element to focus. Applied once whenever this
    /// component changes; `None` is a no-op.
    pub target: Option<String>,
}

impl NoesisFocus {
    /// Empty focus bridge with no target. Chain [`focus`](Self::focus) to name
    /// the element, or attach as-is and set `target` later.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: focus the element identified by `x:Name`.
    #[must_use]
    pub fn focus(mut self, name: impl Into<String>) -> Self {
        self.target = Some(name.into());
        self
    }

    /// Focus element `name` from a system holding `&mut NoesisFocus`. The
    /// runtime counterpart of [`focus`](Self::focus): the next reconcile
    /// applies it to the live element.
    pub fn focus_on(&mut self, name: impl Into<String>) {
        self.target = Some(name.into());
    }

    /// Clear the pending focus target from a system holding `&mut NoesisFocus`.
    /// The next reconcile applies nothing (`None` is a no-op).
    pub fn clear(&mut self) {
        self.target = None;
    }
}

/// Reconcile every view's [`NoesisFocus`]: apply the focus action when the
/// component changed. Write-only: focus is applied once per change.
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

/// Wires the per-view focus bridge. Added transitively by [`crate::NoesisPlugin`].
pub struct NoesisFocusPlugin;

impl Plugin for NoesisFocusPlugin {
    fn build(&self, app: &mut App) {
        // After `sync_panels` so a panel's `NoesisFocus` re-applies the same frame
        // its fragment mounts (panel focus reads `panel_mounted_this_frame`).
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

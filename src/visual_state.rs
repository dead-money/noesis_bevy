//! Per-view `VisualStateManager::GoToState` writes against named XAML controls —
//! the code-driven counterpart to a `ControlTemplate`'s trigger-driven visual
//! states (e.g. a control's `CommonStates`: `Normal` / `MouseOver` / `Pressed`
//! / `Disabled`, or app-authored groups).
//!
//! Noesis declares visual states inside a control's `ControlTemplate` as
//! `VisualStateGroup`s. `VisualStateManager::GoToState` is the SDK's one
//! entry point for transitioning a templated control between those states from
//! code; the runtime surfaces it as `FrameworkElement::go_to_state`. This bridge
//! drives it per element `x:Name`, so gameplay code can flip a HUD widget to
//! "Alert" or a button to "Pressed" without routing a fake input event.
//!
//! Add a [`NoesisVisualState`] component to the view's camera entity. Its
//! `states` map is the desired `(state name, use_transitions)` per `x:Name` —
//! applied to the view's controls whenever the component changes (Bevy change
//! detection). `use_transitions = true` runs the state's `VisualTransition`
//! (animated change); `false` snaps straight to the target state. This is a
//! write-only bridge: there is no read-back message.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisVisualState::new().state("AlarmPanel", "Alert", true),
//! );
//! ```
//!
//! `GoToState` only does useful work for a *templated control*: it walks the
//! element's `ControlTemplate` for the `VisualStateGroup` owning the named
//! state. Targeting a bare element with no template (or naming a state no group
//! knows) is a no-op and logs a warning once per apply.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies the state
//! transitions against that view's live scene — no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// A requested visual-state transition: the target state's name and whether to
/// run its `VisualTransition` (`true`, animated) or snap to it (`false`).
pub type StateRequest = (String, bool);

/// Per-view visual-state bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisVisualState {
    /// Desired `(state, use_transitions)` per control `x:Name`. Driven into the
    /// view's controls via `VisualStateManager::GoToState` whenever this
    /// component changes.
    pub states: HashMap<String, StateRequest>,
}

impl NoesisVisualState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: transition control `name` to visual state `state`. Pass
    /// `use_transitions = true` to run the state's `VisualTransition` (animated),
    /// or `false` to snap straight to it.
    #[must_use]
    pub fn state(
        mut self,
        name: impl Into<String>,
        state: impl Into<String>,
        use_transitions: bool,
    ) -> Self {
        self.states
            .insert(name.into(), (state.into(), use_transitions));
        self
    }
}

/// Reconcile every view's [`NoesisVisualState`]: apply desired state transitions
/// when the component changed. Write-only — no read-back message.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_visual_state_bridge(
    views: Query<(Entity, Ref<NoesisVisualState>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, visual_state) in &views {
        if visual_state.is_changed() {
            state.apply_visual_state_for(entity, &visual_state.states);
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the per-view visual-state bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisVisualStatePlugin;

impl Plugin for NoesisVisualStatePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_visual_state_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_states() {
        let s = NoesisVisualState::new()
            .state("Panel", "Alert", true)
            .state("Button", "Pressed", false);
        assert_eq!(s.states.get("Panel"), Some(&("Alert".to_string(), true)));
        assert_eq!(
            s.states.get("Button"),
            Some(&("Pressed".to_string(), false)),
        );
    }
}

//! Move named templated controls between visual states from code.
//!
//! A control's `ControlTemplate` declares `VisualStateGroup`s (for example
//! `CommonStates` with `Normal`, `MouseOver`, `Pressed` and `Disabled`, or groups
//! you author). This bridge calls `VisualStateManager::GoToState` on elements by
//! `x:Name`, so gameplay code can switch a HUD widget to `"Alert"` or a button to
//! `"Pressed"` without faking input.
//!
//! Add a [`NoesisVisualState`] to the [`NoesisView`](crate::NoesisView) camera
//! entity. Its [`states`](NoesisVisualState::states) map holds the target state
//! per `x:Name`.
//!
//! ```no_run
//! use bevy::prelude::*;
//! use noesis_bevy::visual_state::NoesisVisualState;
//!
//! fn raise_alarm(commands: &mut Commands, view: Entity) {
//!     commands
//!         .entity(view)
//!         .insert(NoesisVisualState::new().state("AlarmPanel", "Alert", true));
//! }
//! ```
//!
//! Every entry in the map is applied in [`NoesisSet::Apply`] whenever the
//! component changes and after the scene is rebuilt, so changing one entry
//! re-runs the transition for all of them. Removing an entry leaves the control
//! in its current state. `GoToState` only works on a templated control whose
//! template defines the named state; a missing name, an untemplated element or
//! an unknown state logs a warning on each apply. There is no read-back.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Target state name, and whether to run its `VisualTransition` (`true`) or
/// snap straight to it (`false`).
pub type StateRequest = (String, bool);

/// Target visual states of named controls in one view. Add it to a
/// [`NoesisView`](crate::NoesisView) camera entity. See the
/// [module docs](crate::visual_state) for when it applies.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisVisualState {
    /// Target `(state, use_transitions)` per control `x:Name`. Names may be
    /// scope-qualified (`"Host/Leaf"`).
    pub states: HashMap<String, StateRequest>,
}

impl NoesisVisualState {
    /// Starts an empty map. Chain [`state`](Self::state) to fill it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: moves control `name` to visual state `state`. With
    /// `use_transitions` it runs the state's `VisualTransition`; without, it
    /// snaps straight to the state.
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

    /// Moves control `name` to visual state `state` on the next apply. The
    /// in-place form of [`state`](Self::state), for systems holding
    /// `&mut NoesisVisualState`.
    pub fn go_to(
        &mut self,
        name: impl Into<String>,
        state: impl Into<String>,
        use_transitions: bool,
    ) {
        self.states
            .insert(name.into(), (state.into(), use_transitions));
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_visual_state_bridge(
    views: Query<(Entity, Ref<NoesisVisualState>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, visual_state) in &views {
        if visual_state.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_visual_state_for(entity, &visual_state.states);
        }
    }
}

/// Wires the [`NoesisVisualState`] bridge. Added by [`crate::NoesisPlugin`].
pub struct NoesisVisualStatePlugin;

impl Plugin for NoesisVisualStatePlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            PostUpdate,
            sync_visual_state_bridge.in_set(NoesisSet::Apply),
        );
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

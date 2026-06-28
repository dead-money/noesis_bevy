//! Imperative `Focus()` requests against named XAML elements.
//!
//! Mirrors [`crate::visibility::NoesisVisibilityRequests`] in shape: a
//! main-app push queue, drained on the render side each frame. Drives
//! the "open the console, give the input box keyboard focus" flow
//! without needing a class registration or custom DP — just a name and
//! one FFI call.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};

use crate::render::NoesisRenderState;

/// Main-app-side queue of pending focus requests. Push via
/// [`Self::request`]; the render world drains and applies during
/// `RenderSystems::Prepare`.
#[derive(Resource, Clone, Default)]
pub struct NoesisFocusRequests(SharedFocusQueue);

impl NoesisFocusRequests {
    /// Queue a focus request for the element identified by `x:Name`.
    /// Multiple requests within a single frame are applied in order;
    /// the last one wins (whichever element accepted focus last is the
    /// one keyboard input goes to).
    pub fn request(&self, name: impl Into<String>) {
        self.0.push(name.into());
    }
}

impl ExtractResource for NoesisFocusRequests {
    type Source = NoesisFocusRequests;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

#[derive(Clone, Default)]
pub(crate) struct SharedFocusQueue(Arc<Mutex<Vec<String>>>);

impl SharedFocusQueue {
    fn push(&self, name: String) {
        self.0.lock().expect("SharedFocusQueue poisoned").push(name);
    }

    pub(crate) fn drain(&self) -> Vec<String> {
        let mut guard = self.0.lock().expect("SharedFocusQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn apply_focus_requests(
    requests: Option<Res<NoesisFocusRequests>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(requests), Some(mut state)) = (requests, state) else {
        return;
    };
    state.apply_focus_requests(&requests.0);
}

/// Wires the focus-request bridge.
pub struct NoesisFocusPlugin;

impl Plugin for NoesisFocusPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisFocusRequests>()
            .add_plugins(ExtractResourcePlugin::<NoesisFocusRequests>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };
        render_app.add_systems(Render, apply_focus_requests.in_set(RenderSystems::Prepare));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_queue_drain_round_trip() {
        let q = SharedFocusQueue::default();
        q.push("CommandInput".into());
        q.push("LogText".into());
        let drained = q.drain();
        assert_eq!(
            drained,
            vec!["CommandInput".to_string(), "LogText".to_string()],
        );
        assert!(q.drain().is_empty(), "second drain should be empty");
    }
}

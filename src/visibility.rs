//! Imperative `Visibility` writes against named XAML elements.
//!
//! The class-FFI custom-control path covers a lot of ground, but the
//! "show / hide a panel that already exists in the scene" case doesn't
//! benefit from it: the panel is just a plain `<Border>` /
//! `<UserControl>` / `<aor:GamePanel>` whose visibility we want to flip
//! from gameplay code. Registering a Rust class purely to drive an
//! `IsOpen` bool DP and a Style trigger would be heavier than the
//! primitive itself.
//!
//! [`NoesisVisibilityRequests`] takes a list of `(x:Name, visible)`
//! pairs from the main world, applies them to the live View on the
//! render side, then clears the queue. The pattern mirrors
//! [`crate::events`]: a main-world resource is `Arc`-shared with the
//! render world; the render-world system drains and writes; the
//! resource is single-app-shared via [`ExtractResource`] so the queue
//! the main app pushes into is the same one the render app drains.
//!
//! ```ignore
//! commands.insert_resource(NoesisVisibilityRequests::default());
//! // ... later, in a Bevy system:
//! visibility.show("QuitConfirmOverlay");
//! visibility.hide("QuitConfirmOverlay");
//! ```

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};

use crate::render::NoesisRenderState;

/// Main-app-side queue of pending visibility writes. Push via [`Self::show`]
/// / [`Self::hide`] / [`Self::set`]; the render world drains and applies
/// during `RenderSystems::Prepare`.
///
/// Cheap to keep around even when no writes are pending — the underlying
/// `Vec` only allocates on first push.
#[derive(Resource, Clone, Default)]
pub struct NoesisVisibilityRequests(SharedVisibilityQueue);

impl NoesisVisibilityRequests {
    /// Queue a write setting `name`'s `Visibility` to `Visible`.
    pub fn show(&self, name: impl Into<String>) {
        self.set(name, true);
    }

    /// Queue a write setting `name`'s `Visibility` to `Collapsed`.
    pub fn hide(&self, name: impl Into<String>) {
        self.set(name, false);
    }

    /// Queue a write. `visible = true` → `Visible`; `false` → `Collapsed`.
    /// Multiple writes for the same name within a single frame are
    /// applied in order; the last one wins.
    pub fn set(&self, name: impl Into<String>, visible: bool) {
        self.0.push(name.into(), visible);
    }
}

impl ExtractResource for NoesisVisibilityRequests {
    type Source = NoesisVisibilityRequests;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

/// Internal Arc-backed queue. Both apps share the same `Vec` via a clone
/// of this `Arc`; the render-side drain mutates the original storage,
/// not a per-frame copy.
#[derive(Clone, Default)]
pub(crate) struct SharedVisibilityQueue(Arc<Mutex<Vec<(String, bool)>>>);

impl SharedVisibilityQueue {
    fn push(&self, name: String, visible: bool) {
        self.0
            .lock()
            .expect("SharedVisibilityQueue poisoned")
            .push((name, visible));
    }

    /// Take the pending writes out of the queue. Cheap when empty.
    pub(crate) fn drain(&self) -> Vec<(String, bool)> {
        let mut guard = self.0.lock().expect("SharedVisibilityQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Render-app system: drain the visibility queue and apply each entry to
/// the live View. Runs in `RenderSystems::Prepare` so writes from this
/// frame's main-world systems land before Noesis's update_render_tree.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn apply_visibility_requests(
    requests: Option<Res<NoesisVisibilityRequests>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(requests), Some(mut state)) = (requests, state) else {
        return;
    };
    state.apply_visibility_requests(&requests.0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the visibility-request bridge: extracts
/// [`NoesisVisibilityRequests`] to the render world and runs the
/// render-side drain after `ensure_noesis_scene`.
pub struct NoesisVisibilityPlugin;

impl Plugin for NoesisVisibilityPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisVisibilityRequests>()
            .add_plugins(ExtractResourcePlugin::<NoesisVisibilityRequests>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // `Prepare` runs after `ensure_noesis_scene`; on frames where the
        // scene isn't built yet, `apply_visibility_requests` is a no-op
        // and the queue stays full for next frame.
        render_app.add_systems(
            Render,
            apply_visibility_requests.in_set(RenderSystems::Prepare),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_takes_all_and_resets() {
        let q = SharedVisibilityQueue::default();
        q.push("Alpha".into(), true);
        q.push("Beta".into(), false);
        let drained = q.drain();
        assert_eq!(
            drained,
            vec![("Alpha".to_string(), true), ("Beta".to_string(), false)]
        );
        // Second drain returns empty without allocating.
        assert!(q.drain().is_empty());
    }

    #[test]
    fn show_hide_helpers_match_explicit_set() {
        let r = NoesisVisibilityRequests::default();
        r.show("X");
        r.hide("Y");
        r.set("Z", true);
        let drained = r.0.drain();
        assert_eq!(
            drained,
            vec![
                ("X".to_string(), true),
                ("Y".to_string(), false),
                ("Z".to_string(), true),
            ]
        );
    }

    #[test]
    fn last_write_wins_within_a_frame() {
        // The contract: multiple writes for the same name in one frame
        // are applied in order, last wins. We don't dedupe — the render
        // side just processes the queue front-to-back, so the final
        // state is whatever the last entry asked for.
        let r = NoesisVisibilityRequests::default();
        r.show("Panel");
        r.hide("Panel");
        let drained = r.0.drain();
        assert_eq!(
            drained,
            vec![("Panel".to_string(), true), ("Panel".to_string(), false)]
        );
    }
}

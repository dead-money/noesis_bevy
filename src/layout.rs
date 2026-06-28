//! Imperative `Margin` writes against named XAML elements — the positioning
//! primitive for floating panels (context menus, popups, tooltips) that must
//! follow gameplay coordinates.
//!
//! Noesis's `Canvas.Left`/`Top` attached property isn't surfaced through the
//! shim, but `FrameworkElement::Margin` is a plain dependency property. A
//! `Left`/`Top`-anchored element with `Margin = (x, y, 0, 0)` lands its corner
//! at `(x, y)`, so a single margin write positions a floating element anywhere
//! in the view. Coordinates are Noesis *view* DIPs (the `NoesisScene::size`
//! space), so a caller working in window pixels scales by `view_size /
//! window_size` first — exactly the mapping the input bridge uses.
//!
//! [`NoesisLayoutRequests`] mirrors [`crate::visibility`]: a main-world resource
//! is `Arc`-shared with the render world; the render-side system drains and
//! writes during `RenderSystems::Prepare`.
//!
//! ```ignore
//! commands.insert_resource(NoesisLayoutRequests::default());
//! // ... later, in a Bevy system:
//! layout.set_margin("PartMenu", cursor_x, cursor_y, 0.0, 0.0);
//! ```

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};

use crate::render::NoesisRenderState;

/// Left, top, right, bottom offsets in view DIPs.
pub type Margin = [f32; 4];

/// Main-app-side queue of pending margin writes. Push via [`Self::set_margin`];
/// the render world drains and applies during `RenderSystems::Prepare`.
///
/// Cheap to keep around even when no writes are pending — the underlying `Vec`
/// only allocates on first push.
#[derive(Resource, Clone, Default)]
pub struct NoesisLayoutRequests(SharedLayoutQueue);

impl NoesisLayoutRequests {
    /// Queue a write setting `name`'s `Margin` to `(left, top, right, bottom)`
    /// (view DIPs). Multiple writes for the same name within a single frame are
    /// applied in order; the last one wins.
    pub fn set_margin(
        &self,
        name: impl Into<String>,
        left: f32,
        top: f32,
        right: f32,
        bottom: f32,
    ) {
        self.0.push(name.into(), [left, top, right, bottom]);
    }
}

impl ExtractResource for NoesisLayoutRequests {
    type Source = NoesisLayoutRequests;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

/// Internal Arc-backed queue. Both apps share the same `Vec` via a clone of
/// this `Arc`; the render-side drain mutates the original storage, not a
/// per-frame copy.
#[derive(Clone, Default)]
pub(crate) struct SharedLayoutQueue(Arc<Mutex<Vec<(String, Margin)>>>);

impl SharedLayoutQueue {
    fn push(&self, name: String, margin: Margin) {
        self.0
            .lock()
            .expect("SharedLayoutQueue poisoned")
            .push((name, margin));
    }

    /// Take the pending writes out of the queue. Cheap when empty.
    pub(crate) fn drain(&self) -> Vec<(String, Margin)> {
        let mut guard = self.0.lock().expect("SharedLayoutQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

/// Render-app system: drain the layout queue and apply each entry to the live
/// View. Runs in `RenderSystems::Prepare` so writes from this frame's main-world
/// systems land before Noesis's `update_render_tree`.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn apply_layout_requests(
    requests: Option<Res<NoesisLayoutRequests>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(requests), Some(mut state)) = (requests, state) else {
        return;
    };
    state.apply_layout_requests(&requests.0);
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the layout-request bridge: extracts [`NoesisLayoutRequests`] to the
/// render world and runs the render-side drain in `RenderSystems::Prepare`.
pub struct NoesisLayoutPlugin;

impl Plugin for NoesisLayoutPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisLayoutRequests>()
            .add_plugins(ExtractResourcePlugin::<NoesisLayoutRequests>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        // `Prepare` runs after `ensure_noesis_scene`; on frames where the scene
        // isn't built yet, `apply_layout_requests` is a no-op and the queue
        // stays full for next frame.
        render_app.add_systems(Render, apply_layout_requests.in_set(RenderSystems::Prepare));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drain_takes_all_and_resets() {
        let q = SharedLayoutQueue::default();
        q.push("Menu".into(), [10.0, 20.0, 0.0, 0.0]);
        q.push("Tip".into(), [1.0, 2.0, 3.0, 4.0]);
        let drained = q.drain();
        assert_eq!(
            drained,
            vec![
                ("Menu".to_string(), [10.0, 20.0, 0.0, 0.0]),
                ("Tip".to_string(), [1.0, 2.0, 3.0, 4.0]),
            ]
        );
        assert!(q.drain().is_empty());
    }

    #[test]
    fn last_write_wins_within_a_frame() {
        let r = NoesisLayoutRequests::default();
        r.set_margin("Menu", 5.0, 5.0, 0.0, 0.0);
        r.set_margin("Menu", 9.0, 9.0, 0.0, 0.0);
        let drained = r.0.drain();
        assert_eq!(
            drained,
            vec![
                ("Menu".to_string(), [5.0, 5.0, 0.0, 0.0]),
                ("Menu".to_string(), [9.0, 9.0, 0.0, 0.0]),
            ]
        );
    }
}

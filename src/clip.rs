//! Polygon clips: clip a named element to a polygon set from Rust.
//!
//! Add a [`NoesisClip`] component to a [`NoesisView`](crate::NoesisView) camera
//! entity or a [`UiPanel`](crate::UiPanel) entity. Its `clips` map holds a
//! polygon per element `x:Name`, in the element's own coordinates. When the
//! component changes, the scene is rebuilt, or the panel mounts, the reconcile
//! system in [`NoesisSet::Apply`] sets each polygon as the element's `Clip`
//! (through [`set_clip_points`](noesis_runtime::view::FrameworkElement::set_clip_points)).
//! Rewriting a polygon every frame animates the clip.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisClip::new().clip("Portrait", vec![[0.0, 0.0], [64.0, 0.0], [32.0, 64.0]]),
//! );
//! ```
//!
//! An empty polygon clears the clip. Removing an entry from the map does not:
//! the element keeps its last clip. Unknown names log a warning.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view clip bridge. Add it to a [`NoesisView`](crate::NoesisView) or
/// [`UiPanel`](crate::UiPanel) entity; see the [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisClip {
    /// Clip polygon per element `x:Name`, as `[x, y]` points in the element's
    /// own coordinates; the polygon closes itself. An empty `Vec` clears the
    /// clip. One or two points is rejected with a warning.
    pub clips: HashMap<String, Vec<[f32; 2]>>,
}

impl NoesisClip {
    /// An empty bridge. Chain [`clip`](Self::clip) to add polygons.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s clip to the closed polygon through `points`
    /// (empty clears it).
    #[must_use]
    pub fn clip(mut self, name: impl Into<String>, points: Vec<[f32; 2]>) -> Self {
        self.clips.insert(name.into(), points);
        self
    }

    /// In-place form of [`clip`](Self::clip), for a system holding
    /// `&mut NoesisClip`. Applied by the next reconcile.
    pub fn set(&mut self, name: impl Into<String>, points: Vec<[f32; 2]>) {
        self.clips.insert(name.into(), points);
    }
}

/// Apply each entity's [`NoesisClip`] when it changed, its scene was rebuilt, or
/// its panel mounted.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_clip_bridge(
    views: Query<(Entity, Ref<NoesisClip>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, clip) in &views {
        if clip.is_changed()
            || state.scene_rebuilt_this_frame(entity)
            || state.panel_mounted_this_frame(entity)
        {
            state.apply_clip_for(entity, &clip.clips);
        }
    }
}

/// Registers the [`NoesisClip`] reconcile system. Added by
/// [`crate::NoesisPlugin`].
pub struct NoesisClipPlugin;

impl Plugin for NoesisClipPlugin {
    fn build(&self, app: &mut App) {
        // After `sync_panels`, which sets `panel_mounted_this_frame`, so a panel
        // applies the same frame it mounts.
        app.add_systems(
            PostUpdate,
            sync_clip_bridge
                .in_set(NoesisSet::Apply)
                .after(crate::panel::sync_panels),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_clips() {
        let c = NoesisClip::new()
            .clip("Panel", vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0]])
            .clip("Cleared", vec![]);
        assert_eq!(c.clips.get("Panel").map(Vec::len), Some(3));
        assert_eq!(c.clips.get("Cleared").map(Vec::len), Some(0));
    }
}

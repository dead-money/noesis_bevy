//! Per-view clip bridge: imperative polygon clips against named XAML elements on a
//! single [`NoesisView`](crate::NoesisView). The clip counterpart of
//! [`crate::geometry`] — where that assigns a `Path`'s `Data`, this assigns any
//! element's [`UIElement::Clip`](noesis_runtime::view::FrameworkElement::set_clip_points).
//!
//! Add a [`NoesisClip`] component to the view's camera entity. Its `clips` map is
//! the desired clip polygon per `x:Name`, applied whenever the component changes
//! (Bevy change detection). Each set of points (in the element's own coordinate
//! space) becomes a filled Noesis `StreamGeometry` set as the element's `Clip`; an
//! empty polygon clears the clip. Rewriting the polygon each frame animates a
//! moving clip region.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies the writes against
//! that view's live scene. No cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view clip bridge. Attach to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisClip {
    /// Desired clip polygon per element `x:Name`. Written to the view's elements
    /// whenever this component changes. Each value is a closed polygon through
    /// `[x, y]` pairs in the element's own coordinate space; an empty `Vec` clears
    /// the element's clip. A degenerate polygon (1 or 2 points) is skipped with a
    /// warning on apply.
    pub clips: HashMap<String, Vec<[f32; 2]>>,
}

impl NoesisClip {
    /// Creates an empty bridge with no clips. Chain [`clip`](Self::clip) to add
    /// polygons before inserting it on the [`NoesisView`](crate::NoesisView) camera.
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

    /// Set element `name`'s clip from a system holding `&mut NoesisClip`. The
    /// runtime counterpart of [`clip`](Self::clip): the next reconcile assigns the
    /// polygon (or clears it, when empty) on the live element.
    pub fn set(&mut self, name: impl Into<String>, points: Vec<[f32; 2]>) {
        self.clips.insert(name.into(), points);
    }
}

/// Reconcile every view's [`NoesisClip`]: apply the desired clip writes when the
/// component changed.
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

/// Wires the per-view clip bridge. Added transitively by [`crate::NoesisPlugin`].
pub struct NoesisClipPlugin;

impl Plugin for NoesisClipPlugin {
    fn build(&self, app: &mut App) {
        // After `sync_panels` so a panel's `NoesisClip` re-applies the same frame
        // its fragment mounts (the bridge reads `panel_mounted_this_frame`, set by
        // `sync_panels`); mirrors the geometry bridge's ordering.
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

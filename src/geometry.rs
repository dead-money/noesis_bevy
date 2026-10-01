//! Draws Rust-supplied polylines into named XAML `Path` elements.
//!
//! Add a [`NoesisGeometry`] to the [`NoesisView`](crate::NoesisView) camera
//! entity (or a [`UiPanel`](crate::panel::UiPanel) entity). Each entry in
//! [`paths`](NoesisGeometry::paths) becomes a Noesis `StreamGeometry` assigned
//! to that `Path`'s `Data`, so a live graph or oscilloscope trace draws as real
//! vector lines.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisGeometry::new()
//!         .path("ScopeTrace", vec![[0.0, 1.0], [2.0, 3.0]]),
//! );
//! ```
//!
//! The geometry is written when the component changes and again after a scene
//! rebuild or panel mount, in [`NoesisSet::Apply`] on the main thread. Removing
//! an entry from the map leaves the element's last geometry in place.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Polyline geometry for named `Path` elements. See the [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisGeometry {
    /// Open polyline per `Path` `x:Name`, as `[x, y]` points in the `Path`'s
    /// local coordinates. A target that isn't a `Path`, or fewer than two
    /// points, is skipped with a warning.
    pub paths: HashMap<String, Vec<[f32; 2]>>,
}

impl NoesisGeometry {
    /// An empty bridge. Chain [`path`](Self::path) to add geometry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets `name`'s geometry to an open polyline through `points`.
    #[must_use]
    pub fn path(mut self, name: impl Into<String>, points: Vec<[f32; 2]>) -> Self {
        self.paths.insert(name.into(), points);
        self
    }

    /// In-place form of [`path`](Self::path), for systems holding
    /// `&mut NoesisGeometry`.
    pub fn draw(&mut self, name: impl Into<String>, points: Vec<[f32; 2]>) {
        self.paths.insert(name.into(), points);
    }
}

/// Writes each changed [`NoesisGeometry`], and every one whose scene was
/// rebuilt or panel mounted this frame.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_geometry_bridge(
    views: Query<(Entity, Ref<NoesisGeometry>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, geometry) in &views {
        if geometry.is_changed()
            || state.scene_rebuilt_this_frame(entity)
            || state.panel_mounted_this_frame(entity)
        {
            state.apply_geometry_for(entity, &geometry.paths);
        }
    }
}

/// Registers [`NoesisGeometry`]'s system. Added by
/// [`NoesisPlugin`](crate::NoesisPlugin).
pub struct NoesisGeometryPlugin;

impl Plugin for NoesisGeometryPlugin {
    fn build(&self, app: &mut App) {
        // After `sync_panels`, which sets `panel_mounted_this_frame`.
        app.add_systems(
            PostUpdate,
            sync_geometry_bridge
                .in_set(NoesisSet::Apply)
                .after(crate::panel::sync_panels),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_paths() {
        let g = NoesisGeometry::new()
            .path("ScopeTrace", vec![[0.0, 1.0], [2.0, 3.0]])
            .path("Grid", vec![[4.0, 5.0]]);
        assert_eq!(
            g.paths.get("ScopeTrace"),
            Some(&vec![[0.0, 1.0], [2.0, 3.0]]),
        );
        assert_eq!(g.paths.get("Grid"), Some(&vec![[4.0, 5.0]]));
    }
}

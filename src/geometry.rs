//! Per-view geometry bridge: imperative vector polyline writes against named
//! XAML `Path` elements on a single [`NoesisView`](crate::NoesisView). The
//! geometry counterpart of [`crate::text`].
//!
//! Add a [`NoesisGeometry`] component to the view's camera entity. Its `paths`
//! map is the desired geometry per `x:Name`, applied to the view's `Path`
//! elements whenever the component changes (Bevy change detection). Each set of
//! points becomes a Noesis `StreamGeometry` assigned as the `Path`'s `Data`, so
//! a live oscilloscope (or any Rust-driven graph) draws a genuine line instead
//! of rasterising to a text canvas.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisGeometry::new()
//!         .path("ScopeTrace", vec![[0.0, 1.0], [2.0, 3.0]]),
//! );
//! ```
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies the writes
//! against that view's live scene. No cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view geometry bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisGeometry {
    /// Desired geometry per element `x:Name`. Written to the view's `Path`
    /// elements whenever this component changes. Each value is an open polyline
    /// through `[x, y]` pairs in the Path's local coordinate space. Each target
    /// must be a `Path`; a type mismatch (or fewer than two points) is skipped
    /// with a warning on apply.
    pub paths: HashMap<String, Vec<[f32; 2]>>,
}

impl NoesisGeometry {
    /// Creates an empty bridge with no paths. Chain [`path`](Self::path) to add
    /// geometry before inserting it on the [`NoesisView`](crate::NoesisView) camera.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s `Path` geometry to an open polyline through
    /// `points`.
    #[must_use]
    pub fn path(mut self, name: impl Into<String>, points: Vec<[f32; 2]>) -> Self {
        self.paths.insert(name.into(), points);
        self
    }
}

/// Reconcile every view's [`NoesisGeometry`]: apply the desired geometry writes
/// when the component changed.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_geometry_bridge(
    views: Query<(Entity, Ref<NoesisGeometry>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, geometry) in &views {
        if geometry.is_changed() {
            state.apply_geometry_for(entity, &geometry.paths);
        }
    }
}

/// Wires the per-view geometry bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisGeometryPlugin;

impl Plugin for NoesisGeometryPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_geometry_bridge.in_set(NoesisSet::Apply));
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

//! Imperative geometry writes against a named XAML `Path` element — a real
//! vector polyline fed from Rust, the geometry counterpart of [`crate::text`].
//!
//! [`NoesisGeometryRequests`] is a main-app push queue of `(x:Name, points)`
//! writes. It is drained on the render side each frame and applied via
//! `dm_noesis_runtime::view::FrameworkElement::set_path_points`, which builds a
//! Noesis `StreamGeometry` and assigns it as the `Path`'s `Data`. Same shape as
//! [`crate::text::NoesisTextRequests`] — infrequent, main-driven writes through a
//! single queue — so the live oscilloscope (or any Rust-driven graph) can draw a
//! genuine line instead of rasterising to a text canvas.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy_render::{
    Render, RenderApp, RenderSystems,
    extract_resource::{ExtractResource, ExtractResourcePlugin},
};

use crate::render::NoesisRenderState;

/// Main-app-side queue of pending geometry writes. Push via [`Self::set_polyline`];
/// the render world drains and applies during `RenderSystems::Prepare`.
///
/// Cheap to keep around even when no writes are pending — the underlying `Vec`
/// only allocates on first push.
#[derive(Resource, Clone, Default)]
pub struct NoesisGeometryRequests(SharedGeometryQueue);

impl NoesisGeometryRequests {
    /// Queue a write setting `name`'s `Path` geometry to an open polyline through
    /// `points` (`[x, y]` pairs in the Path's local coordinate space). The
    /// element must be a `Path`; a type mismatch (or fewer than two points) is
    /// skipped with a warning on apply.
    pub fn set_polyline(&self, name: impl Into<String>, points: Vec<[f32; 2]>) {
        self.0.push(name.into(), points);
    }
}

impl ExtractResource for NoesisGeometryRequests {
    type Source = NoesisGeometryRequests;
    fn extract_resource(source: &Self::Source) -> Self {
        source.clone()
    }
}

/// A pending `(x:Name, points)` geometry write.
type GeometryWrite = (String, Vec<[f32; 2]>);

#[derive(Clone, Default)]
pub(crate) struct SharedGeometryQueue(Arc<Mutex<Vec<GeometryWrite>>>);

impl SharedGeometryQueue {
    fn push(&self, name: String, points: Vec<[f32; 2]>) {
        self.0
            .lock()
            .expect("SharedGeometryQueue poisoned")
            .push((name, points));
    }

    pub(crate) fn drain(&self) -> Vec<GeometryWrite> {
        let mut guard = self.0.lock().expect("SharedGeometryQueue poisoned");
        if guard.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *guard)
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn apply_geometry_writes(
    requests: Option<Res<NoesisGeometryRequests>>,
    state: Option<ResMut<NoesisRenderState>>,
) {
    let (Some(requests), Some(mut state)) = (requests, state) else {
        return;
    };
    state.apply_geometry_writes(&requests.0);
}

/// Wires the geometry-write bridge. Insert via [`crate::NoesisPlugin`] (which adds
/// it transitively).
pub struct NoesisGeometryPlugin;

impl Plugin for NoesisGeometryPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<NoesisGeometryRequests>()
            .add_plugins(ExtractResourcePlugin::<NoesisGeometryRequests>::default());

        let Some(render_app) = app.get_sub_app_mut(RenderApp) else {
            return;
        };

        render_app.add_systems(Render, apply_geometry_writes.in_set(RenderSystems::Prepare));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_write_queue_drain_round_trip() {
        let q = SharedGeometryQueue::default();
        q.push("ScopeTrace".into(), vec![[0.0, 1.0], [2.0, 3.0]]);
        let drained = q.drain();
        assert_eq!(
            drained,
            vec![("ScopeTrace".to_string(), vec![[0.0, 1.0], [2.0, 3.0]])],
        );
        assert!(q.drain().is_empty(), "second drain should be empty");
    }
}

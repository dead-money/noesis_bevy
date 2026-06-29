//! Per-view `Margin` writes against named XAML elements: the positioning
//! primitive for floating panels (context menus, popups, tooltips) that must
//! follow gameplay coordinates.
//!
//! Noesis's `Canvas.Left`/`Top` attached property isn't surfaced through the
//! shim, but `FrameworkElement::Margin` is a plain dependency property. A
//! `Left`/`Top`-anchored element with `Margin = (x, y, 0, 0)` lands its corner
//! at `(x, y)`, so a single margin write positions a floating element anywhere
//! in the view. Coordinates are Noesis *view* DIPs (the `NoesisScene::size`
//! space), so a caller working in window pixels scales by `view_size /
//! window_size` first, the same mapping the input bridge uses.
//!
//! Add a [`NoesisLayout`] component to the view's camera entity. Its `margins`
//! map is the desired `Margin` per `x:Name`, applied to the view's elements
//! whenever the component changes (Bevy change detection). This is a write-only
//! bridge: there is no read-back message.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisLayout::new().margin("PartMenu", [cursor_x, cursor_y, 0.0, 0.0]),
//! );
//! ```
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies the margin
//! writes against that view's live scene, no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Left, top, right, bottom offsets in view DIPs.
pub type Margin = [f32; 4];

/// Per-view layout bridge. Attach to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisLayout {
    /// Desired `Margin` per element `x:Name`, as `[left, top, right, bottom]` in
    /// view DIPs. Written to the view's elements whenever this component changes.
    pub margins: HashMap<String, Margin>,
}

impl NoesisLayout {
    /// An empty layout with no element margins. Build one up with
    /// [`margin`](Self::margin), then insert it on the [`NoesisView`](crate::NoesisView) camera.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s `Margin` to `margin`
    /// (`[left, top, right, bottom]`, view DIPs).
    #[must_use]
    pub fn margin(mut self, name: impl Into<String>, margin: Margin) -> Self {
        self.margins.insert(name.into(), margin);
        self
    }

    /// Set element `name`'s `Margin` from a system holding `&mut NoesisLayout`.
    /// The runtime counterpart of [`margin`](Self::margin): the next reconcile
    /// applies it to the live element.
    pub fn write(&mut self, name: impl Into<String>, margin: Margin) {
        self.margins.insert(name.into(), margin);
    }
}

/// Reconcile every view's [`NoesisLayout`]: apply desired margin writes when the
/// component changed. Write-only, no read-back message.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_layout_bridge(
    views: Query<(Entity, Ref<NoesisLayout>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, layout) in &views {
        if layout.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_layout_for(entity, &layout.margins);
        }
    }
}

/// Wires the per-view layout bridge. Added transitively by [`crate::NoesisPlugin`].
pub struct NoesisLayoutPlugin;

impl Plugin for NoesisLayoutPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_layout_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_margins() {
        let l = NoesisLayout::new()
            .margin("Menu", [10.0, 20.0, 0.0, 0.0])
            .margin("Tip", [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(l.margins.get("Menu"), Some(&[10.0, 20.0, 0.0, 0.0]));
        assert_eq!(l.margins.get("Tip"), Some(&[1.0, 2.0, 3.0, 4.0]));
    }
}

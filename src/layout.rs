//! Sets `Margin` on named XAML elements, for positioning floating panels
//! (context menus, popups, tooltips) at gameplay coordinates.
//!
//! An element aligned `Left`/`Top` with `Margin = (x, y, 0, 0)` puts its
//! top-left corner at `(x, y)`, so one margin write places it anywhere in the
//! view. Coordinates are DIPs: view pixels
//! ([`NoesisView::size`](crate::NoesisView::size)) divided by
//! [`NoesisView::scale`](crate::NoesisView::scale). From window logical
//! pixels, multiply by `view_size / window_size` (as the
//! [input bridge](crate::input#coordinates) does), then divide by the scale.
//!
//! Add a [`NoesisLayout`] to a [`NoesisView`](crate::NoesisView) camera
//! entity or a [`UiPanel`](crate::UiPanel) entity. It is write-only: there is
//! no read-back message.
//!
//! ```no_run
//! # use bevy::prelude::*;
//! # use noesis_bevy::NoesisLayout;
//! # fn place(mut commands: Commands, view: Entity, cursor_x: f32, cursor_y: f32) {
//! commands.entity(view).insert(
//!     NoesisLayout::new().margin("PartMenu", [cursor_x, cursor_y, 0.0, 0.0]),
//! );
//! # }
//! ```

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// `[left, top, right, bottom]` offsets in view DIPs (see the [module docs](self)).
pub type Margin = [f32; 4];

/// Element margins by `x:Name`, for a [`NoesisView`](crate::NoesisView) or
/// [`UiPanel`](crate::UiPanel) entity.
///
/// Every margin is written when the component changes, after a scene rebuild,
/// and when a panel mounts. Removing an entry leaves the element at its last
/// margin. Unknown names log a warning.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisLayout {
    /// `Margin` per element `x:Name`.
    pub margins: HashMap<String, Margin>,
}

impl NoesisLayout {
    /// An empty layout. Build it up with [`margin`](Self::margin).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets element `name`'s `Margin`.
    #[must_use]
    pub fn margin(mut self, name: impl Into<String>, margin: Margin) -> Self {
        self.margins.insert(name.into(), margin);
        self
    }

    /// In-place form of [`margin`](Self::margin), for a system holding
    /// `&mut NoesisLayout`.
    pub fn write(&mut self, name: impl Into<String>, margin: Margin) {
        self.margins.insert(name.into(), margin);
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_layout_bridge(
    views: Query<(Entity, Ref<NoesisLayout>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, layout) in &views {
        if layout.is_changed()
            || state.scene_rebuilt_this_frame(entity)
            || state.panel_mounted_this_frame(entity)
        {
            state.apply_layout_for(entity, &layout.margins);
        }
    }
}

/// Runs the [`NoesisLayout`] bridge in [`NoesisSet::Apply`].
/// [`NoesisPlugin`](crate::NoesisPlugin) adds it.
pub struct NoesisLayoutPlugin;

impl Plugin for NoesisLayoutPlugin {
    fn build(&self, app: &mut App) {
        // After `sync_panels`, which sets `panel_mounted_this_frame`, so a panel's
        // margins apply the frame it mounts.
        app.add_systems(
            PostUpdate,
            sync_layout_bridge
                .in_set(NoesisSet::Apply)
                .after(crate::panel::sync_panels),
        );
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

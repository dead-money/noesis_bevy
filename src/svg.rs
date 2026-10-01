//! Parses SVG path data in Rust and sizes a named element of a
//! [`NoesisView`](crate::NoesisView) to the outline's bounds.
//!
//! Add a [`NoesisSvg`] to the view's camera entity. Each entry in
//! [`sources`](NoesisSvg::sources) is parsed with
//! [`SvgPath`](noesis_runtime::svg::SvgPath); the named element's `Width` and
//! `Height` are set to the outline's measured width and height, and a
//! [`NoesisSvgChanged`] reports the exact bounds. This fits a placeholder
//! element to vector art without authoring XAML.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisSvg::new().path("Icon", "M0 0 L40 0 L40 20 Z"),
//! );
//! // Emits NoesisSvgChanged { name: "Icon", bounds: [0.0, 0.0, 40.0, 20.0], .. }
//! ```
//!
//! Every entry is parsed and applied again whenever the component changes or the
//! view's scene is rebuilt. Removing an entry leaves the element's size as it
//! was.
//!
//! The bridge does not assign the geometry to a `Path`'s `Data`: the runtime has
//! no safe setter for it, and this crate forbids `unsafe`. For polylines, the
//! [`crate::geometry`] bridge writes `Path` points directly.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// SVG sources for named elements. Add to a [`NoesisView`](crate::NoesisView)
/// camera entity; see the [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisSvg {
    /// SVG path-data string per element `x:Name` (may be scope-qualified,
    /// `"Host/Leaf"`). A name not in the live tree, or a source that fails to
    /// parse, is skipped with a warning.
    pub sources: HashMap<String, String>,
}

impl NoesisSvg {
    /// An empty bridge. Chain [`path`](Self::path) to add sources.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: set element `name`'s SVG source to the path-data string `svg`
    /// (e.g. `"M0 0 L40 0 L40 20 Z"`).
    #[must_use]
    pub fn path(mut self, name: impl Into<String>, svg: impl Into<String>) -> Self {
        self.sources.insert(name.into(), svg.into());
        self
    }

    /// Set element `name`'s SVG source. The `&mut` form of
    /// [`path`](Self::path), for systems that update the component.
    pub fn set_path(&mut self, name: impl Into<String>, svg: impl Into<String>) {
        self.sources.insert(name.into(), svg.into());
    }
}

/// An SVG source was parsed and applied to a named element. Emitted for every
/// entry that resolved and parsed each time the bridge applies, including when
/// the element rejected the `Width` / `Height` write (that case also warns).
#[derive(Message, Debug, Clone, PartialEq)]
pub struct NoesisSvgChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element was sized.
    pub view: Entity,
    /// `x:Name` of the element the SVG was applied to.
    pub name: String,
    /// Axis-aligned bounds of the parsed outline, `[x, y, width, height]`, in
    /// path units. Only width and height are applied to the element.
    pub bounds: [f32; 4],
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_svg_bridge(
    views: Query<(Entity, Ref<NoesisSvg>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisSvgChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, svg) in &views {
        if !svg.is_changed() && !state.scene_rebuilt_this_frame(entity) {
            continue;
        }
        for (name, bounds) in state.apply_svg_for(entity, &svg.sources) {
            changed.write(NoesisSvgChanged {
                view: entity,
                name,
                bounds,
            });
        }
    }
}

/// Registers the SVG bridge. Added by [`crate::NoesisPlugin`].
pub struct NoesisSvgPlugin;

impl Plugin for NoesisSvgPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisSvgChanged>()
            .add_systems(PostUpdate, sync_svg_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_sources() {
        let s = NoesisSvg::new()
            .path("Icon", "M0 0 L40 0 L40 20 Z")
            .path("Glyph", "M0 0 L10 10");
        assert_eq!(
            s.sources.get("Icon").map(String::as_str),
            Some("M0 0 L40 0 L40 20 Z"),
        );
        assert_eq!(
            s.sources.get("Glyph").map(String::as_str),
            Some("M0 0 L10 10"),
        );
    }
}

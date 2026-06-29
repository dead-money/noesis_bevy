//! Per-view SVG bridge: drive a named XAML element from an SVG *path-data*
//! source string parsed in Rust. The SVG counterpart of the [`crate::geometry`]
//! polyline bridge.
//!
//! Add a [`NoesisSvg`] component to the view's camera entity. Its `sources` map
//! holds the desired SVG per `x:Name`, parsed and applied to the view's elements
//! whenever the component changes (Bevy change detection). Each source string is
//! parsed by the runtime's CPU-side `Noesis::SVGPath` parser
//! ([`noesis_runtime::svg::SvgPath`]); the parsed outline's *measured bounds*
//! then size the named element (`Width`/`Height`), so a Rust system can fit a
//! placeholder element to vector-art dimensions without authoring XAML.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisSvg::new().path("Icon", "M0 0 L40 0 L40 20 Z"),
//! );
//! ```
//!
//! Unlike the write-only bridges, this one *reads back* the exact bounds the
//! runtime measured for each parsed source and emits a [`NoesisSvgChanged`]
//! message. A known SVG path yields exact known bounds
//! (`"M0 0 L40 0 L40 20 Z"` produces `[0, 0, 40, 20]`); a missing element
//! (`x:Name` absent from the live tree) or an unparseable source emits nothing.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component, parses and applies the
//! sources against that view's live scene, and emits messages directly, with no
//! cross-world queues.
//!
//! ## Why bounds, not `Path.Data`
//!
//! Assigning the parsed geometry straight onto a `Path`'s `Data` DP needs the
//! runtime's `unsafe` `set_component` (a `BaseComponent*` hand-off); this crate
//! is `unsafe_code = forbid` and the runtime exposes no *safe* element
//! `Geometry`/`Data` setter (only `set_path_points` for raw polylines). So the
//! bridge applies the SVG's measured size, a safe and observable effect, and
//! surfaces the exact bounds for the caller.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Per-view SVG bridge. Attach to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisSvg {
    /// Desired SVG *path-data* source per element `x:Name`. Parsed and applied
    /// to the view's elements whenever this component changes. Writes to the
    /// same name apply last-wins. A name absent from the live tree, or a source
    /// that fails to parse, is skipped with a warning on apply.
    pub sources: HashMap<String, String>,
}

impl NoesisSvg {
    /// Creates an empty bridge with no sources. Chain [`path`](Self::path) to
    /// add element sources, then insert it on the [`NoesisView`](crate::NoesisView) camera.
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

    /// Set element `name`'s SVG source from a system holding `&mut NoesisSvg`. The
    /// runtime counterpart of [`path`](Self::path): the next reconcile parses it
    /// and sizes the live element.
    pub fn set_path(&mut self, name: impl Into<String>, svg: impl Into<String>) {
        self.sources.insert(name.into(), svg.into());
    }
}

/// Emitted when a named element's SVG source is parsed and applied. Carries the
/// exact axis-aligned bounds (`[x, y, width, height]`) the runtime measured for
/// the parsed outline, the observable proof the source parsed and routed to a
/// live element. An unparseable source or a name absent from the live tree emits
/// nothing. Read with `MessageReader<NoesisSvgChanged>`.
#[derive(Message, Debug, Clone, PartialEq)]
pub struct NoesisSvgChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element was sized.
    pub view: Entity,
    /// `x:Name` of the element the SVG was applied to.
    pub name: String,
    /// Measured bounds of the parsed SVG outline, `[x, y, width, height]`.
    pub bounds: [f32; 4],
}

/// Reconcile every view's [`NoesisSvg`]: when the component changed, parse each
/// source, size the named element to the measured bounds, and emit a
/// [`NoesisSvgChanged`] per element that resolved and parsed.
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

/// Wires the per-view SVG bridge. Added transitively by [`crate::NoesisPlugin`].
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

//! Builds Noesis vector shapes (`Rectangle`, `Ellipse`, `Line`) in Rust and
//! places each one inside a named container element of a
//! [`NoesisView`](crate::NoesisView).
//!
//! Add a [`NoesisShapes`] to the view's camera entity. Its
//! [`shapes`](NoesisShapes::shapes) map gives the shape for each container
//! `x:Name`. The container may be a `ContentControl` (the shape becomes its
//! `Content`) or a `Border` / `Decorator` (the shape becomes its `Child`).
//! To edit an existing `Path` instead, use the [`crate::geometry`] bridge.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisShapes::new()
//!         .rectangle("Host", 40.0, 24.0)
//!         .ellipse("Dot", 8.0, 8.0),
//! );
//! ```
//!
//! Whenever the component changes, or the view's scene is rebuilt, every entry
//! builds a new shape object that replaces the container's current content.
//! Removing an entry does not clear its container; the last shape stays. There
//! is no read-back message. To observe the effect, watch the container's
//! `ActualWidth` / `ActualHeight` with [`NoesisDp`](crate::dp::NoesisDp).

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// The kind of Noesis [`Shape`](noesis_runtime::shapes::Shape) to build and its
/// geometry, in device-independent pixels. Noesis has no `Polygon` or
/// `Polyline`; use the [`crate::geometry`] bridge for polylines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShapeKind {
    /// An axis-aligned rectangle, optionally with rounded corners.
    Rectangle {
        /// Width.
        width: f32,
        /// Height.
        height: f32,
        /// Horizontal corner radius. `0.0` for square corners.
        radius_x: f32,
        /// Vertical corner radius. `0.0` for square corners.
        radius_y: f32,
    },
    /// An ellipse filling a `width` × `height` box.
    Ellipse {
        /// Width of the bounding box.
        width: f32,
        /// Height of the bounding box.
        height: f32,
    },
    /// A straight line from `(x1, y1)` to `(x2, y2)`, in the shape's own
    /// coordinates.
    Line {
        /// X coordinate of the start point.
        x1: f32,
        /// Y coordinate of the start point.
        y1: f32,
        /// X coordinate of the end point.
        x2: f32,
        /// Y coordinate of the end point.
        y2: f32,
    },
}

/// A shape to build: its geometry plus optional solid paint. Colors are
/// `[r, g, b, a]`, each `0.0..=1.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeSpec {
    /// Geometry.
    pub kind: ShapeKind,
    /// Solid fill color. `None` leaves `Fill` unset (no interior).
    pub fill: Option<[f32; 4]>,
    /// Solid outline color. `None` leaves `Stroke` unset (no outline). A
    /// `Line` draws nothing without one.
    pub stroke: Option<[f32; 4]>,
    /// Outline width in DIPs. `None` keeps the Noesis default.
    pub stroke_thickness: Option<f32>,
}

impl ShapeSpec {
    /// A spec for `kind` with no fill, stroke, or explicit thickness.
    #[must_use]
    pub fn new(kind: ShapeKind) -> Self {
        Self {
            kind,
            fill: None,
            stroke: None,
            stroke_thickness: None,
        }
    }

    /// Builder: paint the shape's interior with solid `rgba`.
    #[must_use]
    pub fn with_fill(mut self, rgba: [f32; 4]) -> Self {
        self.fill = Some(rgba);
        self
    }

    /// Builder: paint the shape's outline with solid `rgba`.
    #[must_use]
    pub fn with_stroke(mut self, rgba: [f32; 4]) -> Self {
        self.stroke = Some(rgba);
        self
    }

    /// Builder: set the outline width.
    #[must_use]
    pub fn with_stroke_thickness(mut self, thickness: f32) -> Self {
        self.stroke_thickness = Some(thickness);
        self
    }
}

/// Code-built shapes for named containers. Add to a
/// [`NoesisView`](crate::NoesisView) camera entity; see the
/// [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisShapes {
    /// Shape per container `x:Name` (may be scope-qualified, `"Host/Leaf"`).
    /// A name not in the live tree, or a container that takes neither
    /// `Content` nor a `Child`, is skipped with a warning.
    pub shapes: HashMap<String, ShapeSpec>,
}

impl NoesisShapes {
    /// An empty set. Chain [`rectangle`](Self::rectangle),
    /// [`ellipse`](Self::ellipse), [`line`](Self::line), or
    /// [`insert`](Self::insert) to populate it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: put `spec` in container `name`. Use this form to add paint.
    #[must_use]
    pub fn insert(mut self, name: impl Into<String>, spec: ShapeSpec) -> Self {
        self.shapes.insert(name.into(), spec);
        self
    }

    /// Builder: assign a plain `width` × `height` rectangle to container `name`.
    #[must_use]
    pub fn rectangle(self, name: impl Into<String>, width: f32, height: f32) -> Self {
        self.insert(
            name,
            ShapeSpec::new(ShapeKind::Rectangle {
                width,
                height,
                radius_x: 0.0,
                radius_y: 0.0,
            }),
        )
    }

    /// Builder: assign a rounded `width` × `height` rectangle (corner radii
    /// `radius_x` / `radius_y`) to container `name`.
    #[must_use]
    pub fn rounded_rectangle(
        self,
        name: impl Into<String>,
        width: f32,
        height: f32,
        radius_x: f32,
        radius_y: f32,
    ) -> Self {
        self.insert(
            name,
            ShapeSpec::new(ShapeKind::Rectangle {
                width,
                height,
                radius_x,
                radius_y,
            }),
        )
    }

    /// Builder: assign a `width` × `height` ellipse to container `name`.
    #[must_use]
    pub fn ellipse(self, name: impl Into<String>, width: f32, height: f32) -> Self {
        self.insert(name, ShapeSpec::new(ShapeKind::Ellipse { width, height }))
    }

    /// Builder: assign a `(x1, y1)`-`(x2, y2)` line to container `name`.
    #[must_use]
    pub fn line(self, name: impl Into<String>, x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        self.insert(name, ShapeSpec::new(ShapeKind::Line { x1, y1, x2, y2 }))
    }

    /// Put `spec` in container `name`. The `&mut` form of
    /// [`insert`](Self::insert), for systems that update the component.
    pub fn set(&mut self, name: impl Into<String>, spec: ShapeSpec) {
        self.shapes.insert(name.into(), spec);
    }

    /// The `&mut` form of [`rectangle`](Self::rectangle).
    pub fn set_rectangle(&mut self, name: impl Into<String>, width: f32, height: f32) {
        self.set(
            name,
            ShapeSpec::new(ShapeKind::Rectangle {
                width,
                height,
                radius_x: 0.0,
                radius_y: 0.0,
            }),
        );
    }

    /// The `&mut` form of [`rounded_rectangle`](Self::rounded_rectangle).
    pub fn set_rounded_rectangle(
        &mut self,
        name: impl Into<String>,
        width: f32,
        height: f32,
        radius_x: f32,
        radius_y: f32,
    ) {
        self.set(
            name,
            ShapeSpec::new(ShapeKind::Rectangle {
                width,
                height,
                radius_x,
                radius_y,
            }),
        );
    }

    /// The `&mut` form of [`ellipse`](Self::ellipse).
    pub fn set_ellipse(&mut self, name: impl Into<String>, width: f32, height: f32) {
        self.set(name, ShapeSpec::new(ShapeKind::Ellipse { width, height }));
    }

    /// The `&mut` form of [`line`](Self::line).
    pub fn set_line(&mut self, name: impl Into<String>, x1: f32, y1: f32, x2: f32, y2: f32) {
        self.set(name, ShapeSpec::new(ShapeKind::Line { x1, y1, x2, y2 }));
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_shapes_bridge(
    views: Query<(Entity, Ref<NoesisShapes>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, shapes) in &views {
        if shapes.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_shapes_for(entity, &shapes.shapes);
        }
    }
}

/// Registers the shapes bridge. Added by [`crate::NoesisPlugin`].
pub struct NoesisShapesPlugin;

impl Plugin for NoesisShapesPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(PostUpdate, sync_shapes_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_shapes() {
        let s = NoesisShapes::new()
            .rectangle("Host", 40.0, 24.0)
            .ellipse("Dot", 8.0, 8.0)
            .line("Edge", 0.0, 0.0, 10.0, 5.0);
        assert_eq!(
            s.shapes.get("Host"),
            Some(&ShapeSpec::new(ShapeKind::Rectangle {
                width: 40.0,
                height: 24.0,
                radius_x: 0.0,
                radius_y: 0.0,
            })),
        );
        assert_eq!(
            s.shapes.get("Dot"),
            Some(&ShapeSpec::new(ShapeKind::Ellipse {
                width: 8.0,
                height: 8.0,
            })),
        );
        assert_eq!(
            s.shapes.get("Edge"),
            Some(&ShapeSpec::new(ShapeKind::Line {
                x1: 0.0,
                y1: 0.0,
                x2: 10.0,
                y2: 5.0,
            })),
        );
    }

    #[test]
    fn spec_builders_attach_paint() {
        let spec = ShapeSpec::new(ShapeKind::Ellipse {
            width: 4.0,
            height: 4.0,
        })
        .with_fill([1.0, 0.0, 0.0, 1.0])
        .with_stroke([0.0, 1.0, 0.0, 1.0])
        .with_stroke_thickness(2.0);
        assert_eq!(spec.fill, Some([1.0, 0.0, 0.0, 1.0]));
        assert_eq!(spec.stroke, Some([0.0, 1.0, 0.0, 1.0]));
        assert_eq!(spec.stroke_thickness, Some(2.0));
    }
}

//! Per-view shapes bridge: build a Noesis vector `Shape` (`Rectangle`,
//! `Ellipse`, or `Line`) entirely in Rust and assign it as the content of a
//! named XAML container element on a single [`NoesisView`](crate::NoesisView).
//!
//! This complements the [`crate::geometry`] polyline bridge. Geometry *mutates*
//! an existing `Path`'s `Data`; this bridge *constructs* a whole shape object
//! (via [`noesis_runtime::shapes`]) with its size, corner radii, fill, stroke,
//! and stroke thickness, then hands it to a named container. Rust can populate a
//! UI region with vector art without authoring it in XAML.
//!
//! Add a [`NoesisShapes`] component to the view's camera entity. Its `shapes`
//! map is the desired shape per container `x:Name`; each entry is built and
//! assigned whenever the component changes (Bevy change detection). The named
//! target may be either a `ContentControl` (the shape becomes its `Content`) or
//! a `Border`/`Decorator` (the shape becomes its `Child`); the bridge tries
//! `Content` first and falls back to the decorator child.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisShapes::new()
//!         .rectangle("Host", 40.0, 24.0)
//!         .ellipse("Dot", 8.0, 8.0),
//! );
//! ```
//!
//! Like [`crate::geometry`] this is write-only and carries no read-back message.
//! The assignment's effect is observable through a [`crate::dp::NoesisDp`] watch
//! on the *container's* `ActualWidth`/`ActualHeight`: a size-to-content `Border`
//! or `ContentControl` adopts the assigned shape's measured size.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies the writes
//! against that view's live scene, with no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Which kind of Noesis [`Shape`](noesis_runtime::shapes::Shape) to build, plus
/// its geometry. Noesis ships only `Rectangle`, `Ellipse`, `Line`, and `Path`
/// as shape elements (no `Polygon`/`Polyline`); polylines are covered by the
/// [`crate::geometry`] bridge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ShapeKind {
    /// An axis-aligned rectangle of `width` × `height` with optional corner
    /// radii `radius_x` / `radius_y`.
    Rectangle {
        /// Width of the rectangle, in device-independent pixels.
        width: f32,
        /// Height of the rectangle, in device-independent pixels.
        height: f32,
        /// Horizontal corner radius. `0.0` for square corners.
        radius_x: f32,
        /// Vertical corner radius. `0.0` for square corners.
        radius_y: f32,
    },
    /// An ellipse filling a `width` × `height` box.
    Ellipse {
        /// Width of the bounding box, in device-independent pixels.
        width: f32,
        /// Height of the bounding box, in device-independent pixels.
        height: f32,
    },
    /// A straight line from `(x1, y1)` to `(x2, y2)`.
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

/// A code-built shape: its geometry ([`ShapeKind`]) plus optional solid `fill` /
/// `stroke` colours (RGBA, each `0.0..=1.0`) and `stroke_thickness`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShapeSpec {
    /// The shape geometry to build.
    pub kind: ShapeKind,
    /// Optional solid fill colour (RGBA). `None` leaves `Fill` unset.
    pub fill: Option<[f32; 4]>,
    /// Optional solid stroke colour (RGBA). `None` leaves `Stroke` unset.
    pub stroke: Option<[f32; 4]>,
    /// Optional outline width. `None` leaves the shape's default thickness.
    pub stroke_thickness: Option<f32>,
}

impl ShapeSpec {
    /// A bare spec for `kind` with no fill, stroke, or explicit thickness.
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

/// Per-view shapes bridge. Attach to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisShapes {
    /// Desired shape per container `x:Name`. Built and assigned to the view's
    /// elements whenever this component changes. Writes to the same name apply
    /// last-wins. A name absent from the live tree, or a target that accepts
    /// neither `Content` nor a decorator `Child`, is skipped with a warning on
    /// apply.
    pub shapes: HashMap<String, ShapeSpec>,
}

impl NoesisShapes {
    /// An empty bridge with no shapes. Chain the builder methods
    /// ([`rectangle`](Self::rectangle), [`ellipse`](Self::ellipse),
    /// [`line`](Self::line), [`insert`](Self::insert)) to populate it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: assign a fully-specified [`ShapeSpec`] to container `name`.
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

    /// Assign a fully-specified [`ShapeSpec`] to container `name` from a system
    /// holding `&mut NoesisShapes`. The runtime counterpart of
    /// [`insert`](Self::insert): the next reconcile builds and assigns it to the
    /// live element.
    pub fn set(&mut self, name: impl Into<String>, spec: ShapeSpec) {
        self.shapes.insert(name.into(), spec);
    }

    /// Assign a plain `width` × `height` rectangle to container `name` from a
    /// system holding `&mut NoesisShapes`. The runtime counterpart of
    /// [`rectangle`](Self::rectangle).
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

    /// Assign a rounded `width` × `height` rectangle (corner radii `radius_x` /
    /// `radius_y`) to container `name` from a system holding `&mut NoesisShapes`.
    /// The runtime counterpart of [`rounded_rectangle`](Self::rounded_rectangle).
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

    /// Assign a `width` × `height` ellipse to container `name` from a system
    /// holding `&mut NoesisShapes`. The runtime counterpart of
    /// [`ellipse`](Self::ellipse).
    pub fn set_ellipse(&mut self, name: impl Into<String>, width: f32, height: f32) {
        self.set(name, ShapeSpec::new(ShapeKind::Ellipse { width, height }));
    }

    /// Assign a `(x1, y1)`-`(x2, y2)` line to container `name` from a system
    /// holding `&mut NoesisShapes`. The runtime counterpart of [`line`](Self::line).
    pub fn set_line(&mut self, name: impl Into<String>, x1: f32, y1: f32, x2: f32, y2: f32) {
        self.set(name, ShapeSpec::new(ShapeKind::Line { x1, y1, x2, y2 }));
    }
}

/// Reconcile every view's [`NoesisShapes`]: build and assign the desired shapes
/// when the component changed.
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

/// Wires the per-view shapes bridge. Added transitively by
/// [`crate::NoesisPlugin`].
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

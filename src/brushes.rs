//! Code-built brushes: paint a named element's `Background`, `Foreground`,
//! `Fill` or `Stroke` with a solid color or linear gradient built in Rust.
//!
//! Add a [`NoesisBrushes`] component to the [`NoesisView`](crate::NoesisView)
//! camera entity. Its `brushes` map holds the desired [`BrushSpec`] per
//! `(x:Name, BrushTarget)`. When the component changes, or the scene is
//! rebuilt, the reconcile system in [`NoesisSet::Apply`] writes every entry.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisBrushes::new()
//!         .solid("Panel", BrushTarget::Background, [1.0, 0.0, 0.0, 1.0]),
//! );
//! ```
//!
//! Writes are one-way: removing an entry leaves the element painted with its
//! last brush. An unknown name, or a target the element lacks (`Fill` on a
//! `Border`), logs a warning. The bridge acts on view entities only.
//!
//! Each frame the bridge reads back the brush on every listed target and sends a
//! [`NoesisBrushChanged`] when it differs from the last read, which confirms the
//! write landed. A solid brush reports its color; a gradient reports
//! [`BrushReadback::NonSolid`] because gradient stops can't be read back. A
//! target with no brush sends nothing.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

/// Which brush property a spec paints. An element without that property
/// rejects the write with a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BrushTarget {
    /// `Control` / `Panel` / `Border` `Background`.
    Background,
    /// Text control `Foreground`.
    Foreground,
    /// `Shape` `Fill` (e.g. `Rectangle`, `Ellipse`).
    Fill,
    /// `Shape` `Stroke`.
    Stroke,
}

impl BrushTarget {
    /// The dependency-property name this target paints.
    #[must_use]
    pub fn property(self) -> &'static str {
        match self {
            Self::Background => "Background",
            Self::Foreground => "Foreground",
            Self::Fill => "Fill",
            Self::Stroke => "Stroke",
        }
    }
}

/// One gradient stop: a `color` (`[r, g, b, a]`, each `0..=1`) at a normalized
/// `offset` (`0..=1`) along the gradient axis.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GradientStop {
    /// Position of the stop along the gradient axis, `0..=1`.
    pub offset: f32,
    /// Stop color `[r, g, b, a]`, each `0..=1`.
    pub color: [f32; 4],
}

impl GradientStop {
    /// Construct a stop from an `offset` and `color`.
    #[must_use]
    pub fn new(offset: f32, color: [f32; 4]) -> Self {
        Self { offset, color }
    }
}

/// A brush description. The live Noesis brush is built at apply time, so the
/// component stays plain data. Colors are `[r, g, b, a]` in `0..=1`.
#[derive(Debug, Clone, PartialEq)]
pub enum BrushSpec {
    /// A flat `SolidColorBrush` of `[r, g, b, a]` (each `0..=1`).
    Solid([f32; 4]),
    /// A `LinearGradientBrush` from `start` to `end`, painted through `stops`.
    /// Points are relative to the element's bounds: `[0, 0]` is top-left and
    /// `[1, 1]` bottom-right.
    LinearGradient {
        /// Gradient start point `[x, y]`.
        start: [f32; 2],
        /// Gradient end point `[x, y]`.
        end: [f32; 2],
        /// Gradient stops in axis order.
        stops: Vec<GradientStop>,
    },
}

/// Per-view brush bridge. Add it to a [`NoesisView`](crate::NoesisView) entity;
/// see the [module docs](self).
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisBrushes {
    /// Desired brush per `(x:Name, target)`. Every entry is rewritten whenever
    /// this component changes; removing an entry does not reset the element.
    pub brushes: HashMap<(String, BrushTarget), BrushSpec>,
}

impl NoesisBrushes {
    /// An empty bridge. Chain [`solid`](Self::solid) and
    /// [`linear_gradient`](Self::linear_gradient) to add targets.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: paint element `name`'s `target` with a flat `rgba`
    /// (`[r, g, b, a]`, each `0..=1`) solid color.
    #[must_use]
    pub fn solid(mut self, name: impl Into<String>, target: BrushTarget, rgba: [f32; 4]) -> Self {
        self.brushes
            .insert((name.into(), target), BrushSpec::Solid(rgba));
        self
    }

    /// Builder: paint element `name`'s `target` with a linear gradient from
    /// `start` to `end` through `stops`.
    #[must_use]
    pub fn linear_gradient(
        mut self,
        name: impl Into<String>,
        target: BrushTarget,
        start: [f32; 2],
        end: [f32; 2],
        stops: Vec<GradientStop>,
    ) -> Self {
        self.brushes.insert(
            (name.into(), target),
            BrushSpec::LinearGradient { start, end, stops },
        );
        self
    }

    /// In-place form of [`solid`](Self::solid), for a system holding
    /// `&mut NoesisBrushes`. Applied by the next reconcile.
    pub fn paint_solid(&mut self, name: impl Into<String>, target: BrushTarget, rgba: [f32; 4]) {
        self.brushes
            .insert((name.into(), target), BrushSpec::Solid(rgba));
    }

    /// In-place form of [`linear_gradient`](Self::linear_gradient), for a system
    /// holding `&mut NoesisBrushes`. Applied by the next reconcile.
    pub fn paint_linear_gradient(
        &mut self,
        name: impl Into<String>,
        target: BrushTarget,
        start: [f32; 2],
        end: [f32; 2],
        stops: Vec<GradientStop>,
    ) {
        self.brushes.insert(
            (name.into(), target),
            BrushSpec::LinearGradient { start, end, stops },
        );
    }
}

/// The brush read back from a painted target. A target with no brush produces
/// no readback at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BrushReadback {
    /// A `SolidColorBrush` with this `[r, g, b, a]` color.
    Solid([f32; 4]),
    /// Any other brush, such as a gradient. Its stops can't be read back.
    NonSolid,
}

/// Sent when the brush on a target listed in [`NoesisBrushes`] differs from the
/// last read, including the first read after it is painted. Changes made by
/// XAML (a style trigger, an animation) are reported too.
#[derive(Message, Debug, Clone)]
pub struct NoesisBrushChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose brush changed.
    pub view: Entity,
    /// `x:Name` of the painted element.
    pub name: String,
    /// The brush property that changed.
    pub target: BrushTarget,
    /// The brush now on the target.
    pub readback: BrushReadback,
}

/// Apply each view's [`NoesisBrushes`] when it changed or the scene was rebuilt,
/// then poll its targets and send [`NoesisBrushChanged`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_brushes_bridge(
    views: Query<(Entity, Ref<NoesisBrushes>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisBrushChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, brushes) in &views {
        if brushes.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_brushes_for(entity, &brushes.brushes);
        }
        for (name, target, readback) in state.poll_brush_reads_for(entity, &brushes.brushes) {
            changed.write(NoesisBrushChanged {
                view: entity,
                name,
                target,
                readback,
            });
        }
    }
}

/// Registers the [`NoesisBrushes`] reconcile system and [`NoesisBrushChanged`].
/// Added by [`crate::NoesisPlugin`].
pub struct NoesisBrushesPlugin;

impl Plugin for NoesisBrushesPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisBrushChanged>()
            .add_systems(PostUpdate, sync_brushes_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_collects_brushes() {
        let b = NoesisBrushes::new()
            .solid("Panel", BrushTarget::Background, [1.0, 0.0, 0.0, 1.0])
            .linear_gradient(
                "Bar",
                BrushTarget::Fill,
                [0.0, 0.0],
                [1.0, 0.0],
                vec![
                    GradientStop::new(0.0, [0.0, 0.0, 0.0, 1.0]),
                    GradientStop::new(1.0, [1.0, 1.0, 1.0, 1.0]),
                ],
            );
        assert_eq!(
            b.brushes
                .get(&("Panel".to_string(), BrushTarget::Background)),
            Some(&BrushSpec::Solid([1.0, 0.0, 0.0, 1.0])),
        );
        assert!(matches!(
            b.brushes.get(&("Bar".to_string(), BrushTarget::Fill)),
            Some(BrushSpec::LinearGradient { stops, .. }) if stops.len() == 2,
        ));
    }

    #[test]
    fn target_property_names() {
        assert_eq!(BrushTarget::Background.property(), "Background");
        assert_eq!(BrushTarget::Foreground.property(), "Foreground");
        assert_eq!(BrushTarget::Fill.property(), "Fill");
        assert_eq!(BrushTarget::Stroke.property(), "Stroke");
    }
}

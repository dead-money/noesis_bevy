//! Per-view code-built brush bridge — paint named XAML elements with brushes
//! constructed in Rust, no XAML authoring required. The brush counterpart of the
//! [`crate::dp`] / [`crate::geometry`] write bridges.
//!
//! Add a [`NoesisBrushes`] component to the view's camera entity. Its `brushes`
//! map is the desired brush per `(x:Name, target)` — applied to the view's
//! elements whenever the component changes (Bevy change detection). Each spec
//! becomes a freshly-built Noesis `Brush` (a `SolidColorBrush` or a
//! `LinearGradientBrush`) assigned through the element's typed brush sugar
//! (`set_background` / `set_foreground` / `set_fill` / `set_stroke`), so a
//! gameplay system can recolor a health bar or flash a panel from Rust.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisBrushes::new()
//!         .solid("Panel", BrushTarget::Background, [1.0, 0.0, 0.0, 1.0]),
//! );
//! ```
//!
//! Unlike the purely write-only bridges, this one also *polls back* the solid
//! color that actually landed on each target and emits a [`NoesisBrushChanged`]
//! message — a read-back that proves the assignment took (a gradient target has
//! no single color, so it reports nothing). The default (unset) Background is
//! null, so a missing apply / wrong-entity routing reads `None` and stays
//! silent; only a real assignment surfaces a message.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component, applies the brush writes,
//! polls the read-back, and emits messages directly — no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Target + spec
// ─────────────────────────────────────────────────────────────────────────────

/// Which Brush-typed dependency property a spec paints. Each maps to the
/// element's typed, safe brush sugar — so the bridge never touches the generic
/// (unsafe) `set_component` path, and a non-brush element simply reports a failed
/// assignment.
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

/// A code-built brush, declarative side. Resolved into a live Noesis `Brush`
/// only at apply time (on the Noesis thread), so the component stays plain data.
#[derive(Debug, Clone, PartialEq)]
pub enum BrushSpec {
    /// A flat `SolidColorBrush` of `[r, g, b, a]` (each `0..=1`).
    Solid([f32; 4]),
    /// A `LinearGradientBrush` along the line `start`..`end` (relative
    /// coordinates by default), painted through `stops`.
    LinearGradient {
        /// Gradient start point `[x, y]`.
        start: [f32; 2],
        /// Gradient end point `[x, y]`.
        end: [f32; 2],
        /// Gradient stops in axis order.
        stops: Vec<GradientStop>,
    },
}

// ─────────────────────────────────────────────────────────────────────────────
// Component
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view brush bridge. Attach to a [`NoesisView`](crate::NoesisView) entity.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisBrushes {
    /// Desired brush per `(x:Name, target)`. Written to the view's elements
    /// whenever this component changes. Writes to the same key apply last-wins.
    pub brushes: HashMap<(String, BrushTarget), BrushSpec>,
}

impl NoesisBrushes {
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
}

// ─────────────────────────────────────────────────────────────────────────────
// Read-back message
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when the solid color read back from a painted target differs from the
/// previous frame's snapshot. Proves a [`BrushSpec::Solid`] assignment landed
/// (gradients report no color). Read with `MessageReader<NoesisBrushChanged>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisBrushChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose brush changed.
    pub view: Entity,
    /// `x:Name` of the painted element.
    pub name: String,
    /// The property that was painted.
    pub target: BrushTarget,
    /// Solid color read back from the live brush, `[r, g, b, a]`.
    pub color: [f32; 4],
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisBrushes`]: apply desired brush writes when the
/// component changed, then poll back each spec'd target's solid color and emit
/// [`NoesisBrushChanged`].
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
        if brushes.is_changed() {
            state.apply_brushes_for(entity, &brushes.brushes);
        }
        for (name, target, color) in state.poll_brush_reads_for(entity, &brushes.brushes) {
            changed.write(NoesisBrushChanged {
                view: entity,
                name,
                target,
                color,
            });
        }
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the per-view brush bridge. Added transitively by [`crate::NoesisPlugin`].
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

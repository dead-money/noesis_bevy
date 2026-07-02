//! Per-view `RenderTransform` writes against named XAML elements: the
//! post-layout scale / skew / rotate / translate that drives UI motion
//! (a button that pops on hover, a panel that slides in, a spinning loader)
//! without touching layout.
//!
//! Noesis's `CompositeTransform` bundles the four 2D operations into one object,
//! applied in the canonical WPF order (scale → skew → rotate → translate) about
//! a shared center. We build one from Rust
//! ([`CompositeTransform`](noesis_runtime::transforms::CompositeTransform)) and
//! assign it to an element's `RenderTransform` via
//! [`FrameworkElement::set_render_transform`](noesis_runtime::view::FrameworkElement::set_render_transform).
//! `RenderTransform` is a render-time concern: it moves/scales the painted
//! pixels but leaves the element's measured/arranged bounds (`ActualWidth` …)
//! untouched, so it never disturbs surrounding layout.
//!
//! Add a [`NoesisTransform`] component to the view's camera entity. Its
//! `transforms` map is the desired [`TransformSpec`] per `x:Name`, applied to
//! the view's elements whenever the component changes (Bevy change detection).
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisTransform::new()
//!         .translate("Panel", 40.0, 0.0)   // slide right 40 DIP
//!         .scale("Icon", 1.5, 1.5)         // 150% pop
//!         .rotate("Spinner", 90.0),        // quarter turn
//! );
//! ```
//!
//! This is a **read-watch** bridge: besides applying the writes, it polls each
//! transformed element's *live* `RenderTransform` back from Noesis and emits a
//! [`NoesisTransformChanged`] carrying the values Noesis actually stored. The
//! read-back is element-sourced (it goes element → `RenderTransform` DP →
//! `CompositeTransform` object), so it confirms the write reached the engine
//! rather than echoing the component. An un-applied or mis-routed write leaves
//! the element with no `RenderTransform` and emits nothing.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies + polls against
//! that view's live scene, with no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::transforms::CompositeFields;

use crate::render::{NoesisRenderState, NoesisSet};

/// A 2D composite render transform: scale → skew → rotate → translate, applied
/// in that canonical order about a shared center `(CenterX, CenterY)`. Mirrors
/// XAML's `CompositeTransform`; the
/// [`Default`] is the identity (unit scale, no skew/rotation/translation).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformSpec {
    /// Translation `[x, y]` in view DIPs.
    pub translate: [f32; 2],
    /// Scale factors `[x, y]` (`1.0` = unchanged).
    pub scale: [f32; 2],
    /// Rotation in degrees, clockwise.
    pub rotation: f32,
    /// Center `[x, y]` (DIPs) that scale / skew / rotate pivot about.
    pub center: [f32; 2],
    /// Skew angles `[x, y]` in degrees.
    pub skew: [f32; 2],
}

impl Default for TransformSpec {
    fn default() -> Self {
        Self {
            translate: [0.0, 0.0],
            scale: [1.0, 1.0],
            rotation: 0.0,
            center: [0.0, 0.0],
            skew: [0.0, 0.0],
        }
    }
}

impl TransformSpec {
    /// Lower this spec into the runtime's flat [`CompositeFields`] for assignment.
    #[must_use]
    pub(crate) fn to_fields(self) -> CompositeFields {
        CompositeFields {
            center_x: self.center[0],
            center_y: self.center[1],
            scale_x: self.scale[0],
            scale_y: self.scale[1],
            skew_x: self.skew[0],
            skew_y: self.skew[1],
            rotation: self.rotation,
            translate_x: self.translate[0],
            translate_y: self.translate[1],
        }
    }

    /// Rebuild a spec from the runtime's [`CompositeFields`] read back off a live
    /// element. The inverse of [`Self::to_fields`].
    #[must_use]
    pub(crate) fn from_fields(f: CompositeFields) -> Self {
        Self {
            translate: [f.translate_x, f.translate_y],
            scale: [f.scale_x, f.scale_y],
            rotation: f.rotation,
            center: [f.center_x, f.center_y],
            skew: [f.skew_x, f.skew_y],
        }
    }
}

/// Per-view render-transform bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity. The builder methods *merge* into the per-name spec, so `translate`
/// then `scale` on the same element compose into one `CompositeTransform`.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisTransform {
    /// Desired [`TransformSpec`] per element `x:Name`. Assigned as each element's
    /// `RenderTransform` whenever this component changes.
    pub transforms: HashMap<String, TransformSpec>,
}

impl NoesisTransform {
    /// An empty bridge with no transforms queued. Chain the builder methods
    /// ([`translate`](Self::translate), [`scale`](Self::scale), etc.) to fill it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: replace `name`'s entire spec.
    #[must_use]
    pub fn set(mut self, name: impl Into<String>, spec: TransformSpec) -> Self {
        self.transforms.insert(name.into(), spec);
        self
    }

    /// Builder: set `name`'s translation, keeping any other fields already
    /// queued for it.
    #[must_use]
    pub fn translate(mut self, name: impl Into<String>, x: f32, y: f32) -> Self {
        self.entry(name).translate = [x, y];
        self
    }

    /// Builder: set `name`'s scale factors, keeping any other queued fields.
    #[must_use]
    pub fn scale(mut self, name: impl Into<String>, x: f32, y: f32) -> Self {
        self.entry(name).scale = [x, y];
        self
    }

    /// Builder: set `name`'s rotation (degrees, clockwise), keeping other fields.
    #[must_use]
    pub fn rotate(mut self, name: impl Into<String>, degrees: f32) -> Self {
        self.entry(name).rotation = degrees;
        self
    }

    /// Builder: set `name`'s pivot `(CenterX, CenterY)`, keeping other fields.
    #[must_use]
    pub fn center(mut self, name: impl Into<String>, x: f32, y: f32) -> Self {
        self.entry(name).center = [x, y];
        self
    }

    /// Builder: set `name`'s skew angles (degrees), keeping other fields.
    #[must_use]
    pub fn skew(mut self, name: impl Into<String>, x: f32, y: f32) -> Self {
        self.entry(name).skew = [x, y];
        self
    }

    /// Replace `name`'s entire spec from a system holding `&mut NoesisTransform`.
    /// The runtime counterpart of [`set`](Self::set): the next reconcile assigns
    /// it to the live element.
    pub fn write(&mut self, name: impl Into<String>, spec: TransformSpec) {
        self.transforms.insert(name.into(), spec);
    }

    /// Set `name`'s translation in place, keeping any other queued fields. The
    /// runtime counterpart of [`translate`](Self::translate).
    pub fn set_translate(&mut self, name: impl Into<String>, x: f32, y: f32) {
        self.entry(name).translate = [x, y];
    }

    /// Set `name`'s scale factors in place, keeping any other queued fields. The
    /// runtime counterpart of [`scale`](Self::scale).
    pub fn set_scale(&mut self, name: impl Into<String>, x: f32, y: f32) {
        self.entry(name).scale = [x, y];
    }

    /// Set `name`'s rotation (degrees, clockwise) in place, keeping other fields.
    /// The runtime counterpart of [`rotate`](Self::rotate).
    pub fn set_rotation(&mut self, name: impl Into<String>, degrees: f32) {
        self.entry(name).rotation = degrees;
    }

    /// Set `name`'s pivot `(CenterX, CenterY)` in place, keeping other fields.
    /// The runtime counterpart of [`center`](Self::center).
    pub fn set_center(&mut self, name: impl Into<String>, x: f32, y: f32) {
        self.entry(name).center = [x, y];
    }

    /// Set `name`'s skew angles (degrees) in place, keeping other fields. The
    /// runtime counterpart of [`skew`](Self::skew).
    pub fn set_skew(&mut self, name: impl Into<String>, x: f32, y: f32) {
        self.entry(name).skew = [x, y];
    }

    fn entry(&mut self, name: impl Into<String>) -> &mut TransformSpec {
        self.transforms.entry(name.into()).or_default()
    }
}

/// Emitted when a transformed element's live `RenderTransform` differs from the
/// previous frame's snapshot (and on the first poll after it is assigned). The
/// `spec` is read back from Noesis, so it reflects what the engine stored.
/// Read with `MessageReader<NoesisTransformChanged>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisTransformChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the element whose `RenderTransform` changed.
    pub name: String,
    /// The transform Noesis currently holds on the element.
    pub spec: TransformSpec,
}

/// Reconcile every view's [`NoesisTransform`]: assign desired render transforms
/// when the component changed, then poll the assigned elements' live transforms
/// and emit [`NoesisTransformChanged`].
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_transform_bridge(
    views: Query<(Entity, Ref<NoesisTransform>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisTransformChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, transform) in &views {
        if transform.is_changed()
            || state.scene_rebuilt_this_frame(entity)
            || state.panel_mounted_this_frame(entity)
        {
            state.apply_transforms_for(entity, &transform.transforms);
        }
        let names: Vec<&str> = transform.transforms.keys().map(String::as_str).collect();
        for (name, spec) in state.poll_transforms_for(entity, &names) {
            changed.write(NoesisTransformChanged {
                view: entity,
                name,
                spec,
            });
        }
    }
}

/// Wires the per-view render-transform bridge. Added transitively by
/// [`crate::NoesisPlugin`].
pub struct NoesisTransformPlugin;

impl Plugin for NoesisTransformPlugin {
    fn build(&self, app: &mut App) {
        // `sync_transform_bridge` runs after `sync_panels` so a panel's
        // `NoesisTransform` re-applies the same frame its fragment mounts (the
        // bridge reads `panel_mounted_this_frame`, set by `sync_panels`); mirrors
        // the focus bridge's ordering.
        app.add_message::<NoesisTransformChanged>().add_systems(
            PostUpdate,
            sync_transform_bridge
                .in_set(NoesisSet::Apply)
                .after(crate::panel::sync_panels),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_merges_fields_per_name() {
        let t = NoesisTransform::new()
            .translate("A", 10.0, 20.0)
            .scale("A", 2.0, 3.0)
            .rotate("A", 45.0)
            .translate("B", 1.0, 2.0);

        let a = t.transforms.get("A").copied().unwrap();
        assert_eq!(a.translate, [10.0, 20.0]);
        assert_eq!(a.scale, [2.0, 3.0]);
        assert_eq!(a.rotation, 45.0);
        assert_eq!(a.center, [0.0, 0.0]);
        assert_eq!(a.skew, [0.0, 0.0]);

        let b = t.transforms.get("B").copied().unwrap();
        assert_eq!(b.translate, [1.0, 2.0]);
        assert_eq!(b.scale, [1.0, 1.0]);
    }

    #[test]
    fn fields_round_trip() {
        let spec = TransformSpec {
            translate: [5.0, 6.0],
            scale: [2.0, 0.5],
            rotation: 30.0,
            center: [7.0, 8.0],
            skew: [1.0, -1.0],
        };
        assert_eq!(TransformSpec::from_fields(spec.to_fields()), spec);
    }
}

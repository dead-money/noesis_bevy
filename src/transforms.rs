//! Sets the `RenderTransform` of named elements: post-layout scale, skew,
//! rotation, and translation for UI motion (a button that pops on hover, a
//! panel that slides in, a spinner).
//!
//! Add a [`NoesisTransform`] to a [`NoesisView`](crate::NoesisView) camera, or
//! to a [`UiPanel`](crate::panel::UiPanel) entity. Each entry in
//! [`transforms`](NoesisTransform::transforms) becomes a `CompositeTransform`
//! assigned with
//! [`FrameworkElement::set_render_transform`](noesis_runtime::view::FrameworkElement::set_render_transform).
//! A render transform moves the painted pixels but not the element's layout
//! bounds (`ActualWidth`, arrangement), so it never disturbs its neighbors.
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
//! Whenever the component changes, the view's scene is rebuilt, or the panel
//! mounts, every entry is assigned again. Removing an entry leaves the last
//! transform on the element.
//!
//! On a view, the bridge reads each element's live `RenderTransform` back every
//! frame and emits [`NoesisTransformChanged`] when its values change. It reports
//! only while the element still holds the transform this bridge assigned, so
//! silence means the write did not land or something else replaced it. Panels
//! get no read-back.

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::transforms::CompositeFields;

use crate::render::{NoesisRenderState, NoesisSet};

/// A 2D `CompositeTransform`: scale, then skew, then rotate about
/// [`center`](Self::center), then translate. [`Default`] is the identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransformSpec {
    /// Translation `[x, y]` in DIPs.
    pub translate: [f32; 2],
    /// Scale factors `[x, y]` (`1.0` = unchanged).
    pub scale: [f32; 2],
    /// Rotation in degrees, clockwise on screen.
    pub rotation: f32,
    /// Pivot `[x, y]` for scale, skew, and rotation, in DIPs relative to the
    /// element's top-left corner.
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

/// Render transforms for named elements. Add to a
/// [`NoesisView`](crate::NoesisView) camera or a
/// [`UiPanel`](crate::panel::UiPanel) entity; see the [module docs](self).
///
/// The per-field builders and setters merge into the element's existing spec,
/// so `translate` then `scale` on one name give one transform with both.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisTransform {
    /// Transform per element `x:Name` (may be scope-qualified, `"Host/Leaf"`).
    /// A missing name or a non-`UIElement` is skipped with a warning.
    pub transforms: HashMap<String, TransformSpec>,
}

impl NoesisTransform {
    /// An empty bridge. Chain [`translate`](Self::translate),
    /// [`scale`](Self::scale), and the other builders to fill it.
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

    /// Builder: set `name`'s translation in DIPs, keeping its other fields.
    #[must_use]
    pub fn translate(mut self, name: impl Into<String>, x: f32, y: f32) -> Self {
        self.entry(name).translate = [x, y];
        self
    }

    /// Builder: set `name`'s scale factors, keeping its other fields.
    #[must_use]
    pub fn scale(mut self, name: impl Into<String>, x: f32, y: f32) -> Self {
        self.entry(name).scale = [x, y];
        self
    }

    /// Builder: set `name`'s rotation (degrees, clockwise), keeping its other
    /// fields.
    #[must_use]
    pub fn rotate(mut self, name: impl Into<String>, degrees: f32) -> Self {
        self.entry(name).rotation = degrees;
        self
    }

    /// Builder: set `name`'s pivot in DIPs, keeping its other fields.
    #[must_use]
    pub fn center(mut self, name: impl Into<String>, x: f32, y: f32) -> Self {
        self.entry(name).center = [x, y];
        self
    }

    /// Builder: set `name`'s skew angles (degrees), keeping its other fields.
    #[must_use]
    pub fn skew(mut self, name: impl Into<String>, x: f32, y: f32) -> Self {
        self.entry(name).skew = [x, y];
        self
    }

    /// Replace `name`'s entire spec. The `&mut` form of [`set`](Self::set),
    /// for systems that update the component.
    pub fn write(&mut self, name: impl Into<String>, spec: TransformSpec) {
        self.transforms.insert(name.into(), spec);
    }

    /// The `&mut` form of [`translate`](Self::translate).
    pub fn set_translate(&mut self, name: impl Into<String>, x: f32, y: f32) {
        self.entry(name).translate = [x, y];
    }

    /// The `&mut` form of [`scale`](Self::scale).
    pub fn set_scale(&mut self, name: impl Into<String>, x: f32, y: f32) {
        self.entry(name).scale = [x, y];
    }

    /// The `&mut` form of [`rotate`](Self::rotate).
    pub fn set_rotation(&mut self, name: impl Into<String>, degrees: f32) {
        self.entry(name).rotation = degrees;
    }

    /// The `&mut` form of [`center`](Self::center).
    pub fn set_center(&mut self, name: impl Into<String>, x: f32, y: f32) {
        self.entry(name).center = [x, y];
    }

    /// The `&mut` form of [`skew`](Self::skew).
    pub fn set_skew(&mut self, name: impl Into<String>, x: f32, y: f32) {
        self.entry(name).skew = [x, y];
    }

    fn entry(&mut self, name: impl Into<String>) -> &mut TransformSpec {
        self.transforms.entry(name.into()).or_default()
    }
}

/// A view element's live `RenderTransform` values changed, or were read for
/// the first time after assignment. `spec` is read from Noesis, not copied
/// from the component.
#[derive(Message, Debug, Clone)]
pub struct NoesisTransformChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the element whose `RenderTransform` changed.
    pub name: String,
    /// The transform Noesis currently holds on the element.
    pub spec: TransformSpec,
}

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

/// Registers the render-transform bridge. Added by [`crate::NoesisPlugin`].
pub struct NoesisTransformPlugin;

impl Plugin for NoesisTransformPlugin {
    fn build(&self, app: &mut App) {
        // After `sync_panels`, which sets `panel_mounted_this_frame`, so a
        // panel's transform applies the frame its fragment mounts.
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

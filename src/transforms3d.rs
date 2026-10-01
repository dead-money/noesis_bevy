//! Sets the `Transform3D` of named elements: rotation, scale, and translation
//! in 3D with perspective, for effects such as a card flip. For flat 2D motion,
//! use the `RenderTransform` bridge in [`crate::transforms`].
//!
//! Add a [`NoesisTransform3D`] to a [`NoesisView`](crate::NoesisView) camera.
//! Each entry in [`transforms`](NoesisTransform3D::transforms) becomes a
//! `CompositeTransform3D`, and each entry in
//! [`matrices`](NoesisTransform3D::matrices) a `MatrixTransform3D`, assigned
//! with
//! [`FrameworkElement::set_transform3d`](noesis_runtime::view::FrameworkElement::set_transform3d).
//! Like a render transform, it changes how the element is painted, not its
//! layout bounds.
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisTransform3D::new()
//!         .rotate_y("Card", 45.0)             // turn 45° around the Y axis
//!         .translate("Card", 0.0, 0.0, -20.0) // push 20 DIP into the screen
//!         .scale("Card", 1.2, 1.2, 1.0),      // 120% in-plane
//! );
//! ```
//!
//! Whenever the component changes or the view's scene is rebuilt, every entry
//! is assigned again. Removing an entry leaves the last transform on the
//! element.
//!
//! The bridge reads each element's live `Transform3D` back every frame and
//! emits [`NoesisTransform3DChanged`] or [`NoesisMatrixTransform3DChanged`] when
//! its values change. It reports only while the element still holds the
//! transform this bridge assigned, so silence means the write did not land or
//! something else replaced it.

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::transforms::Composite3DFields;

use crate::render::{NoesisRenderState, NoesisSet};

/// A `CompositeTransform3D`: scale, then rotate about
/// [`center`](Self::center), then translate. [`Default`] is the identity.
///
/// There is no perspective field. Noesis projects any element with a
/// `Transform3D` through an implicit camera, so depth and rotation about X or Y
/// foreshorten on their own.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform3DSpec {
    /// Pivot `[x, y, z]` for scale and rotation, in DIPs relative to the
    /// element's top-left corner.
    pub center: [f32; 3],
    /// Rotation about each axis `[x, y, z]` in degrees.
    pub rotation: [f32; 3],
    /// Scale factors `[x, y, z]` (`1.0` = unchanged).
    pub scale: [f32; 3],
    /// Translation `[x, y, z]` in DIPs; `z` moves along the view axis.
    pub translate: [f32; 3],
}

impl Default for Transform3DSpec {
    fn default() -> Self {
        Self {
            center: [0.0, 0.0, 0.0],
            rotation: [0.0, 0.0, 0.0],
            scale: [1.0, 1.0, 1.0],
            translate: [0.0, 0.0, 0.0],
        }
    }
}

impl Transform3DSpec {
    #[must_use]
    pub(crate) fn to_fields(self) -> Composite3DFields {
        Composite3DFields {
            center_x: self.center[0],
            center_y: self.center[1],
            center_z: self.center[2],
            rotation_x: self.rotation[0],
            rotation_y: self.rotation[1],
            rotation_z: self.rotation[2],
            scale_x: self.scale[0],
            scale_y: self.scale[1],
            scale_z: self.scale[2],
            translate_x: self.translate[0],
            translate_y: self.translate[1],
            translate_z: self.translate[2],
        }
    }

    #[must_use]
    pub(crate) fn from_fields(f: Composite3DFields) -> Self {
        Self {
            center: [f.center_x, f.center_y, f.center_z],
            rotation: [f.rotation_x, f.rotation_y, f.rotation_z],
            scale: [f.scale_x, f.scale_y, f.scale_z],
            translate: [f.translate_x, f.translate_y, f.translate_z],
        }
    }
}

/// A `MatrixTransform3D`: an arbitrary affine 3D transform, for when
/// [`Transform3DSpec`]'s decomposed form isn't enough.
///
/// Holds the 12 coefficients of a Noesis `Transform3`: four rows of three,
/// with row 3 the translation. Noesis uses row vectors (`v' = v * M`). Build
/// one with [`from_rows`](Self::from_rows) or [`from_mat4`](Self::from_mat4).
/// [`Default`] is the identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix3DSpec {
    /// The 12 coefficients, 4 rows by 3 columns, row-major.
    pub rows: [f32; 12],
}

impl Default for Matrix3DSpec {
    /// The identity transform.
    fn default() -> Self {
        Self {
            #[rustfmt::skip]
            rows: [
                1.0, 0.0, 0.0,
                0.0, 1.0, 0.0,
                0.0, 0.0, 1.0,
                0.0, 0.0, 0.0,
            ],
        }
    }
}

impl Matrix3DSpec {
    /// Build from the 12 coefficients, `[row0 xyz, row1 xyz, row2 xyz, row3 xyz]`.
    #[must_use]
    pub fn from_rows(rows: [f32; 12]) -> Self {
        Self { rows }
    }

    /// Build from a 4x4 affine matrix whose translation is `m[3]`, dropping
    /// the 4th element of each row (`[0, 0, 0, 1]` for an affine matrix).
    /// Transpose a matrix that keeps its translation in `m[_][3]` first.
    #[must_use]
    pub fn from_mat4(m: [[f32; 4]; 4]) -> Self {
        #[rustfmt::skip]
        let rows = [
            m[0][0], m[0][1], m[0][2],
            m[1][0], m[1][1], m[1][2],
            m[2][0], m[2][1], m[2][2],
            m[3][0], m[3][1], m[3][2],
        ];
        Self { rows }
    }
}

/// 3D transforms for named elements. Add to a
/// [`NoesisView`](crate::NoesisView) camera entity; see the
/// [module docs](self).
///
/// The per-field builders and setters merge into the element's existing spec,
/// so `rotate_y` then `translate` on one name give one transform with both.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisTransform3D {
    /// Composite transform per element `x:Name` (may be scope-qualified,
    /// `"Host/Leaf"`). A missing name or a non-`UIElement` is skipped with a
    /// warning.
    pub transforms: HashMap<String, Transform3DSpec>,
    /// Matrix transform per element `x:Name`. Both maps set the same
    /// `Transform3D` property, so use one per name. A name in both gets the
    /// matrix, and its composite read-back stays silent.
    pub matrices: HashMap<String, Matrix3DSpec>,
}

impl NoesisTransform3D {
    /// An empty bridge. Chain [`set`](Self::set),
    /// [`translate`](Self::translate), [`rotate_y`](Self::rotate_y),
    /// [`matrix`](Self::matrix), and the other builders to populate it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder: replace `name`'s entire spec.
    #[must_use]
    pub fn set(mut self, name: impl Into<String>, spec: Transform3DSpec) -> Self {
        self.transforms.insert(name.into(), spec);
        self
    }

    /// Builder: set `name`'s translation in DIPs, keeping its other fields.
    #[must_use]
    pub fn translate(mut self, name: impl Into<String>, x: f32, y: f32, z: f32) -> Self {
        self.entry(name).translate = [x, y, z];
        self
    }

    /// Builder: set `name`'s scale factors, keeping its other fields.
    #[must_use]
    pub fn scale(mut self, name: impl Into<String>, x: f32, y: f32, z: f32) -> Self {
        self.entry(name).scale = [x, y, z];
        self
    }

    /// Builder: set `name`'s pivot in DIPs, keeping its other fields.
    #[must_use]
    pub fn center(mut self, name: impl Into<String>, x: f32, y: f32, z: f32) -> Self {
        self.entry(name).center = [x, y, z];
        self
    }

    /// Builder: set all three rotation angles in degrees, keeping its other
    /// fields.
    #[must_use]
    pub fn rotate(mut self, name: impl Into<String>, x: f32, y: f32, z: f32) -> Self {
        self.entry(name).rotation = [x, y, z];
        self
    }

    /// Builder: set `name`'s rotation about the X axis in degrees, keeping
    /// everything else.
    #[must_use]
    pub fn rotate_x(mut self, name: impl Into<String>, degrees: f32) -> Self {
        self.entry(name).rotation[0] = degrees;
        self
    }

    /// Builder: set `name`'s rotation about the Y axis in degrees, keeping
    /// everything else.
    #[must_use]
    pub fn rotate_y(mut self, name: impl Into<String>, degrees: f32) -> Self {
        self.entry(name).rotation[1] = degrees;
        self
    }

    /// Builder: set `name`'s rotation about the Z axis in degrees, keeping
    /// everything else.
    #[must_use]
    pub fn rotate_z(mut self, name: impl Into<String>, degrees: f32) -> Self {
        self.entry(name).rotation[2] = degrees;
        self
    }

    fn entry(&mut self, name: impl Into<String>) -> &mut Transform3DSpec {
        self.transforms.entry(name.into()).or_default()
    }

    /// Builder: give `name` a matrix transform. Don't also give it a composite
    /// transform; see [`matrices`](Self::matrices).
    #[must_use]
    pub fn matrix(mut self, name: impl Into<String>, spec: Matrix3DSpec) -> Self {
        self.matrices.insert(name.into(), spec);
        self
    }

    /// Builder: [`matrix`](Self::matrix) with [`Matrix3DSpec::from_rows`].
    #[must_use]
    pub fn matrix_rows(self, name: impl Into<String>, rows: [f32; 12]) -> Self {
        self.matrix(name, Matrix3DSpec::from_rows(rows))
    }

    /// Replace `name`'s entire spec. The `&mut` form of [`set`](Self::set),
    /// for systems that update the component.
    pub fn write(&mut self, name: impl Into<String>, spec: Transform3DSpec) {
        self.transforms.insert(name.into(), spec);
    }

    /// The `&mut` form of [`translate`](Self::translate).
    pub fn set_translate(&mut self, name: impl Into<String>, x: f32, y: f32, z: f32) {
        self.entry(name).translate = [x, y, z];
    }

    /// The `&mut` form of [`scale`](Self::scale).
    pub fn set_scale(&mut self, name: impl Into<String>, x: f32, y: f32, z: f32) {
        self.entry(name).scale = [x, y, z];
    }

    /// The `&mut` form of [`center`](Self::center).
    pub fn set_center(&mut self, name: impl Into<String>, x: f32, y: f32, z: f32) {
        self.entry(name).center = [x, y, z];
    }

    /// The `&mut` form of [`rotate`](Self::rotate).
    pub fn set_rotation(&mut self, name: impl Into<String>, x: f32, y: f32, z: f32) {
        self.entry(name).rotation = [x, y, z];
    }

    /// The `&mut` form of [`rotate_x`](Self::rotate_x).
    pub fn set_rotation_x(&mut self, name: impl Into<String>, degrees: f32) {
        self.entry(name).rotation[0] = degrees;
    }

    /// The `&mut` form of [`rotate_y`](Self::rotate_y).
    pub fn set_rotation_y(&mut self, name: impl Into<String>, degrees: f32) {
        self.entry(name).rotation[1] = degrees;
    }

    /// The `&mut` form of [`rotate_z`](Self::rotate_z).
    pub fn set_rotation_z(&mut self, name: impl Into<String>, degrees: f32) {
        self.entry(name).rotation[2] = degrees;
    }

    /// The `&mut` form of [`matrix`](Self::matrix).
    pub fn write_matrix(&mut self, name: impl Into<String>, spec: Matrix3DSpec) {
        self.matrices.insert(name.into(), spec);
    }

    /// The `&mut` form of [`matrix_rows`](Self::matrix_rows).
    pub fn write_matrix_rows(&mut self, name: impl Into<String>, rows: [f32; 12]) {
        self.matrices
            .insert(name.into(), Matrix3DSpec::from_rows(rows));
    }
}

/// An element's live composite `Transform3D` values changed, or were read for
/// the first time after assignment. `spec` is read from Noesis, not copied from
/// the component.
#[derive(Message, Debug, Clone)]
pub struct NoesisTransform3DChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the element whose `Transform3D` changed.
    pub name: String,
    /// The transform Noesis currently holds on the element.
    pub spec: Transform3DSpec,
}

/// An element's live matrix `Transform3D` changed, or was read for the first
/// time after assignment. `matrix` is read from Noesis, not copied from the
/// component.
#[derive(Message, Debug, Clone)]
pub struct NoesisMatrixTransform3DChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the element whose `Transform3D` changed.
    pub name: String,
    /// The 12 coefficients Noesis holds, laid out as [`Matrix3DSpec::rows`].
    pub matrix: [f32; 12],
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sync_transform3d_bridge(
    views: Query<(Entity, Ref<NoesisTransform3D>)>,
    state: Option<NonSendMut<NoesisRenderState>>,
    mut changed: MessageWriter<NoesisTransform3DChanged>,
    mut matrix_changed: MessageWriter<NoesisMatrixTransform3DChanged>,
) {
    let Some(mut state) = state else {
        return;
    };
    for (entity, transform) in &views {
        if transform.is_changed() || state.scene_rebuilt_this_frame(entity) {
            state.apply_transforms3d_for(entity, &transform.transforms);
            state.apply_matrix_transforms3d_for(entity, &transform.matrices);
        }

        let names: Vec<&str> = transform.transforms.keys().map(String::as_str).collect();
        for (name, spec) in state.poll_transforms3d_for(entity, &names) {
            changed.write(NoesisTransform3DChanged {
                view: entity,
                name,
                spec,
            });
        }

        let matrix_names: Vec<&str> = transform.matrices.keys().map(String::as_str).collect();
        for (name, matrix) in state.poll_matrix_transforms3d_for(entity, &matrix_names) {
            matrix_changed.write(NoesisMatrixTransform3DChanged {
                view: entity,
                name,
                matrix,
            });
        }
    }
}

/// Registers the 3D-transform bridge. Added by [`crate::NoesisPlugin`].
pub struct NoesisTransform3DPlugin;

impl Plugin for NoesisTransform3DPlugin {
    fn build(&self, app: &mut App) {
        app.add_message::<NoesisTransform3DChanged>()
            .add_message::<NoesisMatrixTransform3DChanged>()
            .add_systems(PostUpdate, sync_transform3d_bridge.in_set(NoesisSet::Apply));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_merges_fields_per_name() {
        let t = NoesisTransform3D::new()
            .translate("A", 10.0, 20.0, 30.0)
            .scale("A", 2.0, 3.0, 4.0)
            .rotate_y("A", 45.0)
            .rotate_x("A", 15.0)
            .translate("B", 1.0, 2.0, 3.0);

        let a = t.transforms.get("A").copied().unwrap();
        assert_eq!(a.translate, [10.0, 20.0, 30.0]);
        assert_eq!(a.scale, [2.0, 3.0, 4.0]);
        assert_eq!(a.rotation, [15.0, 45.0, 0.0]);
        assert_eq!(a.center, [0.0, 0.0, 0.0]);

        let b = t.transforms.get("B").copied().unwrap();
        assert_eq!(b.translate, [1.0, 2.0, 3.0]);
        assert_eq!(b.scale, [1.0, 1.0, 1.0]);
        assert_eq!(b.rotation, [0.0, 0.0, 0.0]);
    }

    #[test]
    fn rotate_sets_all_three_axes() {
        let t = NoesisTransform3D::new().rotate("C", 10.0, 20.0, 30.0);
        let c = t.transforms.get("C").copied().unwrap();
        assert_eq!(c.rotation, [10.0, 20.0, 30.0]);
    }

    #[test]
    fn matrix_builder_queues_per_name() {
        #[rustfmt::skip]
        let rows = [
            2.0, 0.0, 0.0,
            0.0, 3.0, 0.0,
            0.0, 0.0, 4.0,
            5.0, 6.0, 7.0,
        ];
        let t = NoesisTransform3D::new()
            .matrix_rows("A", rows)
            .matrix("B", Matrix3DSpec::default());

        assert_eq!(t.matrices.get("A").unwrap().rows, rows);
        assert_eq!(t.matrices.get("B").copied(), Some(Matrix3DSpec::default()));
        // Composite and matrix maps are independent.
        assert!(t.transforms.is_empty());
    }

    #[test]
    fn matrix_from_mat4_drops_projective_column() {
        // Row-major affine: scale (2,3,4) on the diagonal, translation in row 3,
        // projective 4th column [0,0,0,1] must be dropped.
        let m = [
            [2.0, 0.0, 0.0, 0.0],
            [0.0, 3.0, 0.0, 0.0],
            [0.0, 0.0, 4.0, 0.0],
            [5.0, 6.0, 7.0, 1.0],
        ];
        #[rustfmt::skip]
        let expected = [
            2.0, 0.0, 0.0,
            0.0, 3.0, 0.0,
            0.0, 0.0, 4.0,
            5.0, 6.0, 7.0,
        ];
        assert_eq!(Matrix3DSpec::from_mat4(m).rows, expected);
    }

    #[test]
    fn fields_round_trip() {
        let spec = Transform3DSpec {
            center: [7.0, 8.0, 9.0],
            rotation: [30.0, -15.0, 5.0],
            scale: [2.0, 0.5, 1.5],
            translate: [5.0, 6.0, -7.0],
        };
        assert_eq!(Transform3DSpec::from_fields(spec.to_fields()), spec);
    }
}

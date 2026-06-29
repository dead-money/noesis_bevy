//! Per-view **3D transform** writes against named XAML elements: the
//! `UIElement.Transform3D` attached behaviour (WinUI/Noesis) that rotates,
//! scales and translates an element in 3D space about a center, with the
//! implicit projection camera supplying perspective. Distinct from the 2D
//! `RenderTransform` bridge ([`crate::transforms`]): that one sets
//! `RenderTransform`, this one sets `Transform3D`.
//!
//! Noesis's `CompositeTransform3D` bundles center / rotation / scale /
//! translation (each XYZ) into one object. We build one from Rust
//! ([`CompositeTransform3D`](noesis_runtime::transforms::CompositeTransform3D))
//! and assign it via
//! [`FrameworkElement::set_transform3d`](noesis_runtime::view::FrameworkElement::set_transform3d)
//! (`UIElement::SetTransform3D`). Like `RenderTransform`, `Transform3D` is a
//! render-time concern: it never disturbs the element's measured/arranged
//! bounds, so it can't reflow surrounding layout.
//!
//! Add a [`NoesisTransform3D`] component to the view's camera entity. Its
//! `transforms` map is the desired [`Transform3DSpec`] per `x:Name`, applied to
//! the view's elements whenever the component changes (Bevy change detection).
//!
//! ```ignore
//! commands.entity(view).insert(
//!     NoesisTransform3D::new()
//!         .rotate_y("Card", 45.0)            // flip 45° around the Y axis
//!         .translate("Card", 0.0, 0.0, -20.0) // push 20 DIP into the screen
//!         .scale("Card", 1.2, 1.2, 1.0),     // 120% in-plane
//! );
//! ```
//!
//! This is a **read-watch** bridge mirroring [`crate::transforms`]: besides
//! applying the writes it polls each element's *live* `Transform3D` back from
//! Noesis and emits a [`NoesisTransform3DChanged`] carrying the values Noesis
//! actually stored. The read-back is element-sourced (element → `Transform3D`
//! DP → `CompositeTransform3D` object) and gated on pointer identity with the
//! object we assigned, so it is bluff-resistant: an un-applied / mis-routed
//! write leaves the element with no `Transform3D` and emits nothing.
//!
//! **Rendering caveat.** Assigning a `Transform3D` (this bridge) is a pure
//! data-model operation. *Compositing* the resulting perspective image, however,
//! routes through the offscreen effects/projection render path, parts of which
//! (Downsample/Upsample and the effect shaders) are not yet implemented in our
//! wgpu render device. A scene that needs that path can panic at render time. The
//! bridge itself does not require it; only the final visual does.
//!
//! Everything runs on the main thread (Noesis is thread-affine and lives there):
//! the reconcile system reads each view's component and applies + polls against
//! that view's live scene; no cross-world queues.

use std::collections::HashMap;

use bevy::prelude::*;
use noesis_runtime::transforms::Composite3DFields;

use crate::render::{NoesisRenderState, NoesisSet};

// ─────────────────────────────────────────────────────────────────────────────
// Spec
// ─────────────────────────────────────────────────────────────────────────────

/// A 3D composite transform: scale → rotate → translate, applied about a shared
/// center `(CenterX, CenterY, CenterZ)`. Mirrors XAML's `CompositeTransform3D`;
/// the [`Default`] is the identity (unit scale, no rotation/translation, origin
/// center).
///
/// Perspective is *not* a field here: Noesis applies an implicit projection
/// camera to any element that carries a `Transform3D`, so depth (`translate.z`,
/// rotation about X/Y) reads as perspective foreshortening without an explicit
/// distance knob.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Transform3DSpec {
    /// Center of the transformation `[x, y, z]` in view DIPs.
    pub center: [f32; 3],
    /// Rotation about each axis `[x, y, z]` in degrees.
    pub rotation: [f32; 3],
    /// Scale factors `[x, y, z]` (`1.0` = unchanged).
    pub scale: [f32; 3],
    /// Translation `[x, y, z]` in view DIPs (`z` toward/away from the viewer).
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
    /// Lower this spec into the runtime's flat [`Composite3DFields`] for
    /// assignment.
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

    /// Rebuild a spec from the runtime's [`Composite3DFields`] read back off a
    /// live element; the inverse of [`Self::to_fields`].
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

/// A raw 3D matrix transform: the arbitrary-affine alternative to
/// [`Transform3DSpec`], mirroring XAML's `MatrixTransform3D`. Where
/// `Transform3DSpec` decomposes into center/rotation/scale/translate, this
/// carries the bare 12 coefficients of a Noesis `Transform3`: four rows of a
/// `Vector3`, `[row0(xyz), row1(xyz), row2(xyz), row3(xyz)]`, with row 3 the
/// translation (Noesis uses row-vector convention, `v' = v · M`).
///
/// Build one with [`Self::from_rows`] (the runtime form) or [`Self::from_mat4`]
/// (a row-major 4×4 affine whose projective 4th column is dropped). The
/// [`Default`] is the identity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Matrix3DSpec {
    /// The 12 `Transform3` coefficients (4 rows × 3 columns, row-major).
    pub rows: [f32; 12],
}

impl Default for Matrix3DSpec {
    /// The identity transform (no scale/rotation, zero translation).
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
    /// Build from the runtime's 12-float `Transform3` form directly
    /// (`[row0(xyz), row1(xyz), row2(xyz), row3(xyz)]`).
    #[must_use]
    pub fn from_rows(rows: [f32; 12]) -> Self {
        Self { rows }
    }

    /// Build from a **row-major** 4×4 affine matrix, dropping the projective 4th
    /// column. For an affine transform that column is `[0, 0, 0, 1]ᵀ`, so the
    /// `Transform3` is exactly `rows[r][0..3]` for each row `r`, with translation
    /// in row 3. This matches Noesis's row-vector convention; a column-vector matrix
    /// must be transposed by the caller first.
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

// ─────────────────────────────────────────────────────────────────────────────
// Component
// ─────────────────────────────────────────────────────────────────────────────

/// Per-view 3D-transform bridge. Attach to a [`NoesisView`](crate::NoesisView)
/// entity. The builder methods *merge* into the per-name spec, so `rotate_y`
/// then `translate` on the same element compose into one `CompositeTransform3D`.
#[derive(Component, Clone, Default, Debug)]
pub struct NoesisTransform3D {
    /// Desired [`Transform3DSpec`] (decomposed `CompositeTransform3D`) per element
    /// `x:Name`. Assigned as each element's `Transform3D` whenever this component
    /// changes.
    pub transforms: HashMap<String, Transform3DSpec>,
    /// Desired [`Matrix3DSpec`] (raw `MatrixTransform3D`) per element `x:Name`.
    /// Both maps drive the single `UIElement::Transform3D` DP, so a name should
    /// appear in *one* of them. If it appears in both, the later-applied kind
    /// wins on the element and the other's read-back stays silent.
    pub matrices: HashMap<String, Matrix3DSpec>,
}

impl NoesisTransform3D {
    /// An empty bridge with no queued transforms. Chain the builder methods
    /// ([`set`](Self::set), [`translate`](Self::translate),
    /// [`rotate_y`](Self::rotate_y), [`matrix`](Self::matrix), ...) to populate
    /// it, then insert it on the [`NoesisView`](crate::NoesisView) camera.
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

    /// Builder: set `name`'s translation `(x, y, z)`, keeping any other fields
    /// already queued for it.
    #[must_use]
    pub fn translate(mut self, name: impl Into<String>, x: f32, y: f32, z: f32) -> Self {
        self.entry(name).translate = [x, y, z];
        self
    }

    /// Builder: set `name`'s scale factors `(x, y, z)`, keeping other fields.
    #[must_use]
    pub fn scale(mut self, name: impl Into<String>, x: f32, y: f32, z: f32) -> Self {
        self.entry(name).scale = [x, y, z];
        self
    }

    /// Builder: set `name`'s pivot center `(x, y, z)`, keeping other fields.
    #[must_use]
    pub fn center(mut self, name: impl Into<String>, x: f32, y: f32, z: f32) -> Self {
        self.entry(name).center = [x, y, z];
        self
    }

    /// Builder: set all three rotation angles `(x, y, z)` in degrees, keeping
    /// other fields.
    #[must_use]
    pub fn rotate(mut self, name: impl Into<String>, x: f32, y: f32, z: f32) -> Self {
        self.entry(name).rotation = [x, y, z];
        self
    }

    /// Builder: set `name`'s rotation about the X axis (degrees), keeping the
    /// other two rotation angles and all other fields.
    #[must_use]
    pub fn rotate_x(mut self, name: impl Into<String>, degrees: f32) -> Self {
        self.entry(name).rotation[0] = degrees;
        self
    }

    /// Builder: set `name`'s rotation about the Y axis (degrees), keeping the
    /// other two rotation angles and all other fields.
    #[must_use]
    pub fn rotate_y(mut self, name: impl Into<String>, degrees: f32) -> Self {
        self.entry(name).rotation[1] = degrees;
        self
    }

    /// Builder: set `name`'s rotation about the Z axis (degrees), keeping the
    /// other two rotation angles and all other fields.
    #[must_use]
    pub fn rotate_z(mut self, name: impl Into<String>, degrees: f32) -> Self {
        self.entry(name).rotation[2] = degrees;
        self
    }

    fn entry(&mut self, name: impl Into<String>) -> &mut Transform3DSpec {
        self.transforms.entry(name.into()).or_default()
    }

    /// Builder: assign `name` a raw matrix [`Transform3D`](Matrix3DSpec),
    /// replacing any matrix previously queued for it. See the note on
    /// [`Self::matrices`]: a name should use either the composite builders or this
    /// matrix path, not both.
    #[must_use]
    pub fn matrix(mut self, name: impl Into<String>, spec: Matrix3DSpec) -> Self {
        self.matrices.insert(name.into(), spec);
        self
    }

    /// Builder: assign `name` a raw matrix from the runtime's 12-float
    /// `Transform3` form. Convenience for [`Self::matrix`] +
    /// [`Matrix3DSpec::from_rows`].
    #[must_use]
    pub fn matrix_rows(self, name: impl Into<String>, rows: [f32; 12]) -> Self {
        self.matrix(name, Matrix3DSpec::from_rows(rows))
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Read-back message
// ─────────────────────────────────────────────────────────────────────────────

/// Emitted when a transformed element's live `Transform3D` differs from the
/// previous frame's snapshot (and on the first poll after it is assigned). The
/// `spec` is read back from Noesis, so it reflects what the engine stored.
/// Read with `MessageReader<NoesisTransform3DChanged>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisTransform3DChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the element whose `Transform3D` changed.
    pub name: String,
    /// The transform Noesis currently holds on the element.
    pub spec: Transform3DSpec,
}

/// Emitted when a matrix-transformed element's live `Transform3D` differs from
/// the previous frame's snapshot (and on the first poll after it is assigned).
/// The `matrix` is read back from Noesis (the 12 `Transform3` coefficients), so
/// it reflects what the engine stored. Read with
/// `MessageReader<NoesisMatrixTransform3DChanged>`.
#[derive(Message, Debug, Clone)]
pub struct NoesisMatrixTransform3DChanged {
    /// The [`NoesisView`](crate::NoesisView) entity whose element changed.
    pub view: Entity,
    /// `x:Name` of the element whose `Transform3D` changed.
    pub name: String,
    /// The 12 `Transform3` coefficients Noesis currently holds on the element.
    pub matrix: [f32; 12],
}

// ─────────────────────────────────────────────────────────────────────────────
// Systems
// ─────────────────────────────────────────────────────────────────────────────

/// Reconcile every view's [`NoesisTransform3D`]: assign desired 3D transforms
/// when the component changed, then poll the assigned elements' live transforms
/// and emit [`NoesisTransform3DChanged`].
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
        if transform.is_changed() {
            state.apply_transforms3d_for(entity, &transform.transforms);
            let matrices: HashMap<String, [f32; 12]> = transform
                .matrices
                .iter()
                .map(|(name, spec)| (name.clone(), spec.rows))
                .collect();
            state.apply_matrix_transforms3d_for(entity, &matrices);
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

// ─────────────────────────────────────────────────────────────────────────────
// Plugin
// ─────────────────────────────────────────────────────────────────────────────

/// Wires the per-view 3D-transform bridge. Added transitively by
/// [`crate::NoesisPlugin`].
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

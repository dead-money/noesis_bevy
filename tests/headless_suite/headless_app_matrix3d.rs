//! Integration test for the raw 3D matrix transform path of [`NoesisTransform3D`]
//! ([`Matrix3DSpec`] / `MatrixTransform3D`) on the headless harness.
//!
//! The matrix lives on a nested object that a scalar `NoesisDp` watch can't reach.
//! The bridge reads the element's live `Transform3D` back after assignment and emits
//! [`NoesisMatrixTransform3DChanged`] only when it is the object it assigned.
//!
//! Positive: assigning a non-trivial affine matrix to `Box` reads those exact 12
//! floats back. A no-op apply, wrong-entity routing, or inverted change-detection
//! leaves `Box` with no `Transform3D`, so no message is emitted and the assertion
//! fails. Negative: `Other` is never given a transform and must never appear in
//! any [`NoesisMatrixTransform3DChanged`].
//!
//! The component starts empty and is filled once the scene is built, so change
//! detection fires on the real assignment.
//!
//! The rendered perspective pixels are not asserted; the empty `#[ignore]`d test
//! below is a placeholder for that check.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    Matrix3DSpec, NoesisCamera, NoesisMatrixTransform3DChanged, NoesisTransform3D, NoesisView,
    XamlRegistry,
};

use crate::common::{headless_app, run_until};

const SET_AT_FRAME: usize = 10;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Box" Width="20" Height="10"/>
  <Border x:Name="Other" Width="20" Height="10"/>
</Grid>"##;

// Anisotropic scale, off-diagonal shear, and translation.
#[rustfmt::skip]
const MATRIX: [f32; 12] = [
    2.0, 0.5, 0.0,
    0.0, 3.0, 0.0,
    0.0, 0.0, 4.0,
    5.0, 6.0, -7.0,
];

type Observed = Vec<(Entity, String, [f32; 12])>;

#[test]
fn matrix_transform3d_bridge_reads_back_assigned_matrix() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "matrix3d.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "matrix3d.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    // Filled at SET_AT_FRAME, after the scene exists.
                    NoesisTransform3D::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<&mut NoesisTransform3D>,
              mut changes: MessageReader<NoesisMatrixTransform3DChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut t in &mut q {
                    *t = NoesisTransform3D::new().matrix("Box", Matrix3DSpec::from_rows(MATRIX));
                }
            }

            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.matrix));
            }
        },
    );

    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        pred_observed
            .lock()
            .unwrap()
            .iter()
            .rfind(|(e, name, _)| *e == view && name == "Box")
            .map(|(_, _, m)| *m)
            == Some(MATRIX)
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisMatrixTransform3DChanged ---");
    for (e, name, matrix) in &got {
        eprintln!("  {e:?} {name} = {matrix:?}");
    }

    assert!(
        converged,
        "Box never reported its assigned matrix Transform3D within 240 frames; \
         observed {got:?}",
    );

    assert!(
        got.iter().all(|(_, name, _)| name != "Other"),
        "an un-transformed element must never emit NoesisMatrixTransform3DChanged",
    );

    let latest_box = got
        .iter()
        .rfind(|(e, name, _)| *e == view && name == "Box")
        .map(|(_, _, matrix)| *matrix)
        .expect("Box should report its assigned matrix Transform3D back");

    assert_eq!(
        latest_box, MATRIX,
        "the 12 Transform3 coefficients should round-trip exactly through the \
         element's live Transform3D",
    );
}

#[test]
#[ignore = "Transform3D perspective compositing needs the unimplemented Shadow/Blur effect shaders"]
fn matrix_transform3d_visual_render_is_gated_on_effect_shaders() {}

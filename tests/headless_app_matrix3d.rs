//! Bevy-app-level integration test for the **raw 3D matrix transform** path of
//! the [`NoesisTransform3D`] bridge ([`Matrix3DSpec`] / `MatrixTransform3D`),
//! exercised end-to-end through the real `NoesisPlugin` pipeline (headless,
//! pipelined rendering on).
//!
//! `MatrixTransform3D` (assigned via `UIElement::SetTransform3D`) is a
//! post-layout property: it never changes an element's `ActualWidth`/
//! `ActualHeight`, and its value lives on a nested `MatrixTransform3D` object —
//! neither reachable through a scalar `NoesisDp` watch. So this bridge ships its
//! own read-back: after assigning the matrix it reads the element's *live*
//! `Transform3D` back from Noesis (the 12 `Transform3` coefficients) and emits a
//! [`NoesisMatrixTransform3DChanged`]. The read-back is element-sourced and gated
//! on pointer identity with the object we assigned, so it is bluff-resistant:
//!
//!   * **positive** — assigning a non-trivial affine matrix (scale + shear +
//!     translate) to `Box` reads those exact 12 floats back. A no-op apply,
//!     wrong-entity routing, or inverted change-detection leaves `Box` with no
//!     `Transform3D`, so no message is emitted and the exact-value assertion
//!     fails.
//!   * **negative control** — `Other` is never given a transform, so it must
//!     never appear in any `NoesisMatrixTransform3DChanged`.
//!
//! The component starts empty (no-op) and is filled in *after* the scene is
//! built, because the apply runs on Bevy change-detection.
//!
//! **Scope.** This test asserts the *data-model* bridge (assignment +
//! element-sourced read-back), which is fully implemented. It does NOT assert the
//! perspective *pixels*: compositing a `Transform3D` routes through the offscreen
//! effects/projection render path whose Shadow/Blur effect shaders are not yet
//! implemented in our wgpu device. To keep the bridge test stable the transformed
//! element is empty (no background/content); the visual aspect is covered by the
//! `#[ignore]`d note below.
//!
//! Font-free XAML (only transform values are asserted, no glyph rendering).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    Matrix3DSpec, NoesisCamera, NoesisMatrixTransform3DChanged, NoesisPlugin, NoesisTransform3D,
    NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Box" Width="20" Height="10"/>
  <Border x:Name="Other" Width="20" Height="10"/>
</Grid>"##;

// A non-trivial affine Transform3: anisotropic scale on the diagonal, an
// off-diagonal shear, and a translation in row 3. None of these are the identity
// default, so a missing apply reads back nothing.
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
    noesis_license_from_env();

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::winit::WinitPlugin>()
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            }),
    );
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(4)));
    app.add_plugins(NoesisPlugin::default());

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
                    // Starts empty (no-op); filled after the scene exists so the
                    // one-shot apply isn't lost.
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
              mut changes: MessageReader<NoesisMatrixTransform3DChanged>,
              mut exit: MessageWriter<AppExit>| {
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

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisMatrixTransform3DChanged ---");
    for (e, name, matrix) in &got {
        eprintln!("  {e:?} {name} = {matrix:?}");
    }

    // Negative control: an un-targeted element must never be reported.
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

/// Visual compositing of a `Transform3D` (the perspective-projected pixels)
/// requires the offscreen effects/projection render path whose Shadow/Blur effect
/// shaders our wgpu render device does not implement yet (see CLAUDE.md /
/// TODO.md). The data-model bridge above is fully exercised; only the rendered
/// output is gated. Re-enable once the remaining effect shaders land.
#[test]
#[ignore = "Transform3D perspective compositing needs the unimplemented Shadow/Blur effect shaders"]
fn matrix_transform3d_visual_render_is_gated_on_effect_shaders() {}

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

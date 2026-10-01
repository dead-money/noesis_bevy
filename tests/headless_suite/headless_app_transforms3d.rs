//! Integration test for the [`NoesisTransform3D`] bridge, run through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! `Transform3D` (`UIElement::SetTransform3D`) is a post-layout property whose
//! value lives on a nested `CompositeTransform3D` object, not reachable through
//! a scalar `NoesisDp` watch. The bridge assigns the transform then reads the
//! element's live `Transform3D` back from Noesis, gated on pointer identity with
//! the assigned object.
//!
//! Test contract:
//! - `Box` receives rotate/translate/scale/center; the read-back must return the
//!   exact values assigned.
//! - `Other` is never given a transform and must never appear in any
//!   [`NoesisTransform3DChanged`].
//!
//! Only the data-model bridge is asserted here. Visual compositing (perspective
//! pixels) requires Downsample/Upsample effect shaders not yet implemented; the
//! ignored test below gates that path.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisTransform3D, NoesisTransform3DChanged, NoesisView, Transform3DSpec,
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

type Observed = Vec<(Entity, String, Transform3DSpec)>;

#[test]
fn transform3d_bridge_reads_back_assigned_transform() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "transforms3d.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "transforms3d.xaml".to_string(),
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
              mut changes: MessageReader<NoesisTransform3DChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut t in &mut q {
                    *t = NoesisTransform3D::new()
                        .rotate("Box", 10.0, 20.0, 30.0)
                        .translate("Box", 5.0, 6.0, -7.0)
                        .scale("Box", 2.0, 0.5, 1.5)
                        .center("Box", 1.0, 2.0, 3.0);
                }
            }

            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.spec));
            }
        },
    );

    // Stop once Box has reported its assigned transform back, rather than padding a
    // fixed frame count. The transform is assigned at SET_AT_FRAME.
    let pred_view = Arc::clone(&view_entity);
    let pred_observed = Arc::clone(&observed);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        pred_observed
            .lock()
            .unwrap()
            .iter()
            .rfind(|(e, name, _)| *e == view && name == "Box")
            .map(|(_, _, spec)| {
                spec.rotation == [10.0, 20.0, 30.0]
                    && spec.translate == [5.0, 6.0, -7.0]
                    && spec.scale == [2.0, 0.5, 1.5]
                    && spec.center == [1.0, 2.0, 3.0]
            })
            .unwrap_or(false)
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisTransform3DChanged ---");
    for (e, name, spec) in &got {
        eprintln!("  {e:?} {name} = {spec:?}");
    }

    assert!(
        converged,
        "Box never reported its assigned Transform3D within 240 frames; observed {got:?}",
    );

    // Negative control: an un-targeted element must never be reported.
    assert!(
        got.iter().all(|(_, name, _)| name != "Other"),
        "an un-transformed element must never emit NoesisTransform3DChanged",
    );

    let latest_box = got
        .iter()
        .rfind(|(e, name, _)| *e == view && name == "Box")
        .map(|(_, _, spec)| *spec)
        .expect("Box should report its assigned Transform3D back");

    assert_eq!(
        latest_box.rotation,
        [10.0, 20.0, 30.0],
        "rotation should round-trip through the element's live Transform3D",
    );
    assert_eq!(
        latest_box.translate,
        [5.0, 6.0, -7.0],
        "translate should round-trip",
    );
    assert_eq!(latest_box.scale, [2.0, 0.5, 1.5], "scale should round-trip");
    assert_eq!(
        latest_box.center,
        [1.0, 2.0, 3.0],
        "center should round-trip"
    );
}

/// Compositing a `Transform3D` (perspective-projected pixels) requires the
/// offscreen effects/projection render path: Downsample/Upsample and effect
/// shaders. The wgpu render device does not implement these yet
/// (`Shader(49)=DOWNSAMPLE` panics). Re-enable once the effect shaders land.
#[test]
#[ignore = "Transform3D perspective compositing needs the unimplemented Downsample/Upsample effect shaders"]
fn transform3d_visual_render_is_gated_on_effect_shaders() {}

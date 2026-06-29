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
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    NoesisCamera, NoesisPlugin, NoesisTransform3D, NoesisTransform3DChanged, NoesisView,
    Transform3DSpec, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Box" Width="20" Height="10"/>
  <Border x:Name="Other" Width="20" Height="10"/>
</Grid>"##;

type Observed = Vec<(Entity, String, Transform3DSpec)>;

#[test]
fn transform3d_bridge_reads_back_assigned_transform() {
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
              mut changes: MessageReader<NoesisTransform3DChanged>,
              mut exit: MessageWriter<AppExit>| {
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

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisTransform3DChanged ---");
    for (e, name, spec) in &got {
        eprintln!("  {e:?} {name} = {spec:?}");
    }

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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

//! Bevy-app-level integration test for the **render-transform** bridge
//! ([`NoesisTransform`]), exercised end-to-end through the real `NoesisPlugin`
//! pipeline (headless, pipelined rendering on).
//!
//! `RenderTransform` is a post-layout property: it never changes an element's
//! `ActualWidth`/`ActualHeight`, and its value lives on a nested
//! `CompositeTransform` object — neither reachable through a scalar `NoesisDp`
//! watch. So this bridge ships its own read-back: after assigning the transform
//! it reads the element's *live* `RenderTransform` back from Noesis and emits a
//! [`NoesisTransformChanged`]. The read-back is element-sourced and gated on
//! pointer identity with the object we assigned, so it is bluff-resistant:
//!
//!   * **positive** — assigning `translate (50,30) · scale (2,3) · rotate 45` to
//!     `Box` reads those exact values back. A no-op apply, wrong-entity routing,
//!     or inverted change-detection leaves `Box` with no `RenderTransform`, so
//!     no message is emitted and the exact-value assertion fails.
//!   * **negative control** — `Other` is never given a transform, so it must
//!     never appear in any `NoesisTransformChanged` (a "transform everything"
//!     regression would surface it).
//!
//! The component starts empty (no-op) and is filled in *after* the scene is
//! built, because the apply runs on Bevy change-detection — mutating it before
//! the view exists would drop the one-shot apply.
//!
//! Font-free XAML (only transform values are asserted, no glyph rendering), so
//! the scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    NoesisCamera, NoesisPlugin, NoesisTransform, NoesisTransformChanged, NoesisView, TransformSpec,
    XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Box" Width="20" Height="10" Background="#400000FF"/>
  <Border x:Name="Other" Width="20" Height="10" Background="#4000FF00"/>
</Grid>"##;

type Observed = Vec<(Entity, String, TransformSpec)>;

#[test]
fn render_transform_bridge_reads_back_assigned_transform() {
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
                "transforms.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "transforms.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    // Starts empty (no-op); filled after the scene exists so the
                    // one-shot apply isn't lost.
                    NoesisTransform::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<&mut NoesisTransform>,
              mut changes: MessageReader<NoesisTransformChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut t in &mut q {
                    *t = NoesisTransform::new()
                        .translate("Box", 50.0, 30.0)
                        .scale("Box", 2.0, 3.0)
                        .rotate("Box", 45.0);
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
    eprintln!("--- observed NoesisTransformChanged ---");
    for (e, name, spec) in &got {
        eprintln!("  {e:?} {name} = {spec:?}");
    }

    // Negative control: an un-targeted element must never be reported.
    assert!(
        got.iter().all(|(_, name, _)| name != "Other"),
        "an un-transformed element must never emit NoesisTransformChanged",
    );

    let latest_box = got
        .iter()
        .rfind(|(e, name, _)| *e == view && name == "Box")
        .map(|(_, _, spec)| *spec)
        .expect("Box should report its assigned RenderTransform back");

    assert_eq!(
        latest_box.translate,
        [50.0, 30.0],
        "translate should round-trip through the element's live RenderTransform",
    );
    assert_eq!(latest_box.scale, [2.0, 3.0], "scale should round-trip");
    assert_eq!(latest_box.rotation, 45.0, "rotation should round-trip");
}

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

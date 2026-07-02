//! Regression for P1.13: [`NoesisPointerOverUi`] must not stay stuck `true`
//! after the view under the pointer despawns.
//!
//! Drives a full-bleed, hit-test-visible Border, moves the Noesis pointer onto
//! it (so `over` latches `true`), then despawns the view. Before the fix,
//! `apply_input` bailed out on the now-empty scene map without clearing the
//! flag, so `over` stayed `true` forever — exactly the state that wrongly
//! suppresses 3D-world interaction. After the fix it drains back to `false`.
//!
//! Uses [`NoesisInputQueue::push`] directly (no window needed) so the render
//! side hit-tests and mirrors the flag through the real pipeline.
//!
//! One `#[test]` per file (thread-affine Noesis runtime, one app per process).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue, NoesisPointerOverUi};
use noesis_bevy::routed_events::MouseButton;
use noesis_bevy::{NoesisCamera, NoesisPlugin, NoesisView, XamlRegistry};

// A full-bleed Button: it consumes pointer input, so the `View` returns
// "over hit-test-visible UI" for a press on it (a plain Border doesn't consume
// the event and reports false). No text content, so no font folder is needed.
const XAML: &str = r##"<Button xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      HorizontalAlignment="Stretch" VerticalAlignment="Stretch"/>"##;

// Press/release the pointer every frame across this window so the events land
// after the scene has built (build takes a handful of frames), not before it
// exists. Each press latches the "over UI" flag.
const MOVE_FROM: usize = 15;
const DESPAWN_AT: usize = 35;
const CAPTURE_POST_AT: usize = 55;
const EXIT_AT: usize = 65;

#[test]
fn pointer_over_ui_resets_when_view_despawns() {
    noesis_license_from_env();

    // Latched true if `over` was ever observed true while the view was alive.
    let over_while_alive = Arc::new(Mutex::new(false));
    // `over` sampled well after the despawn.
    let over_after_despawn = Arc::new(Mutex::new(None::<bool>));

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

    app.add_systems(
        Startup,
        |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("over.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            commands.spawn((
                Camera2d,
                NoesisCamera,
                NoesisView {
                    xaml_uri: "over.xaml".to_string(),
                    size: UVec2::new(64, 32),
                    ..default()
                },
            ));
        },
    );

    let alive_sys = Arc::clone(&over_while_alive);
    let post_sys = Arc::clone(&over_after_despawn);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut input: ResMut<NoesisInputQueue>,
              over: Res<NoesisPointerOverUi>,
              views: Query<Entity, With<NoesisView>>,
              mut commands: Commands,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            if (MOVE_FROM..DESPAWN_AT).contains(&*frame) {
                input.push(NoesisInputEvent::MouseMove { x: 32, y: 16 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 32,
                    y: 16,
                    button: MouseButton::Left,
                });
                input.push(NoesisInputEvent::MouseButton {
                    down: false,
                    x: 32,
                    y: 16,
                    button: MouseButton::Left,
                });
                if over.over {
                    *alive_sys.lock().unwrap() = true;
                }
            }
            if *frame == DESPAWN_AT {
                for e in &views {
                    commands.entity(e).despawn();
                }
            }
            if *frame == CAPTURE_POST_AT {
                *post_sys.lock().unwrap() = Some(over.over);
            }
            if *frame >= EXIT_AT {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let over_while_alive = *over_while_alive.lock().unwrap();
    let over_after_despawn = over_after_despawn
        .lock()
        .unwrap()
        .expect("post-despawn sample captured");
    eprintln!(
        "--- pointer-over-ui reset: while_alive={over_while_alive} after_despawn={over_after_despawn} ---"
    );

    // Guards the test isn't vacuous: the pointer really did latch onto the UI.
    assert!(
        over_while_alive,
        "pointer-over-UI never became true over a full-bleed Button; test is vacuous",
    );
    // The actual regression: despawning the view must clear the flag.
    assert!(
        !over_after_despawn,
        "pointer-over-UI stayed true after the view despawned (P1.13 regression)",
    );
}

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

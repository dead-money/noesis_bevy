//! Regression for P1.13: [`NoesisPointerOverUi`] must not stay stuck `true`
//! after the view under the pointer despawns.
//!
//! Drives a full-bleed, hit-test-visible Button, moves the Noesis pointer onto
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

use bevy::prelude::*;
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue, NoesisPointerOverUi};
use noesis_bevy::routed_events::MouseButton;
use noesis_bevy::{NoesisCamera, NoesisView, XamlRegistry};

use crate::common::{headless_app, run_until};

// A full-bleed Button: it consumes pointer input, so the `View` returns
// "over hit-test-visible UI" for a press on it (a plain Border doesn't consume
// the event and reports false). No text content, so no font folder is needed.
const XAML: &str = r##"<Button xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      HorizontalAlignment="Stretch" VerticalAlignment="Stretch"/>"##;

// Press/release the pointer every frame across this window so the events land
// after the scene has built (build takes a handful of frames), not before it
// exists. Each press latches the "over UI" flag. These sequence the scenario;
// the run's exit is the terminal predicate (post-despawn sample captured), not a
// fixed frame count.
const MOVE_FROM: usize = 15;
const DESPAWN_AT: usize = 35;
const CAPTURE_POST_AT: usize = 55;

#[test]
fn pointer_over_ui_resets_when_view_despawns() {
    // Latched true if `over` was ever observed true while the view was alive.
    let over_while_alive = Arc::new(Mutex::new(false));
    // `over` sampled well after the despawn.
    let over_after_despawn = Arc::new(Mutex::new(None::<bool>));

    let mut app = headless_app();

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
              mut commands: Commands| {
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
        },
    );

    // Exit once the post-despawn sample has been taken; assert its value after.
    let pred_post = Arc::clone(&over_after_despawn);
    let captured = run_until(&mut app, 120, move |_app| {
        pred_post.lock().unwrap().is_some()
    });

    let over_while_alive = *over_while_alive.lock().unwrap();
    let over_after_despawn = over_after_despawn
        .lock()
        .unwrap()
        .expect("post-despawn sample captured");
    eprintln!(
        "--- pointer-over-ui reset: while_alive={over_while_alive} after_despawn={over_after_despawn} ---"
    );

    assert!(
        captured,
        "post-despawn sample was never taken within 120 frames"
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

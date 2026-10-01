//! Regression: an *unhandled* mouse wheel (over a `Button`, or a
//! `ScrollViewer` with nothing to scroll) must not lower
//! [`NoesisPointerOverUi`] — wheel results carry handled-ness, not pointer
//! position, so they may only raise the flag.
//!
//! One `#[test]` per file (thread-affine Noesis runtime, one app per process).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue, NoesisPointerOverUi};
use noesis_bevy::routed_events::MouseButton;
use noesis_bevy::{NoesisCamera, NoesisView, XamlRegistry};

use crate::common::{headless_app, run_until};

// A full-bleed Button: a press on it latches the over-UI flag; it has no
// scroll behavior, so a wheel over it is unhandled.
const XAML: &str = r##"<Button xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      HorizontalAlignment="Stretch" VerticalAlignment="Stretch"/>"##;

// Scene builds over the first frames; click latches the flag, then wheel and
// resample. Stimulus timings; the exit is the terminal predicate.
const MOVE_AT: usize = 15;
const SAMPLE_OVER_AT: usize = 20;
const WHEEL_AT: usize = 25;
const SAMPLE_POST_AT: usize = 35;

#[test]
fn unhandled_wheel_does_not_lower_pointer_over_ui() {
    let over_after_move = Arc::new(Mutex::new(None::<bool>));
    let over_after_wheel = Arc::new(Mutex::new(None::<bool>));

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

    let moved_sys = Arc::clone(&over_after_move);
    let wheeled_sys = Arc::clone(&over_after_wheel);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut input: ResMut<NoesisInputQueue>,
              over: Res<NoesisPointerOverUi>| {
            *frame += 1;
            if *frame == MOVE_AT {
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
            }
            if *frame == SAMPLE_OVER_AT {
                *moved_sys.lock().unwrap() = Some(over.over);
            }
            if *frame == WHEEL_AT {
                // Pointer has NOT moved; the wheel lands on the button, where
                // nothing scrolls, so Noesis reports it unhandled.
                input.push(NoesisInputEvent::MouseWheel {
                    x: 32,
                    y: 16,
                    delta: 120,
                });
            }
            if *frame == SAMPLE_POST_AT {
                *wheeled_sys.lock().unwrap() = Some(over.over);
            }
        },
    );

    let pred = Arc::clone(&over_after_wheel);
    let captured = run_until(&mut app, 120, move |_app| pred.lock().unwrap().is_some());

    let over_after_move = over_after_move
        .lock()
        .unwrap()
        .expect("post-move sample captured");
    let over_after_wheel = over_after_wheel
        .lock()
        .unwrap()
        .expect("post-wheel sample captured");
    eprintln!("--- over-UI wheel: after_move={over_after_move} after_wheel={over_after_wheel} ---");

    assert!(captured, "post-wheel sample never captured");
    assert!(
        over_after_move,
        "clicking the full-bleed Button should latch NoesisPointerOverUi",
    );
    assert!(
        over_after_wheel,
        "an unhandled wheel over the Button lowered NoesisPointerOverUi",
    );
}

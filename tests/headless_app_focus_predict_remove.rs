//! Integration test for two [`NoesisFocusControl`] features run headless through
//! the real `NoesisPlugin` pipeline.
//!
//! 1. `predict_focus_name`: verifies that [`FocusPredict`] reports the actual
//!    `x:Name` of the predicted element, not just a yes/no against `expect`.
//!    Three watches cover: the positive path (Right from "Left" names "Right",
//!    `matches_expected=true`); the None path (tab-order direction unsupported, no
//!    candidate); and the mismatch path (Right from "Right" wraps to "Left",
//!    `predicted_name` is the actual target, not the caller's expect).
//!
//! 2. `KeyBinding::remove_from`: verifies that dropping a [`KeyBindingSpec`]
//!    detaches only that binding. F1 fires before removal and is silent after;
//!    F2 fires in both phases. Fire frames are checked against the removal frame
//!    to distinguish before/after.
//!
//! Theme-free / font-free XAML (bare `TextBox`es), so the scene builds without
//! a font gate or theme dictionary.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    FocusNavigationDirection, Key, ModifierKeys, NoesisCamera, NoesisFocus,
    NoesisFocusBindingFired, NoesisFocusControl, NoesisFocusPredicted, NoesisInputEvent,
    NoesisInputQueue, NoesisPlugin, NoesisView, XamlRegistry,
};

const VIEW_W: u32 = 80;
const VIEW_H: u32 = 32;

/// Build the scene + focus the root + install both bindings & predict watches.
const SETUP_AT_FRAME: usize = 12;
/// First chord press, with both F1 and F2 still installed.
const PRESS1_AT_FRAME: usize = 20;
/// Drop the F1 binding spec (keep F2). Reconciled this frame.
const REMOVE_AT_FRAME: usize = 28;
/// Second chord press, after the F1 binding was detached.
const PRESS2_AT_FRAME: usize = 36;
const EXIT_AT_FRAME: usize = 60;

/// Two `TextBox`es for directional focus navigation. Chord bindings on the root
/// Grid fire via F-key bubble-up from the focused `TextBox`, which doesn't
/// consume function keys.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      x:Name="Root" Width="80" Height="32">
  <StackPanel Orientation="Horizontal">
    <TextBox x:Name="Left"  Width="40" Height="20"/>
    <TextBox x:Name="Right" Width="40" Height="20"/>
  </StackPanel>
</Grid>"##;

fn press(queue: &mut NoesisInputQueue, key: Key) {
    queue.push(NoesisInputEvent::KeyDown(key));
    queue.push(NoesisInputEvent::KeyUp(key));
}

#[test]
fn predict_names_target_and_remove_detaches_only_dropped_binding() {
    noesis_license_from_env();

    // (frame, key) per NoesisFocusBindingFired.
    let fires: Arc<Mutex<Vec<(usize, Key)>>> = Arc::new(Mutex::new(Vec::new()));
    // (from, direction, candidate, predicted_name, matches_expected) per NoesisFocusPredicted.
    type Pred = (String, FocusNavigationDirection, bool, Option<String>, bool);
    let preds: Arc<Mutex<Vec<Pred>>> = Arc::new(Mutex::new(Vec::new()));
    let view_id: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

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

    let view_startup = Arc::clone(&view_id);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "focus_pr.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "focus_pr.xaml".to_string(),
                        size: UVec2::new(VIEW_W, VIEW_H),
                        ..default()
                    },
                    // filled in at SETUP_AT_FRAME after the scene exists
                    NoesisFocus::new(),
                    NoesisFocusControl::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let fires_sys = Arc::clone(&fires);
    let preds_sys = Arc::clone(&preds);
    let view_run = Arc::clone(&view_id);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<(&mut NoesisFocus, &mut NoesisFocusControl)>,
              mut queue: ResMut<NoesisInputQueue>,
              mut fired: MessageReader<NoesisFocusBindingFired>,
              mut predicted: MessageReader<NoesisFocusPredicted>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            let view = view_run.lock().unwrap().expect("view spawned");

            if *frame == SETUP_AT_FRAME {
                for (mut focus, mut ctl) in &mut q {
                    *focus = NoesisFocus::new().focus("Left");
                    *ctl = NoesisFocusControl::new()
                        .key_binding("Root", Key::F1, ModifierKeys::NONE)
                        .key_binding("Root", Key::F2, ModifierKeys::NONE)
                        // Positive: Right-from-Left lands on Right.
                        .predict_to("Left", FocusNavigationDirection::Right, "Right")
                        // None path: tab-order directions unsupported; no candidate.
                        .predict("Left", FocusNavigationDirection::Next)
                        // Name-reports-actual: Right-from-Right wraps to Left; expect="Right" mismatches.
                        .predict_to("Right", FocusNavigationDirection::Right, "Right");
                }
            }

            if *frame == PRESS1_AT_FRAME {
                press(&mut queue, Key::F1);
                press(&mut queue, Key::F2);
            }

            if *frame == REMOVE_AT_FRAME {
                // Drop the F1 spec, keep F2; keep the predict watches.
                for (_focus, mut ctl) in &mut q {
                    *ctl = NoesisFocusControl::new()
                        .key_binding("Root", Key::F2, ModifierKeys::NONE)
                        .predict_to("Left", FocusNavigationDirection::Right, "Right")
                        .predict("Left", FocusNavigationDirection::Next)
                        .predict_to("Right", FocusNavigationDirection::Right, "Right");
                }
            }

            if *frame == PRESS2_AT_FRAME {
                press(&mut queue, Key::F1);
                press(&mut queue, Key::F2);
            }

            for ev in fired.read() {
                if ev.view == view {
                    fires_sys.lock().unwrap().push((*frame, ev.key));
                }
            }
            for ev in predicted.read() {
                if ev.view == view {
                    preds_sys.lock().unwrap().push((
                        ev.from.clone(),
                        ev.direction,
                        ev.candidate,
                        ev.predicted_name.clone(),
                        ev.matches_expected,
                    ));
                }
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let fires = fires.lock().unwrap().clone();
    let preds = preds.lock().unwrap().clone();
    eprintln!("--- fires (frame, key) ---\n{fires:?}");
    eprintln!("--- preds (from, candidate, name, matches) ---\n{preds:?}");

    // Last entry for a (from, dir) pair; returns (candidate, predicted_name, matches_expected).
    let latest_pred =
        |from: &str, dir: FocusNavigationDirection| -> Option<(bool, Option<String>, bool)> {
            preds
                .iter()
                .rev()
                .find(|(f, d, ..)| f == from && *d == dir)
                .map(|(_, _, c, n, m)| (*c, n.clone(), *m))
        };

    // Positive: lands on "Right", matches_expected=true.
    assert_eq!(
        latest_pred("Left", FocusNavigationDirection::Right),
        Some((true, Some("Right".to_string()), true)),
        "PredictFocus(Right) from the focused Left should name the actual target \
         \"Right\" (candidate=true, matches_expected=true)",
    );
    // None path: tab-order direction unsupported; no candidate, no name.
    assert_eq!(
        latest_pred("Left", FocusNavigationDirection::Next),
        Some((false, None, false)),
        "PredictFocus(Next) is unsupported and should report no candidate / no name",
    );
    // Name-reports-actual: Right-from-Right wraps to "Left"; predicted_name is
    // the actual target, not the expect string; matches_expected is false.
    assert_eq!(
        latest_pred("Right", FocusNavigationDirection::Right),
        Some((true, Some("Left".to_string()), false)),
        "PredictFocus(Right) from Right wraps to \"Left\"; predicted_name must be \
         the actual target and matches_expected false against expect \"Right\"",
    );

    let in_window = |k: Key, lo: usize, hi: usize| {
        fires
            .iter()
            .any(|(f, key)| *key == k && *f >= lo && *f < hi)
    };

    // Pre-removal: both bindings installed and fire.
    assert!(
        in_window(Key::F1, PRESS1_AT_FRAME, REMOVE_AT_FRAME),
        "F1 should fire before its spec is removed; fires={fires:?}",
    );
    assert!(
        in_window(Key::F2, PRESS1_AT_FRAME, REMOVE_AT_FRAME),
        "F2 should fire before removal; fires={fires:?}",
    );

    // Post-removal: F1 detached, F2 retained.
    assert!(
        !in_window(Key::F1, PRESS2_AT_FRAME, EXIT_AT_FRAME + 1),
        "after remove_from, the F1 chord must NOT fire; fires={fires:?}",
    );
    assert!(
        in_window(Key::F2, PRESS2_AT_FRAME, EXIT_AT_FRAME + 1),
        "the retained F2 chord must still fire after F1 was removed; fires={fires:?}",
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

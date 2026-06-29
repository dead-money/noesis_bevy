//! Bevy-app-level integration test for the two runtime-0.10-unblocked
//! [`NoesisFocusControl`] enhancements, exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on, one Noesis app per
//! process).
//!
//! 1. **`predict_focus_name`** — [`FocusPredict`] now reports the predicted
//!    element's *actual* `x:Name` (via `FrameworkElement::predict_focus_name`),
//!    not just a yes/no against a caller-supplied `expect`. The XAML lays out two
//!    side-by-side `TextBox`es ("Left", "Right") — real navigation stops. Three
//!    watches, each on a distinct `(from, direction)` so they're unambiguous,
//!    pin the behaviour:
//!      * `Right` from the focused "Left" names "Right", `matches_expected=true`;
//!      * `Next` (a tab-order direction `PredictFocus` does not support) reports
//!        no candidate and no name — the `None` path;
//!      * `Right` from "Right" wraps to "Left", so `predicted_name` is the actual
//!        "Left" while `matches_expected` is false against the expected "Right" —
//!        proving the name is the real target, not an echo of `expect`.
//!
//! 2. **`KeyBinding::remove_from`** — installed key bindings are now diff-synced:
//!    dropping a [`KeyBindingSpec`] from the component detaches its binding from
//!    the element. Two chord bindings (F1, F2) are installed on the root Grid and
//!    each fires once per press onto [`NoesisFocusBindingFired`] (F-keys bubble
//!    up past the focused `TextBox`, which doesn't consume them). After the F1
//!    spec is removed (F2 retained), an F1 press must produce NO fire while F2
//!    still fires — proving the removal was real *and* selective (a "detach
//!    everything" or "detach nothing" regression both fail).
//!
//! Bluff-resistance for the binding half rests on three observations in one run:
//! F1 fires *before* removal, F1 is silent *after* removal, and F2 fires in
//! *both* windows. The frame each fire was observed on is recorded so "before"
//! vs "after" is checked against the removal frame, not just presence.
//!
//! Theme-free / font-free XAML (bare `TextBox`es, no glyphs asserted, no control
//! templates), so the scene builds with no font gate and no theme dictionary.
//!
//!   `cargo test -p noesis_bevy --test headless_app_focus_predict_remove -- --nocapture`

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

/// Two side-by-side `TextBox`es (real navigation stops) so `PredictFocus(Right)`
/// from the focused "Left" lands on "Right". The chord bindings hang off the
/// root Grid; an F-key press on the focused `TextBox` (which doesn't consume
/// function keys) bubbles up to fire them.
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

    // (frame, key) per NoesisFocusBindingFired, on our view.
    let fires: Arc<Mutex<Vec<(usize, Key)>>> = Arc::new(Mutex::new(Vec::new()));
    // (from, direction, candidate, predicted_name, matches_expected) per
    // NoesisFocusPredicted.
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
                    // Start empty; the one-shot focus apply and the binding
                    // installs are filled in after the scene exists.
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
                        // Positive: Right from the focused Left lands on Right.
                        .predict_to("Left", FocusNavigationDirection::Right, "Right")
                        // Tab-order directions are unsupported by PredictFocus
                        // and report null: the None / no-candidate path.
                        .predict("Left", FocusNavigationDirection::Next)
                        // Name-reports-actual control: directional nav wraps, so
                        // Right-from-Right lands on Left; the watch expected
                        // "Right", so predicted_name must be the ACTUAL "Left"
                        // and matches_expected must be false.
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
                        // Positive: Right from the focused Left lands on Right.
                        .predict_to("Left", FocusNavigationDirection::Right, "Right")
                        // Tab-order directions are unsupported by PredictFocus
                        // and report null: the None / no-candidate path.
                        .predict("Left", FocusNavigationDirection::Next)
                        // Name-reports-actual control: directional nav wraps, so
                        // Right-from-Right lands on Left; the watch expected
                        // "Right", so predicted_name must be the ACTUAL "Left"
                        // and matches_expected must be false.
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

    // ── predict_focus_name ───────────────────────────────────────────────
    // The latest prediction reported for a (from, direction) pair, reduced to
    // (candidate, predicted_name, matches_expected).
    let latest_pred =
        |from: &str, dir: FocusNavigationDirection| -> Option<(bool, Option<String>, bool)> {
            preds
                .iter()
                .rev()
                .find(|(f, d, ..)| f == from && *d == dir)
                .map(|(_, _, c, n, m)| (*c, n.clone(), *m))
        };

    // Positive: the predicted element is named "Right", and it matches expect.
    assert_eq!(
        latest_pred("Left", FocusNavigationDirection::Right),
        Some((true, Some("Right".to_string()), true)),
        "PredictFocus(Right) from the focused Left should name the actual target \
         \"Right\" (candidate=true, matches_expected=true)",
    );
    // None path: PredictFocus does not support tab-order directions, so it
    // reports no candidate and no name.
    assert_eq!(
        latest_pred("Left", FocusNavigationDirection::Next),
        Some((false, None, false)),
        "PredictFocus(Next) is unsupported and should report no candidate / no name",
    );
    // Name-reports-actual: wrapping nav sends Right-from-Right to "Left". The
    // watch *expected* "Right", so this proves predicted_name carries the ACTUAL
    // predicted name (not the expect string) and matches_expected is a real
    // comparison that here is false.
    assert_eq!(
        latest_pred("Right", FocusNavigationDirection::Right),
        Some((true, Some("Left".to_string()), false)),
        "PredictFocus(Right) from Right wraps to \"Left\"; predicted_name must be \
         the actual target and matches_expected false against expect \"Right\"",
    );

    // ── KeyBinding::remove_from ───────────────────────────────────────────
    let in_window = |k: Key, lo: usize, hi: usize| {
        fires
            .iter()
            .any(|(f, key)| *key == k && *f >= lo && *f < hi)
    };

    // Pre-removal: both chords fire (proves the bindings installed at all).
    assert!(
        in_window(Key::F1, PRESS1_AT_FRAME, REMOVE_AT_FRAME),
        "F1 should fire before its spec is removed; fires={fires:?}",
    );
    assert!(
        in_window(Key::F2, PRESS1_AT_FRAME, REMOVE_AT_FRAME),
        "F2 should fire before removal; fires={fires:?}",
    );

    // Post-removal: F1 is detached (silent), F2 is retained (still fires).
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

//! Bevy-app-level integration test for the **focus-navigation** bridge
//! (`NoesisFocusControl`), exercised end-to-end through the real `NoesisPlugin`
//! pipeline (headless, pipelined rendering on). One Noesis app per test process.
//!
//! Two bluff-*resistant* effects are asserted, both against the element's
//! un-applied default:
//!
//!   1. **directional `MoveFocus`** — two side-by-side `TextBox`es (`First`,
//!      `Second`) in a horizontal `StackPanel`. We focus `First`, then issue
//!      `MoveFocus(First, Right)`. We observe focus through a `NoesisDp` watch on
//!      `IsFocused` (`bool`): after the move `Second.IsFocused` must be `true`
//!      (default `false`) and `First.IsFocused` must be `false` (it *was* `true`
//!      after the initial focus). A missing apply / wrong-entity routing /
//!      no-op move all read back the default and fail. `First` flipping back to
//!      `false` proves focus genuinely *moved* rather than being set on both.
//!
//!   2. **`PredictFocus`** — a `predict_to(First, Right, "Second")` watch must
//!      surface a `NoesisFocusPredicted` carrying `candidate = true` and
//!      `matches_expected = true`. The default is *no message at all*, so any
//!      such message is itself the positive signal; `matches_expected` (a raw
//!      pointer identity compare to `Second`'s element) rules out a stray
//!      candidate.
//!
//! Font-free XAML (only DP values / predictions are asserted, no glyph
//! rendering), so the scene builds with no font gate. The component is filled in
//! *after* the scene exists because its one-shot moves apply on change-detection.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpKind, DpValue, FocusNavigationDirection, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisFocus, NoesisFocusControl, NoesisFocusPredicted, NoesisPlugin, NoesisView, XamlRegistry,
};

const FOCUS_AT_FRAME: usize = 10;
const MOVE_AT_FRAME: usize = 25;
const EXIT_AT_FRAME: usize = 60;

// Two side-by-side, focusable TextBoxes so directional (Right) navigation has a
// real spatial neighbour to find.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="80" Height="32">
  <StackPanel Orientation="Horizontal">
    <TextBox x:Name="First" Width="40" Height="20"/>
    <TextBox x:Name="Second" Width="40" Height="20"/>
  </StackPanel>
</Grid>"##;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("First", "IsFocused", DpKind::Bool)
        .watch("Second", "IsFocused", DpKind::Bool)
}

type ObservedDp = Vec<(Entity, String, DpValue)>;
type ObservedPredict = Vec<(Entity, String, bool, bool)>;

#[test]
fn focus_control_moves_focus_and_predicts() {
    noesis_license_from_env();

    let dp_seen: Arc<Mutex<ObservedDp>> = Arc::new(Mutex::new(Vec::new()));
    let predict_seen: Arc<Mutex<ObservedPredict>> = Arc::new(Mutex::new(Vec::new()));
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
            reg.insert("focus.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "focus.xaml".to_string(),
                        size: UVec2::new(80, 32),
                        ..default()
                    },
                    // Write-only / poll components start empty so their one-shot
                    // apply isn't lost before the scene exists.
                    NoesisFocus::new(),
                    NoesisFocusControl::new(),
                    watcher(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let dp_sys = Arc::clone(&dp_seen);
    let predict_sys = Arc::clone(&predict_seen);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<(&mut NoesisFocus, &mut NoesisFocusControl)>,
              mut dp_changes: MessageReader<NoesisDpChanged>,
              mut predicts: MessageReader<NoesisFocusPredicted>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == FOCUS_AT_FRAME {
                for (mut focus, mut ctl) in &mut q {
                    // Focus First (existing bridge) and start the prediction watch.
                    *focus = NoesisFocus::new().focus("First");
                    *ctl = NoesisFocusControl::new().predict_to(
                        "First",
                        FocusNavigationDirection::Right,
                        "Second",
                    );
                }
            }
            if *frame == MOVE_AT_FRAME {
                for (_focus, mut ctl) in &mut q {
                    // Keep the prediction watch; add the directional move action.
                    *ctl = NoesisFocusControl::new()
                        .predict_to("First", FocusNavigationDirection::Right, "Second")
                        .move_focus("First", FocusNavigationDirection::Right, false);
                }
            }

            for ev in dp_changes.read() {
                dp_sys.lock().unwrap().push((
                    ev.view,
                    format!("{}.{}", ev.name, ev.property),
                    ev.value.clone(),
                ));
            }
            for ev in predicts.read() {
                predict_sys.lock().unwrap().push((
                    ev.view,
                    ev.from.clone(),
                    ev.candidate,
                    ev.matches_expected,
                ));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let dp = dp_seen.lock().unwrap().clone();
    let predicted = predict_seen.lock().unwrap().clone();

    eprintln!("--- observed NoesisDpChanged ---");
    for (e, np, v) in &dp {
        eprintln!("  {e:?} {np} = {v:?}");
    }
    eprintln!("--- observed NoesisFocusPredicted ---");
    for (e, from, c, m) in &predicted {
        eprintln!("  {e:?} from={from} candidate={c} matches={m}");
    }

    let latest = |np: &str| -> Option<DpValue> {
        dp.iter()
            .rfind(|(e, k, _)| *e == view && k == np)
            .map(|(_, _, v)| v.clone())
    };

    // 1. Directional MoveFocus: focus landed on Second, and left First.
    assert_eq!(
        latest("Second.IsFocused"),
        Some(DpValue::Bool(true)),
        "MoveFocus(First, Right) should focus Second (default IsFocused=false)",
    );
    assert_eq!(
        latest("First.IsFocused"),
        Some(DpValue::Bool(false)),
        "after MoveFocus, First must have lost focus (it was true after the initial focus)",
    );

    // 2. PredictFocus: a candidate exists in the Right direction and it IS Second.
    //    Default behaviour emits no NoesisFocusPredicted at all.
    assert!(
        predicted
            .iter()
            .any(|(e, from, candidate, matches)| *e == view
                && from == "First"
                && *candidate
                && *matches),
        "expected a NoesisFocusPredicted(First -> Right) with candidate && matches_expected; \
         got {predicted:?}",
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

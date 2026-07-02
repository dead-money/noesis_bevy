//! Integration test for `NoesisFocusControl` focus-navigation through the real `NoesisPlugin` pipeline (headless).
//!
//! Two assertions, each verified against the un-applied default:
//!
//!   - `MoveFocus(First, Right)` on a horizontal `StackPanel` of two `TextBox`es:
//!     `Second.IsFocused` must flip to `true` and `First.IsFocused` back to `false`.
//!   - `PredictFocus(First, Right, "Second")`: must surface `NoesisFocusPredicted`
//!     with `candidate = true` and `matches_expected = true` (default emits nothing).
//!
//! Font-free XAML; only DP values and predictions are asserted, so no font gate.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, FocusNavigationDirection, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisFocus, NoesisFocusControl, NoesisFocusPredicted, NoesisView, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const FOCUS_AT_FRAME: usize = 10;
const MOVE_AT_FRAME: usize = 25;

// Two focusable TextBoxes side-by-side so Right navigation has a real spatial target.
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
    let dp_seen: Arc<Mutex<ObservedDp>> = Arc::new(Mutex::new(Vec::new()));
    let predict_seen: Arc<Mutex<ObservedPredict>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

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
                    // start empty; one-shot applies fire after the scene exists
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
              mut predicts: MessageReader<NoesisFocusPredicted>| {
            *frame += 1;

            if *frame == FOCUS_AT_FRAME {
                for (mut focus, mut ctl) in &mut q {
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
        },
    );

    // Event-driven exit: stop once the move has focused Second (First lost focus)
    // and the matching prediction has surfaced. The move is frame-gated above.
    let pred_dp = Arc::clone(&dp_seen);
    let pred_predict = Arc::clone(&predict_seen);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let dp = pred_dp.lock().unwrap();
        let latest = |np: &str| -> Option<DpValue> {
            dp.iter()
                .rfind(|(e, k, _)| *e == view && k == np)
                .map(|(_, _, v)| v.clone())
        };
        let focus_moved = latest("Second.IsFocused") == Some(DpValue::Bool(true))
            && latest("First.IsFocused") == Some(DpValue::Bool(false));
        let predicted = pred_predict.lock().unwrap();
        let has_predict = predicted.iter().any(|(e, from, candidate, matches)| {
            *e == view && from == "First" && *candidate && *matches
        });
        focus_moved && has_predict
    });

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

    assert!(
        converged,
        "focus move + prediction never converged within 240 frames; dp={dp:?} predicted={predicted:?}",
    );

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

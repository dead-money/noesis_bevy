//! Regression for audit P0.5: `NoesisFocusControl` one-shot actions
//! (`moves` / `engages`) must be *drained* after they apply, so they neither
//! accumulate nor replay on a later change or a scene rebuild.
//!
//! Drives `request_move` (the push API) once against a live scene, then asserts
//! two frames later that `moves` is empty. Under the pre-fix code the vec was
//! push-only and never drained, so it stayed non-empty (and every subsequent
//! change replayed the whole accumulated history).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, FocusNavigationDirection, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisFocus, NoesisFocusControl, NoesisView, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const FOCUS_AT_FRAME: usize = 10;
const MOVE_AT_FRAME: usize = 25;
const CHECK_AT_FRAME: usize = 40;

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

#[test]
fn focus_control_one_shots_drain_after_apply() {
    let dp_seen: Arc<Mutex<ObservedDp>> = Arc::new(Mutex::new(Vec::new()));
    let moves_after_check: Arc<Mutex<Option<usize>>> = Arc::new(Mutex::new(None));
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
                    NoesisFocus::new(),
                    NoesisFocusControl::new(),
                    watcher(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let dp_sys = Arc::clone(&dp_seen);
    let moves_sys = Arc::clone(&moves_after_check);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<(&mut NoesisFocus, &mut NoesisFocusControl)>,
              mut dp_changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;

            if *frame == FOCUS_AT_FRAME {
                for (mut focus, _ctl) in &mut q {
                    *focus = NoesisFocus::new().focus("First");
                }
            }
            if *frame == MOVE_AT_FRAME {
                for (_focus, mut ctl) in &mut q {
                    // Push API: this is the path that accumulated pre-fix.
                    ctl.request_move("First", FocusNavigationDirection::Right, false);
                }
            }
            if *frame == CHECK_AT_FRAME {
                for (_focus, ctl) in &q {
                    *moves_sys.lock().unwrap() = Some(ctl.moves.len());
                }
            }

            for ev in dp_changes.read() {
                dp_sys.lock().unwrap().push((
                    ev.view,
                    format!("{}.{}", ev.name, ev.property),
                    ev.value.clone(),
                ));
            }
        },
    );

    // Event-driven exit: the move/check are frame-gated (the move must land, then
    // a later frame reads back the drained `moves` len), so exit once that post-
    // apply snapshot exists.
    let pred_moves = Arc::clone(&moves_after_check);
    let checked = run_until(&mut app, 240, move |_app| {
        pred_moves.lock().unwrap().is_some()
    });

    assert!(
        checked,
        "post-apply `moves` snapshot never captured within 240 frames",
    );

    let view = view_entity.lock().unwrap().expect("view spawned");
    let dp = dp_seen.lock().unwrap().clone();

    let latest = |np: &str| -> Option<DpValue> {
        dp.iter()
            .rfind(|(e, k, _)| *e == view && k == np)
            .map(|(_, _, v)| v.clone())
    };

    // The move actually took effect (sanity: the apply still runs).
    assert_eq!(
        latest("Second.IsFocused"),
        Some(DpValue::Bool(true)),
        "request_move(First, Right) should focus Second",
    );

    // The crux: the one-shot was drained, not left to accumulate/replay.
    assert_eq!(
        *moves_after_check.lock().unwrap(),
        Some(0),
        "moves must be drained after apply (was push-only pre-fix)",
    );
}

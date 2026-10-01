//! Integration test for the routed-event bridge on the headless harness.
//!
//! Injects `MouseDown` via [`NoesisInputQueue`] and asserts that the resulting [`NoesisRoutedEvent`]
//! carries the correct view entity, `RoutedEvent::MouseDown`, and a non-default arg snapshot
//! (button + position). A snapshot never read from the live args would be all `None` and
//! fail.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::routed_events::{
    EventWatchEntry, MouseButton, NoesisEventWatch, NoesisRoutedEvent, RoutedEvent,
};
use noesis_bevy::{NoesisCamera, NoesisView, XamlRegistry};

use crate::common::{headless_app, run_until};

const INJECT_AT_FRAME: usize = 14;

// 64x32 grid fully covered by a hit-testable Border (has a Background).
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Target" Background="#400000FF"/>
</Grid>"##;

type Collected = Vec<(
    Entity,
    String,
    RoutedEvent,
    Option<MouseButton>,
    Option<(f32, f32)>,
)>;

#[test]
fn routed_event_watch_surfaces_mouse_down_with_args() {
    let collected: Arc<Mutex<Collected>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "routed.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "routed.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    // Subscribes once the scene exists, so it can be attached at spawn.
                    NoesisEventWatch::new([EventWatchEntry::new("Target", RoutedEvent::MouseDown)]),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let collected_sys = Arc::clone(&collected);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut input: ResMut<NoesisInputQueue>,
              mut events: MessageReader<NoesisRoutedEvent>| {
            *frame += 1;

            // MouseMove first: Noesis hit-tests at the last known pointer position.
            if *frame == INJECT_AT_FRAME {
                input.push(NoesisInputEvent::MouseMove { x: 32, y: 16 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 32,
                    y: 16,
                    button: MouseButton::Left,
                });
            }

            for ev in events.read() {
                collected_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.event,
                    ev.args.mouse_button,
                    ev.args.position,
                ));
            }
        },
    );

    let pred_view = Arc::clone(&view_entity);
    let pred_collected = Arc::clone(&collected);
    let observed_hit = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        pred_collected
            .lock()
            .unwrap()
            .iter()
            .any(|(e, name, event, button, pos)| {
                *e == view
                    && name == "Target"
                    && *event == RoutedEvent::MouseDown
                    && button.is_some()
                    && pos.is_some()
            })
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = collected.lock().unwrap().clone();
    eprintln!("--- observed NoesisRoutedEvent ---");
    for (e, name, event, button, pos) in &got {
        eprintln!("  {e:?} {name} {event:?} button={button:?} pos={pos:?}");
    }

    assert!(
        observed_hit,
        "no MouseDown routed event surfaced on Target within 240 frames; observed {got:?}",
    );

    let hit = got
        .iter()
        .find(|(e, name, event, _, _)| {
            *e == view && name == "Target" && *event == RoutedEvent::MouseDown
        })
        .expect("expected a MouseDown routed event on Target tagged with our view");

    assert_eq!(
        hit.3,
        Some(MouseButton::Left),
        "MouseDown args should report the pressed button",
    );
    match hit.4 {
        Some((x, y)) => {
            assert!(
                (28.0..=36.0).contains(&x) && (12.0..=20.0).contains(&y),
                "MouseDown position should be near the injected (32,16); got ({x},{y})",
            );
        }
        None => panic!("MouseDown args should carry a position"),
    }
}

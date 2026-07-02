//! Integration test for the routed-event bridge, end-to-end through the real `NoesisPlugin` (headless).
//!
//! Injects `MouseDown` via [`NoesisInputQueue`] and asserts that the resulting [`NoesisRoutedEvent`]
//! carries the correct view entity, `RoutedEvent::MouseDown`, and a non-default arg snapshot
//! (button + position). An all-`None` snapshot (never read from live args) would fail the arg
//! assertions, catching a broken subscribe/reconcile or snapshot path.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::routed_events::{
    EventWatchEntry, MouseButton, NoesisEventWatch, NoesisRoutedEvent, RoutedEvent,
};
use noesis_bevy::{NoesisCamera, NoesisPlugin, NoesisView, XamlRegistry};

const INJECT_AT_FRAME: usize = 14;
const EXIT_AT_FRAME: usize = 50;

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
    noesis_license_from_env();

    let collected: Arc<Mutex<Collected>> = Arc::new(Mutex::new(Vec::new()));
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
                    // Reconciles every frame once the scene exists; safe to attach at spawn.
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
              mut events: MessageReader<NoesisRoutedEvent>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            // Pushed in Update so PostUpdate's apply pass drains it onto the View this same frame.
            // MouseMove first: Noesis hit-tests on the last known pointer position.
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

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = collected.lock().unwrap().clone();
    eprintln!("--- observed NoesisRoutedEvent ---");
    for (e, name, event, button, pos) in &got {
        eprintln!("  {e:?} {name} {event:?} button={button:?} pos={pos:?}");
    }

    let hit = got
        .iter()
        .find(|(e, name, event, _, _)| {
            *e == view && name == "Target" && *event == RoutedEvent::MouseDown
        })
        .expect("expected a MouseDown routed event on Target tagged with our view");

    // all-`None` default (snapshot never read from live args) would fail both.
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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

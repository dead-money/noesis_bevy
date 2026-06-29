//! Bevy-app-level integration test for the **generic routed-event** bridge,
//! exercised end-to-end through the real `NoesisPlugin` pipeline (headless,
//! pipelined rendering on).
//!
//! Bluff-resistance: a routed-event watch reports *nothing* until a real event
//! fires, so the un-applied default is an empty message stream AND an all-`None`
//! arg snapshot. We provoke a genuine `UIElement.MouseDown` by injecting a
//! left-button press over a hit-testable `Border` directly into the
//! [`NoesisInputQueue`] (the same path the Bevy input forwarders feed), then
//! assert a [`NoesisRoutedEvent`] comes back that:
//!
//!   * carries OUR view entity (per-entity routing), and
//!   * carries `event == RoutedEvent::MouseDown`, and
//!   * carries a non-default arg snapshot: `mouse_button == Some(Left)` and a
//!     `position` near the injected press point.
//!
//! A broken subscribe/reconcile, wrong-entity tag, or a snapshot that wasn't
//! actually read off the live args (the all-`None` default) each fail it.
//!
//! Font-free XAML (no glyph rendering asserted), so the scene builds with no
//! font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use dm_noesis_bevy::routed_events::{
    EventWatchEntry, MouseButton, NoesisEventWatch, NoesisRoutedEvent, RoutedEvent,
};
use dm_noesis_bevy::{NoesisCamera, NoesisPlugin, NoesisView, XamlRegistry};

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
                    // Watch is not change-detection gated: it reconciles every
                    // frame and binds once the scene + content exist, so it is
                    // safe to attach at spawn.
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

            // Inject a press over the Border's centre once the scene + the
            // routed-event subscription are live. Pushed in Update so it
            // survives PreUpdate's queue reset and is extracted to the render
            // world this same frame. MouseMove first: Noesis hit-tests on the
            // last known pointer position.
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

    // Non-default arg snapshot: the all-`None` default (a snapshot that was never
    // read off the live args) would fail both of these.
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

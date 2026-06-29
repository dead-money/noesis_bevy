//! Integration test for the `ICommand` bridge (`noesis_bevy::commands`).
//!
//! A [`NoesisCommands`] host declares `Fire` (no parameter) and `FireParam`
//! (`CommandParameter="payload-42"`), attached as `DataContext` on the view
//! root. Two opaque `Border`s carry `MouseBinding` gestures; Borders are used
//! instead of Buttons so the scene is hit-testable without a control theme.
//! Synthetic left-clicks invoke each command; the test asserts the correct
//! [`NoesisCommandInvoked`] arrives with the right view entity, command name,
//! and decoded parameter, and that no message arrives before the first click.
//!
//! Theme-free / font-free XAML so the scene builds with no font gate.
//!
//!   `cargo test -p noesis_bevy --test headless_app_commands -- --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::commands::{CommandsDef, NoesisCommandInvoked, NoesisCommands};
use noesis_bevy::{
    NoesisCamera, NoesisInputEvent, NoesisInputQueue, NoesisPlugin, NoesisView, XamlRegistry,
};
use noesis_runtime::view::MouseButton;

const VIEW_W: u32 = 64;
const VIEW_H: u32 = 32;

// Wait for scene build + DataContext attach + binding resolution before clicking.
const CLICK_FIRE_AT_FRAME: usize = 14;
const CLICK_PARAM_AT_FRAME: usize = 20;
const EXIT_AT_FRAME: usize = 60;

const PAYLOAD: &str = "payload-42";

// Borders not Buttons: hit-testable without a ControlTemplate.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Grid.ColumnDefinitions>
    <ColumnDefinition Width="*"/>
    <ColumnDefinition Width="*"/>
  </Grid.ColumnDefinitions>
  <Border Grid.Column="0" Background="#FF202020">
    <Border.InputBindings>
      <MouseBinding MouseAction="LeftClick" Command="{Binding Fire}"/>
    </Border.InputBindings>
  </Border>
  <Border Grid.Column="1" Background="#FF404040">
    <Border.InputBindings>
      <MouseBinding MouseAction="LeftClick" Command="{Binding FireParam}"
                    CommandParameter="payload-42"/>
    </Border.InputBindings>
  </Border>
</Grid>"##;

// (frame, entity, name, decoded-param); frame is checked to assert nothing fires before the click.
type Invocation = (usize, Entity, String, Option<String>);

// View-pixel coords, no window mapping.
fn left_click(queue: &mut NoesisInputQueue, x: i32, y: i32) {
    queue.push(NoesisInputEvent::MouseMove { x, y });
    queue.push(NoesisInputEvent::MouseButton {
        down: true,
        x,
        y,
        button: MouseButton::Left,
    });
    queue.push(NoesisInputEvent::MouseButton {
        down: false,
        x,
        y,
        button: MouseButton::Left,
    });
}

#[test]
fn ui_command_invocation_surfaces_message_with_decoded_parameter() {
    noesis_license_from_env();

    let collected: Arc<Mutex<Vec<Invocation>>> = Arc::new(Mutex::new(Vec::new()));
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
            reg.insert("cmd.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "cmd.xaml".to_string(),
                        size: UVec2::new(VIEW_W, VIEW_H),
                        ..default()
                    },
                    NoesisCommands::new(
                        CommandsDef::new("Test.Commands")
                            .command("Fire")
                            .command("FireParam"),
                    ),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let collected_sys = Arc::clone(&collected);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut queue: ResMut<NoesisInputQueue>,
              mut invoked: MessageReader<NoesisCommandInvoked>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == CLICK_FIRE_AT_FRAME {
                left_click(&mut queue, VIEW_W as i32 / 4, VIEW_H as i32 / 2);
            }
            if *frame == CLICK_PARAM_AT_FRAME {
                left_click(&mut queue, VIEW_W as i32 * 3 / 4, VIEW_H as i32 / 2);
            }

            for ev in invoked.read() {
                collected_sys.lock().unwrap().push((
                    *frame,
                    ev.view,
                    ev.name.clone(),
                    ev.parameter.clone(),
                ));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_id.lock().unwrap().expect("view was never spawned");
    let got = collected.lock().unwrap().clone();
    eprintln!("--- observed NoesisCommandInvoked ---");
    for (f, e, name, param) in &got {
        eprintln!("  frame {f}: {e:?} {name} param={param:?}");
    }

    assert!(
        got.iter()
            .any(|(_, e, name, param)| *e == view && name == "Fire" && param.is_none()),
        "expected NoesisCommandInvoked {{ view: {view:?}, name: \"Fire\", parameter: None }}; \
         got {got:?}",
    );

    // Some(PAYLOAD) proves the decode path; None would mean the parameter never flowed.
    assert!(
        got.iter().any(|(_, e, name, param)| *e == view
            && name == "FireParam"
            && param.as_deref() == Some(PAYLOAD)),
        "expected NoesisCommandInvoked {{ view: {view:?}, name: \"FireParam\", \
         parameter: Some({PAYLOAD:?}) }}; got {got:?}",
    );

    // Every invocation must have exactly the expected param: no cross-wiring, no missed decodes.
    for (_, _, name, param) in &got {
        match name.as_str() {
            "Fire" => assert!(
                param.is_none(),
                "Fire carries no CommandParameter, so its parameter must be None; got {param:?}",
            ),
            "FireParam" => assert_eq!(
                param.as_deref(),
                Some(PAYLOAD),
                "FireParam's CommandParameter must decode to {PAYLOAD:?}",
            ),
            other => panic!("unexpected command name surfaced: {other:?}"),
        }
    }

    assert!(
        got.iter().all(|(f, _, _, _)| *f >= CLICK_FIRE_AT_FRAME),
        "a command was invoked before the synthetic click; got {got:?}",
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

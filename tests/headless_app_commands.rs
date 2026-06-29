//! Bevy-app-level integration test for the per-view **`ICommand`** bridge
//! (`dm_noesis_bevy::commands`), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! Bluff-resistance: a [`NoesisCommands`] host declares one command, `Fire`,
//! attached as the view-root `DataContext`. The XAML's root `Grid` has an
//! opaque `Background` (so it is hit-testable WITHOUT any control theme/template)
//! and a `<MouseBinding MouseAction="LeftClick" Command="{Binding Fire}"/>`. The
//! test injects a synthetic left-click at the view centre via the public
//! [`NoesisInputQueue`]. The *only* way a [`NoesisCommandInvoked`] carrying
//! `name == "Fire"` and the right `view` entity can appear is if: the host class
//! was registered, the `Command` was assigned to its `BaseComponent` DP, the
//! instance was attached as `DataContext`, the `{Binding Fire}` resolved, the
//! click hit the `Grid`, the gesture matched, and the per-entity message path
//! tagged the correct entity.
//!
//! The "differs from the un-applied default" contract: with the bridge inert,
//! `Command="{Binding Fire}"` resolves to nothing and the click produces NO
//! message at all — so observing the message (with exact name + view) is the
//! positive signal. The test also asserts NO message arrives BEFORE the click.
//!
//! Theme-free / font-free XAML (a coloured `Grid`, no glyphs, no `Button`
//! template), so the scene builds with no font gate and no theme dictionary.
//!
//!   `cargo test -p dm_noesis_bevy --test headless_app_commands -- --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::commands::{CommandsDef, NoesisCommandInvoked, NoesisCommands};
use dm_noesis_bevy::{
    NoesisCamera, NoesisInputEvent, NoesisInputQueue, NoesisPlugin, NoesisView, XamlRegistry,
};
use noesis_runtime::view::MouseButton;

const VIEW_W: u32 = 64;
const VIEW_H: u32 = 32;

/// Wait long enough for the scene to build + the `DataContext` to attach + the
/// `{Binding}` to resolve before clicking.
const CLICK_AT_FRAME: usize = 14;
const EXIT_AT_FRAME: usize = 60;

/// A coloured root `Grid` is hit-testable with no theme; the `MouseBinding`
/// invokes the bound command on a left-click, so we never need a `Button`
/// `ControlTemplate`.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Background="#FF202020" Width="64" Height="32">
  <Grid.InputBindings>
    <MouseBinding MouseAction="LeftClick" Command="{Binding Fire}"/>
  </Grid.InputBindings>
  <TextBlock Text="hit me"/>
</Grid>"##;

#[test]
fn ui_command_invocation_surfaces_message_for_the_right_view() {
    noesis_license_from_env();

    // (view, name) pairs seen via NoesisCommandInvoked, with the frame observed,
    // so we can assert nothing arrived before the click.
    let collected: Arc<Mutex<Vec<(usize, Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
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
                    NoesisCommands::new(CommandsDef::new("Test.Commands").command("Fire")),
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

            if *frame == CLICK_AT_FRAME {
                // Synthetic left-click at the view centre, in view-pixel space
                // (these coords go straight onto the View — no window mapping).
                let (x, y) = (VIEW_W as i32 / 2, VIEW_H as i32 / 2);
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

            for ev in invoked.read() {
                collected_sys
                    .lock()
                    .unwrap()
                    .push((*frame, ev.view, ev.name.clone()));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_id.lock().unwrap().expect("view was never spawned");
    let got = collected.lock().unwrap().clone();

    // Positive signal: the click invoked `Fire` on the right view entity.
    assert!(
        got.iter()
            .any(|(_, e, name)| *e == view && name == "Fire"),
        "expected a NoesisCommandInvoked {{ view: {view:?}, name: \"Fire\" }}; got {got:?}",
    );

    // Bluff-catch: nothing should have been invoked before the click frame.
    assert!(
        got.iter().all(|(f, _, _)| *f >= CLICK_AT_FRAME),
        "a command was invoked before the synthetic click; got {got:?}",
    );

    // Bluff-catch: no phantom command names.
    assert!(
        got.iter().all(|(_, _, name)| name == "Fire"),
        "unexpected command name surfaced; got {got:?}",
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

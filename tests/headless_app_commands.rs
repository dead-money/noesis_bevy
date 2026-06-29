//! Bevy-app-level integration test for the per-view **`ICommand`** bridge
//! (`dm_noesis_bevy::commands`), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! Bluff-resistance: a [`NoesisCommands`] host declares two commands, `Fire`
//! and `FireParam`, attached as the view-root `DataContext`. The XAML's root
//! `Grid` has two side-by-side opaque `Border`s (so each is hit-testable WITHOUT
//! any control theme/template); the left one carries a
//! `<MouseBinding MouseAction="LeftClick" Command="{Binding Fire}"/>` (no
//! parameter) and the right one a
//! `<MouseBinding … Command="{Binding FireParam}" CommandParameter="payload-42"/>`.
//! The test injects synthetic left-clicks at each `Border`'s centre via the
//! public [`NoesisInputQueue`]. The *only* way a [`NoesisCommandInvoked`]
//! carrying the right `name`, the right `view` entity, and the right decoded
//! `parameter` can appear is if: the host class was registered, the `Command`s
//! were assigned to their `BaseComponent` DPs, the instance was attached as
//! `DataContext`, the `{Binding …}`s resolved, the clicks hit the `Border`s, the
//! gestures matched, the `CommandParameter` flowed through Noesis to the
//! command's `Execute`, the parameter was decoded, and the per-entity message
//! path tagged the correct entity.
//!
//! The "differs from the un-applied default" contract has two layers:
//!   * With the bridge inert, `Command="{Binding …}"` resolves to nothing and a
//!     click produces NO message at all — so observing the message (with exact
//!     name + view) is the positive signal. The test also asserts NO message
//!     arrives BEFORE the first click.
//!   * For the parameter decode specifically: an un-decoded parameter would read
//!     back as `None` (the negative control), so asserting the right `Border`'s
//!     invocation carries `Some("payload-42")` proves the decode path — while the
//!     left `Border` (no `CommandParameter`) must carry `None`.
//!
//! Theme-free / font-free XAML (coloured `Border`s, no glyphs, no `Button`
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
/// `{Binding}`s to resolve before clicking. `Fire` (no param) fires first, then
/// `FireParam` (with `CommandParameter`) a few frames later.
const CLICK_FIRE_AT_FRAME: usize = 14;
const CLICK_PARAM_AT_FRAME: usize = 20;
const EXIT_AT_FRAME: usize = 60;

/// The decoded value we expect for the right `Border`'s `CommandParameter`.
const PAYLOAD: &str = "payload-42";

/// Two coloured `Border`s split the 64-wide view into a left half (the `Fire`
/// command, no parameter) and a right half (the `FireParam` command, carrying
/// `CommandParameter="payload-42"`). Each is hit-testable with no theme, so a
/// left-click in its half invokes the bound command — no `Button`
/// `ControlTemplate` needed.
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

/// One observed invocation: the frame it surfaced on (to prove nothing fired
/// before the click), the view entity, the command name, and the decoded
/// parameter.
type Invocation = (usize, Entity, String, Option<String>);

/// Inject a synthetic left-click (move → down → up) at `(x, y)` in view-pixel
/// space (these coords go straight onto the `View` — no window mapping).
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

            // Left half centre → `Fire` (no parameter).
            if *frame == CLICK_FIRE_AT_FRAME {
                left_click(&mut queue, VIEW_W as i32 / 4, VIEW_H as i32 / 2);
            }
            // Right half centre → `FireParam` (CommandParameter="payload-42").
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

    // Positive signal #1: the no-parameter command invoked `Fire` on the right
    // view with a `None` parameter (the un-decoded / no-param negative control).
    assert!(
        got.iter()
            .any(|(_, e, name, param)| *e == view && name == "Fire" && param.is_none()),
        "expected NoesisCommandInvoked {{ view: {view:?}, name: \"Fire\", parameter: None }}; \
         got {got:?}",
    );

    // Positive signal #2: the parameterised command invoked `FireParam` on the
    // right view with the DECODED parameter. `None` here would mean the parameter
    // never flowed through / never decoded — the negative control.
    assert!(
        got.iter().any(|(_, e, name, param)| *e == view
            && name == "FireParam"
            && param.as_deref() == Some(PAYLOAD)),
        "expected NoesisCommandInvoked {{ view: {view:?}, name: \"FireParam\", \
         parameter: Some({PAYLOAD:?}) }}; got {got:?}",
    );

    // Bluff-catch: the `FireParam` invocation must NOT carry `None` (would be the
    // un-decoded parameter) and the `Fire` invocation must NOT carry a value
    // (would be a cross-wired parameter). Check every observed invocation.
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

    // Bluff-catch: nothing should have been invoked before the first click frame.
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

//! Headless smoke test for the `commands` example (a self-contained port of the
//! Noesis SDK's `Commands` `ICommand` sample). It `#[path]`-includes the example
//! and boots its **exact** app config ([`configure_commands`]) under the
//! headless `ScheduleRunnerPlugin`, so the thing under test is the same wiring a
//! user runs windowed.
//!
//! It drives the scene the way the demo expects — synthetic left-clicks through
//! the public [`NoesisInputQueue`] onto the three command `Button`s — and asserts
//! the command bridge reports real round-trips:
//!
//!   1. A click on `HelloButton` surfaces a [`NoesisCommandInvoked`] carrying our
//!      view entity, the name `"SayHello"`, and the decoded `CommandParameter`
//!      `Some("World")`.
//!   2. A click on `ByeButton` surfaces `"Goodbye"` with `Some("Moon")`.
//!   3. A click on `LockButton` — whose `Locked` command was disabled via
//!      [`NoesisCommands::set_enabled`](dm_noesis_bevy::NoesisCommands::set_enabled)
//!      — produces NO invocation. A disabled command's `CanExecute` is `false`,
//!      so the bound `Button` never fires it. This is the bluff-resistant
//!      negative control: clicking it the same way as the others must do nothing.
//!   4. The reaction's `NoesisText` write to `Output.Text` reads back through the
//!      `NoesisDp` watch as `"Hello, World!"` — a cross-bridge round-trip whose
//!      negative control is the default `"Awaiting command..."`.
//!
//! With the bridge inert, `Command="{Binding …}"` resolves to nothing and a click
//! produces no message at all — so observing the message (exact name + view +
//! parameter) is the positive signal, and the disabled command producing nothing
//! while its enabled siblings fire is the discriminating evidence.
//!
//!   `cargo test -p dm_noesis_bevy --test headless_example_commands -- --nocapture`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpValue, NoesisCommandInvoked, NoesisDpChanged, NoesisInputEvent, NoesisInputQueue,
};
use noesis_runtime::view::MouseButton;

// The example is a binary; included as a module here. Only some items are used.
#[allow(dead_code)]
#[path = "../examples/commands.rs"]
mod commands;

const CLICK_HELLO_AT_FRAME: usize = 16;
const CLICK_BYE_AT_FRAME: usize = 24;
const CLICK_LOCK_AT_FRAME: usize = 40;
const EXIT_AT_FRAME: usize = 90;

/// Inject a synthetic left-click (move -> down -> up) at view-pixel `(x, y)`.
/// These coordinates go straight onto the `View` (no window mapping headless).
fn left_click(queue: &mut NoesisInputQueue, (x, y): (i32, i32)) {
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

/// One observed invocation: the frame it surfaced on (to prove nothing fired
/// before its click), the view entity, the command name, and decoded parameter.
type Invocation = (usize, Entity, String, Option<String>);

#[test]
fn commands_example_invokes_enabled_and_gates_disabled() {
    noesis_license_from_env();

    let invocations: Arc<Mutex<Vec<Invocation>>> = Arc::new(Mutex::new(Vec::new()));
    let dp_changes: Arc<Mutex<Vec<(Entity, String, String, DpValue)>>> =
        Arc::new(Mutex::new(Vec::new()));
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

    // Boot the example's real app config — same view, same bridges.
    commands::configure_commands(&mut app);

    let inv_sys = Arc::clone(&invocations);
    let dp_sys = Arc::clone(&dp_changes);
    let view_sys = Arc::clone(&view_id);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut queue: ResMut<NoesisInputQueue>,
              cmd_view: Option<Res<commands::CommandsView>>,
              mut invoked: MessageReader<NoesisCommandInvoked>,
              mut changed: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if let Some(cv) = cmd_view {
                *view_sys.lock().unwrap() = Some(cv.0);
            }

            if *frame == CLICK_HELLO_AT_FRAME {
                left_click(&mut queue, commands::HELLO_CENTER);
            }
            if *frame == CLICK_BYE_AT_FRAME {
                left_click(&mut queue, commands::BYE_CENTER);
            }
            if *frame == CLICK_LOCK_AT_FRAME {
                left_click(&mut queue, commands::LOCK_CENTER);
            }

            for ev in invoked.read() {
                inv_sys.lock().unwrap().push((
                    *frame,
                    ev.view,
                    ev.name.clone(),
                    ev.parameter.clone(),
                ));
            }
            for ev in changed.read() {
                dp_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_id.lock().unwrap().expect("commands view spawned");
    let got = invocations.lock().unwrap().clone();
    let got_dp = dp_changes.lock().unwrap().clone();
    eprintln!("--- NoesisCommandInvoked ---");
    for (f, e, name, param) in &got {
        eprintln!("  frame {f}: {e:?} {name} param={param:?}");
    }
    eprintln!("--- NoesisDpChanged ---");
    for (e, name, prop, value) in &got_dp {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    // Positive #1: HelloButton invoked `SayHello` on our view with the decoded
    // CommandParameter "World". `None` here would mean the parameter never
    // flowed / decoded; no message at all would mean the binding never resolved.
    assert!(
        got.iter().any(|(_, e, name, param)| *e == view
            && name == "SayHello"
            && param.as_deref() == Some(commands::HELLO_PARAM)),
        "expected NoesisCommandInvoked {{ view: {view:?}, name: \"SayHello\", \
         parameter: Some({:?}) }}; got {got:?}",
        commands::HELLO_PARAM,
    );

    // Positive #2: ByeButton invoked `Goodbye` with "Moon".
    assert!(
        got.iter().any(|(_, e, name, param)| *e == view
            && name == "Goodbye"
            && param.as_deref() == Some(commands::BYE_PARAM)),
        "expected NoesisCommandInvoked {{ view: {view:?}, name: \"Goodbye\", \
         parameter: Some({:?}) }}; got {got:?}",
        commands::BYE_PARAM,
    );

    // Bluff-resistant negative control: `Locked` was disabled via set_enabled,
    // so clicking its button the SAME way as the others must invoke NOTHING.
    // A regression where set_enabled is a no-op would surface a `Locked`
    // invocation here.
    assert!(
        !got.iter().any(|(_, _, name, _)| name == "Locked"),
        "the disabled `Locked` command must not fire; got {got:?}",
    );

    // Nothing should have fired before the first click frame.
    assert!(
        got.iter().all(|(f, _, _, _)| *f >= CLICK_HELLO_AT_FRAME),
        "a command was invoked before the first synthetic click; got {got:?}",
    );

    // Cross-bridge round-trip: the SayHello reaction wrote `Output.Text` via
    // NoesisText; the DP watch read it back. The default "Awaiting command..."
    // is the negative control.
    assert!(
        got_dp.iter().any(|(e, name, prop, value)| *e == view
            && name == "Output"
            && prop == "Text"
            && *value == DpValue::Str("Hello, World!".to_string())),
        "SayHello reaction should drive Output.Text to \"Hello, World!\" \
         (default \"Awaiting command...\"); got {got_dp:?}",
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

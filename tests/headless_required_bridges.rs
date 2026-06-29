//! Regression test for the auto-attached bridges (the main-menu bug).
//!
//! Before required components, a write from `Startup`/`OnEnter` was lost when the
//! bridge component didn't exist yet: `NoesisUi::get_mut()` returned `None` and
//! nothing retried. `NoesisView` now pulls in every per-view bridge via required
//! components, so the component is always present and the write lands once the
//! scene builds.
//!
//! This test spawns a bare `NoesisView` (no bridge components added by hand),
//! writes text through `NoesisUi` before the scene exists, registers the XAML
//! late, and reads the value back through an auto-attached `NoesisDp` watch.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin, NoesisText, NoesisUi,
    NoesisView, XamlRegistry,
};

const FRAMES: usize = 60;
const WRITE_AT_FRAME: usize = 2;
const REGISTER_AT_FRAME: usize = 8;
const TARGET: &str = "Label";
const WRITTEN: &str = "Applied";

const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
    <TextBlock x:Name="Label" Text="default"/>
</Border>"##;

#[test]
fn write_without_spawning_the_bridge_survives() {
    noesis_runtime::set_license(
        &std::env::var("NOESIS_LICENSE_NAME").unwrap_or_default(),
        &std::env::var("NOESIS_LICENSE_KEY").unwrap_or_default(),
    );

    let observed: Arc<Mutex<Vec<(usize, String)>>> = Arc::new(Mutex::new(Vec::new()));
    // Records that the auto-attached components were reachable through NoesisUi
    // even though the test never spawned them.
    let bridges_present = Arc::new(AtomicBool::new(false));

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

    // A bare view: no NoesisText, no NoesisDp. Required components attach them.
    app.add_systems(Startup, |mut commands: Commands| {
        commands.spawn((
            Camera2d,
            NoesisCamera,
            NoesisView {
                xaml_uri: "late.xaml".to_string(),
                size: UVec2::new(128, 128),
                ..default()
            },
        ));
    });

    let present_sys = Arc::clone(&bridges_present);
    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut reg: ResMut<XamlRegistry>,
              mut writer: NoesisUi<(&mut NoesisText, &mut NoesisDp)>,
              mut changed: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            // Write before the scene exists, through the auto-attached bridges.
            if *frame == WRITE_AT_FRAME {
                if let Some((mut text, mut dp)) = writer.get_mut() {
                    present_sys.store(true, Ordering::SeqCst);
                    text.write(TARGET, WRITTEN);
                    dp.observe(TARGET, "Text", DpKind::Str);
                }
            }
            if *frame == REGISTER_AT_FRAME {
                reg.insert("late.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            }
            for ev in changed.read() {
                if ev.name == TARGET && ev.property == "Text" {
                    if let DpValue::Str(text) = &ev.value {
                        observed_sys.lock().unwrap().push((*frame, text.clone()));
                    }
                }
            }
            if *frame >= FRAMES {
                exit.write(AppExit::Success);
            }
        },
    );

    let exit = app.run();

    assert!(
        bridges_present.load(Ordering::SeqCst),
        "NoesisText/NoesisDp were not auto-attached to the bare NoesisView"
    );
    let seen = observed.lock().unwrap().clone();
    let applied = seen.iter().find(|(_, text)| text == WRITTEN);
    assert!(
        applied.is_some(),
        "write never reached the element; observed: {seen:?}"
    );
    assert!(
        applied.unwrap().0 >= REGISTER_AT_FRAME,
        "write applied before the scene could build"
    );
    assert!(matches!(exit, AppExit::Success), "app exited with {exit:?}");
}

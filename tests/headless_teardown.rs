//! Regression test for the main-world Noesis driving pipeline teardown.
//!
//! Stands up a **headless** Bevy app with the full `NoesisPlugin` — `DefaultPlugins`
//! (so the render sub-app and **pipelined rendering** are present) minus `WinitPlugin`,
//! driven by `ScheduleRunnerPlugin`. Seeds a *font-free* inline XAML (a solid `Border`,
//! so the scene builds with no font gate), pumps a handful of frames, then exits via
//! `AppExit` — exercising the real shutdown path.
//!
//! Guards two bugs found when Noesis moved to the main world (Phase 0):
//!  1. **Teardown ordering** — `NoesisRenderState::drop` must release every Noesis handle
//!     *before* the global `shutdown()` (it now owns `shutdown()` for exactly this reason).
//!  2. **Pipelined-cleanup deadlock** — no `NonSendMut<NoesisRenderState>` system may live in
//!     the render schedule, or Bevy's pipelined render-thread cleanup handshake deadlocks.
//!
//! If either regresses, `app.run()` hangs and the outer test timeout fails the run.

use std::sync::Arc;
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{NoesisCamera, NoesisPlugin, NoesisView, XamlRegistry};

const FRAMES: usize = 30;

// Solid blue Border — no text, so `ensure_scene` builds it without waiting on a font folder.
const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    Background="#FF3050FF"/>"##;

#[test]
fn headless_drive_and_teardown_do_not_hang() {
    noesis_runtime::set_license(
        &std::env::var("NOESIS_LICENSE_NAME").unwrap_or_default(),
        &std::env::var("NOESIS_LICENSE_KEY").unwrap_or_default(),
    );

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
    // Headless runner — drives frames + processes `AppExit` with the same clean shutdown
    // handshake as the real app (incl. the pipelined render thread, which stays ENABLED).
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(4)));
    app.add_plugins(NoesisPlugin::default());

    app.add_systems(
        Startup,
        |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("repro.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            commands.spawn((
                Camera2d,
                NoesisCamera,
                NoesisView {
                    xaml_uri: "repro.xaml".to_string(),
                    size: UVec2::new(256, 256),
                    ..default()
                },
            ));
        },
    );
    app.add_systems(
        Update,
        |mut frame: Local<usize>, mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            if *frame >= FRAMES {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();
}

//! Regression test for the stale-intermediate "frozen UI ghost" (audit P0.8),
//! component-removal variant.
//!
//! The sibling `headless_intermediate_ghost.rs` covers tearing the scene down by
//! clearing `xaml_uri`. This covers the *other* teardown path the first fix
//! missed: the caller drops only the `NoesisView` component while keeping the
//! entity alive (a game toggling its UI off but retaining `Camera2d`/
//! `NoesisCamera`, rather than despawning). `RemovedComponents<NoesisView>` fires
//! for both a despawn and a bare component drop; `teardown_for` prunes the entity
//! out of `publish_intermediates`' sweep, so the reap system must strip the stale
//! `NoesisIntermediate` off the survivor itself — otherwise the render world
//! blits the last-painted frame over live content forever.
//!
//! One `#[test]` per file (thread-affine Noesis runtime, one app per process).
//! Font-free XAML so the scene builds without a font folder.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{NoesisCamera, NoesisIntermediate, NoesisPlugin, NoesisView, XamlRegistry};

const URI: &str = "ghost.xaml";
const REMOVE_AT_FRAME: usize = 25;
const CAPTURE_HAD_AT: usize = 24;
const EXIT_AT_FRAME: usize = 55;

const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    Background="#FF3050FF"/>"##;

#[test]
fn removing_the_view_component_removes_the_published_intermediate() {
    noesis_license_from_env();

    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    // Presence of NoesisIntermediate on the view before the removal and after.
    let had_before: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let has_after: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));

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
            reg.insert(URI.to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: URI.to_string(),
                        size: UVec2::new(128, 128),
                        ..default()
                    },
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let view_sys = Arc::clone(&view_entity);
    let had_before_sys = Arc::clone(&had_before);
    let has_after_sys = Arc::clone(&has_after);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut commands: Commands,
              intermediates: Query<Entity, With<NoesisIntermediate>>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == CAPTURE_HAD_AT {
                *had_before_sys.lock().unwrap() = intermediates.iter().next().is_some();
            }
            // Drop only the component; the entity (Camera2d + NoesisCamera) lives on.
            if *frame == REMOVE_AT_FRAME {
                if let Some(view) = *view_sys.lock().unwrap() {
                    commands.entity(view).remove::<NoesisView>();
                }
            }
            if *frame == EXIT_AT_FRAME {
                *has_after_sys.lock().unwrap() = Some(intermediates.iter().next().is_some());
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let had_before = *had_before.lock().unwrap();
    let has_after = has_after
        .lock()
        .unwrap()
        .expect("post-removal intermediate presence captured");
    eprintln!(
        "--- intermediate ghost (component removal) had_before={had_before} has_after={has_after} ---"
    );

    assert!(
        had_before,
        "the view should have published a NoesisIntermediate before the component was removed",
    );
    assert!(
        !has_after,
        "removing NoesisView while the entity survives tears the scene down; the stale \
         NoesisIntermediate must be removed or the render world blits a frozen ghost over \
         live content forever",
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

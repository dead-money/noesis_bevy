//! Regression test for the stale-intermediate "frozen UI ghost" (audit P0.8).
//!
//! When a view's scene is torn down but the entity survives (here: `xaml_uri`
//! cleared to `""`), nothing used to remove the last-published
//! `NoesisIntermediate` component. The render world kept extracting and blitting
//! the final painted frame forever — a frozen ghost. This test drives a view
//! until it publishes an intermediate, clears its `xaml_uri` (tearing the scene
//! down while the entity lives on), and asserts the component is gone.
//!
//! Font-free XAML so the scene builds without a font folder.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{NoesisCamera, NoesisIntermediate, NoesisPlugin, NoesisView, XamlRegistry};

const URI: &str = "ghost.xaml";
const CLEAR_AT_FRAME: usize = 25;
const CAPTURE_HAD_AT: usize = 24;
const EXIT_AT_FRAME: usize = 55;

const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    Background="#FF3050FF"/>"##;

#[test]
fn clearing_xaml_uri_removes_the_published_intermediate() {
    noesis_license_from_env();

    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    // Presence of NoesisIntermediate on the view before the clear and after.
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

    let had_before_sys = Arc::clone(&had_before);
    let has_after_sys = Arc::clone(&has_after);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              intermediates: Query<Entity, With<NoesisIntermediate>>,
              mut views: Query<&mut NoesisView>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == CAPTURE_HAD_AT {
                *had_before_sys.lock().unwrap() = intermediates.iter().next().is_some();
            }
            // Clear the URI: teardown_scene runs, the entity survives.
            if *frame == CLEAR_AT_FRAME {
                for mut view in &mut views {
                    view.xaml_uri.clear();
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
        .expect("post-clear intermediate presence captured");
    eprintln!("--- intermediate ghost had_before={had_before} has_after={has_after} ---");

    assert!(
        had_before,
        "the view should have published a NoesisIntermediate before the URI was cleared",
    );
    assert!(
        !has_after,
        "clearing xaml_uri tears the scene down; the stale NoesisIntermediate must be \
         removed or the render world blits a frozen UI ghost forever",
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

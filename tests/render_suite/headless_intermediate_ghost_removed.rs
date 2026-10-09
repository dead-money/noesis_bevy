//! Stale-intermediate "frozen UI ghost", component-removal case.
//!
//! The sibling `headless_intermediate_ghost.rs` tears the scene down by clearing
//! `xaml_uri`. This one removes only the `NoesisView` component and keeps the
//! entity alive, as a game does when it toggles its UI off but keeps `Camera2d`
//! and `NoesisCamera`. `teardown_for` drops the entity from
//! `publish_intermediates`' sweep, so the `RemovedComponents<NoesisView>` reap
//! must strip the stale `NoesisIntermediate` itself. Otherwise the render world
//! keeps extracting it and blits the last-painted frame over live content.
//!
//! Runs on the real render graph ([`render_app`]) because the ghost comes from
//! render-world extraction. The XAML has no text, so no font folder is needed.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisIntermediate, NoesisView, XamlRegistry};

use crate::common::{render_app, run_until, settle};

const URI: &str = "ghost.xaml";
const REMOVE_AT_FRAME: usize = 25;
const CAPTURE_HAD_AT: usize = 24;
const CAPTURE_AFTER_AT: usize = 55;
// Frames pumped after the capture, before the app drops, to drain any in-flight
// pipeline compile (dropping mid-compile segfaults the GPU driver).
const SETTLE_FRAMES: usize = 60;
const CAP: usize = 240;

const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    Background="#FF3050FF"/>"##;

#[test]
fn removing_the_view_component_removes_the_published_intermediate() {
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));
    // Presence of NoesisIntermediate on the view before the removal and after.
    let had_before: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let has_after: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));

    let mut app = render_app();

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
              intermediates: Query<Entity, With<NoesisIntermediate>>| {
            *frame += 1;

            if *frame == CAPTURE_HAD_AT {
                *had_before_sys.lock().unwrap() = intermediates.iter().next().is_some();
            }
            // Drop only the component; the entity (Camera2d + NoesisCamera) lives on.
            if *frame == REMOVE_AT_FRAME
                && let Some(view) = *view_sys.lock().unwrap()
            {
                commands.entity(view).remove::<NoesisView>();
            }
            if *frame == CAPTURE_AFTER_AT {
                *has_after_sys.lock().unwrap() = Some(intermediates.iter().next().is_some());
            }
        },
    );

    let has_after_pred = Arc::clone(&has_after);
    let captured = run_until(&mut app, CAP, |_app| {
        has_after_pred.lock().unwrap().is_some()
    });
    assert!(
        captured,
        "post-removal intermediate presence never captured within {CAP} frames"
    );

    settle(&mut app, SETTLE_FRAMES);

    let had_before = *had_before.lock().unwrap();
    let has_after = has_after.lock().unwrap().unwrap();
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

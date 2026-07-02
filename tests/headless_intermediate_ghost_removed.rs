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
//! Runs on the real render graph ([`render_app`]) because the ghost is a
//! render-world extraction bug. One `#[test]` per file (thread-affine Noesis
//! runtime, one app per process). Font-free XAML so the scene builds without a
//! font folder.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisIntermediate, NoesisView, XamlRegistry};

mod common;
use common::{render_app, run_until, settle};

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
            if *frame == REMOVE_AT_FRAME {
                if let Some(view) = *view_sys.lock().unwrap() {
                    commands.entity(view).remove::<NoesisView>();
                }
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

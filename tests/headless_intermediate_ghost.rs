//! Regression test for the stale-intermediate "frozen UI ghost" (audit P0.8).
//!
//! When a view's scene is torn down but the entity survives (here: `xaml_uri`
//! cleared to `""`), nothing used to remove the last-published
//! `NoesisIntermediate` component. The render world kept extracting and blitting
//! the final painted frame forever — a frozen ghost. This test drives a view
//! until it publishes an intermediate, clears its `xaml_uri` (tearing the scene
//! down while the entity lives on), and asserts the component is gone.
//!
//! Runs on the real render graph ([`render_app`]) because the ghost is a
//! render-world extraction bug.
//!
//! Font-free XAML so the scene builds without a font folder.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisIntermediate, NoesisView, XamlRegistry};

mod common;
use common::{render_app, run_until, settle};

const URI: &str = "ghost.xaml";
const CLEAR_AT_FRAME: usize = 25;
const CAPTURE_HAD_AT: usize = 24;
const CAPTURE_AFTER_AT: usize = 55;
// Frames pumped after the capture, before the app drops, to drain any in-flight
// pipeline compile (dropping mid-compile segfaults the GPU driver).
const SETTLE_FRAMES: usize = 60;
const CAP: usize = 240;

const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    Background="#FF3050FF"/>"##;

#[test]
fn clearing_xaml_uri_removes_the_published_intermediate() {
    // Presence of NoesisIntermediate on the view before the clear and after.
    let had_before: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    let has_after: Arc<Mutex<Option<bool>>> = Arc::new(Mutex::new(None));

    let mut app = render_app();

    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(URI.to_string(), Arc::new(XAML.as_bytes().to_vec()));
            commands.spawn((
                Camera2d,
                NoesisCamera,
                NoesisView {
                    xaml_uri: URI.to_string(),
                    size: UVec2::new(128, 128),
                    ..default()
                },
            ));
        },
    );

    let had_before_sys = Arc::clone(&had_before);
    let has_after_sys = Arc::clone(&has_after);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              intermediates: Query<Entity, With<NoesisIntermediate>>,
              mut views: Query<&mut NoesisView>| {
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
        "post-clear intermediate presence never captured within {CAP} frames"
    );

    settle(&mut app, SETTLE_FRAMES);

    let had_before = *had_before.lock().unwrap();
    let has_after = has_after.lock().unwrap().unwrap();
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

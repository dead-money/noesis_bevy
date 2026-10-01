//! Despawn-teardown regression: despawning a `NoesisView` must reap its Noesis
//! state, not leak it.
//!
//! Before the `teardown_removed_views` hook there was *no* removal handling in
//! the crate, so a despawned view's `!Send` scene + side-table entry lived for
//! the whole process. This test drives a view until its scene exists
//! (`live_scenes == 1`), despawns the entity, and asserts the live-scene count
//! drains back to 0, i.e. the side-table entry is gone. It also doubles as a
//! smoke test for the FFI-hop instrumentation (a built scene must have resolved
//! at least one name, so `ffi_hops > 0`).
//!
//! Font-free XAML so the scene builds without a font folder.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisDiagnostics, NoesisDp, NoesisView, XamlRegistry};

use crate::common::{headless_app, run_until};

const CAPTURE_PRE_AT: usize = 20;
const DESPAWN_AT: usize = 21;
const CAPTURE_POST_AT: usize = 45;

const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    x:Name="Panel"
    xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
    Background="#FF3050FF"/>"##;

#[test]
fn despawning_a_view_reaps_its_noesis_state() {
    let pre: Arc<Mutex<Option<NoesisDiagnostics>>> = Arc::new(Mutex::new(None));
    let post: Arc<Mutex<Option<NoesisDiagnostics>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    app.add_systems(
        Startup,
        |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "despawn.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            commands.spawn((
                Camera2d,
                NoesisCamera,
                NoesisView {
                    xaml_uri: "despawn.xaml".to_string(),
                    size: UVec2::new(128, 128),
                    ..default()
                },
                // A live DP write so the dp bridge resolves "Panel" every frame,
                // exercising the FFI-hop instrumentation (resolve_named + DP set).
                NoesisDp::new().set_f32("Panel", "Opacity", 0.5),
            ));
        },
    );

    let pre_sys = Arc::clone(&pre);
    let post_sys = Arc::clone(&post);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              diag: Res<NoesisDiagnostics>,
              views: Query<Entity, With<NoesisView>>,
              mut commands: Commands| {
            *frame += 1;
            if *frame == CAPTURE_PRE_AT {
                *pre_sys.lock().unwrap() = Some(*diag);
            }
            if *frame == DESPAWN_AT {
                // Despawn every live view; teardown must run off RemovedComponents.
                for e in &views {
                    commands.entity(e).despawn();
                }
            }
            if *frame == CAPTURE_POST_AT {
                *post_sys.lock().unwrap() = Some(*diag);
            }
        },
    );

    // Exit once the post-despawn snapshot shows the view's scene reaped.
    let pred_post = Arc::clone(&post);
    let reaped = run_until(
        &mut app,
        240,
        move |_app| matches!(*pred_post.lock().unwrap(), Some(d) if d.live_scenes == 0),
    );

    let pre = snapshot(&pre, "pre-despawn");
    let post = snapshot(&post, "post-despawn");
    eprintln!("--- despawn teardown pre={pre:?} post={post:?} ---");

    assert!(
        reaped,
        "view scene never reaped to 0 live scenes within 240 frames; post={post:?}",
    );

    // Before despawn: the scene built (so its side-table entry exists) and the
    // bridges resolved at least one name through FFI.
    assert_eq!(
        pre.live_scenes, 1,
        "scene should be live before despawn; got {} live scenes",
        pre.live_scenes,
    );
    assert!(
        pre.ffi_hops > 0,
        "a built scene must have made at least one FFI hop; got {}",
        pre.ffi_hops,
    );

    // After despawn: the scene's side-table entry is gone, no leak.
    assert_eq!(
        post.live_scenes, 0,
        "despawn must reap the view's scene; {} live scenes still tracked",
        post.live_scenes,
    );
    // The hop counter is cumulative, so it never goes backwards on teardown.
    assert!(
        post.ffi_hops >= pre.ffi_hops,
        "ffi_hops is cumulative and must not regress (pre={}, post={})",
        pre.ffi_hops,
        post.ffi_hops,
    );
}

fn snapshot(slot: &Arc<Mutex<Option<NoesisDiagnostics>>>, which: &str) -> NoesisDiagnostics {
    slot.lock()
        .unwrap()
        .unwrap_or_else(|| panic!("{which} diagnostics snapshot captured"))
}

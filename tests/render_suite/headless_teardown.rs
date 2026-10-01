//! Driving and dropping a Noesis app on the real render graph must not hang.
//!
//! Guards two failure modes:
//!  1. Teardown order: `NoesisRenderState::drop` must release every Noesis handle
//!     before the global `shutdown()`, which is why it owns the `shutdown()` call.
//!  2. Pipelined-cleanup deadlock: a `NonSendMut<NoesisRenderState>` system in the
//!     render schedule deadlocks Bevy's pipelined render-thread cleanup handshake.
//!
//! A regression hangs the app and the outer test timeout fails the run. Uses
//! [`render_app`] so the pipelined render thread actually runs.

use std::sync::Arc;

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisIntermediate, NoesisView, XamlRegistry};

use crate::common::{render_app, run_until, settle};

// Frames pumped after the scene is up, before the app drops: a pipeline may still
// be compiling on a driver thread, and dropping mid-compile segfaults the driver.
const SETTLE_FRAMES: usize = 180;
const CAP: usize = 240;

// No text element, so the scene builds without a font folder.
const XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    Background="#FF3050FF"/>"##;

#[test]
fn headless_drive_and_teardown_do_not_hang() {
    let mut app = render_app();

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

    // The scene is live once it publishes an intermediate: that means the render
    // graph actually ran, which is the state teardown must unwind cleanly.
    let up = run_until(&mut app, CAP, |app| {
        let mut q = app
            .world_mut()
            .query_filtered::<(), With<NoesisIntermediate>>();
        q.iter(app.world()).next().is_some()
    });
    assert!(
        up,
        "view never published a NoesisIntermediate within {CAP} frames"
    );

    settle(&mut app, SETTLE_FRAMES);

    drop(app);
}

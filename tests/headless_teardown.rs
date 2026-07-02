//! Regression test for Noesis teardown ordering and pipelined-cleanup deadlock.
//!
//! Guards two bugs:
//!  1. Teardown ordering: `NoesisRenderState::drop` must release every Noesis handle
//!     before the global `shutdown()` (it owns `shutdown()` for exactly this reason).
//!  2. Pipelined-cleanup deadlock: no `NonSendMut<NoesisRenderState>` system may live in
//!     the render schedule, or Bevy's pipelined render-thread cleanup handshake deadlocks.
//!
//! If either regresses, driving or dropping the app hangs and the outer test timeout
//! fails the run. Runs on the real render graph ([`render_app`]) so the pipelined
//! render thread the deadlock lives on is actually spun up.

use std::sync::Arc;

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisIntermediate, NoesisView, XamlRegistry};

mod common;
use common::{render_app, run_until, settle};

// Frames to keep pumping after the scene is up, before the app drops. The scene
// coming up can leave Bevy async-compiling a render pipeline on a driver thread;
// dropping mid-compile segfaults the GPU driver, so we drain that first.
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

    // Drain any in-flight pipeline compile before the drop below tears down.
    settle(&mut app, SETTLE_FRAMES);

    // Dropping `app` here exercises the teardown ordering + pipelined-cleanup path.
    drop(app);
}

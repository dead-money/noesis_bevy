//! Regression test for the label-baker cross-world panic.
//!
//! `bake_pending_labels` runs in the main world (it drives the `!Send`
//! `NoesisRenderState`), but it used to take `Res<RenderAssets<GpuImage>>`, which
//! only exists in the render world. The instant the system ran, system-param
//! validation failed with "Resource does not exist" and the app panicked.
//!
//! The fix splits the work: a render-world system resolves each target's GPU
//! texture and hands it back through the baker's shared state; the main-world
//! system pulls the resolved texture and bakes. This test queues one label, runs
//! on the real render graph ([`render_app`], so both worlds are live), and
//! asserts the bake completes ([`NoesisLabelBaker::pending_count`] drops to zero)
//! without panic.
//!
//! Skips (passes) when `$NOESIS_SDK_DIR` is unset: the bake gates on an installed
//! font, and the font is read from the SDK at runtime, never vendored.
//!
//!   `cargo test -p noesis_bevy --test headless_bake_label -- --nocapture`

use std::path::PathBuf;
use std::sync::Arc;

use bevy::prelude::*;
use noesis_bevy::{
    FontRegistry, NoesisCamera, NoesisLabelBaker, NoesisLabelBakerPlugin, NoesisView, XamlRegistry,
};

mod common;
use common::{render_app, run_until, settle};

const CAP: usize = 240;
// Frames pumped after the bake drains, before the app drops. The scene can leave
// Bevy async-compiling a render pipeline on a driver thread; dropping mid-compile
// segfaults the GPU driver, so we drain that first.
const SETTLE_FRAMES: usize = 180;
const BAKE_URI: &str = "bake_label.xaml";

// A named TextBlock the bake writes into. Text forces the font gate, which is
// exactly the path the real label baker exercises.
const BAKE_XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
    Background="#FF101010">
    <TextBlock x:Name="Label" Text="placeholder" Foreground="#FFFFFFFF"/>
</Border>"##;

// A text-free live view, so the scene builds and installs the font fallback
// chain (which flips the bake's font gate) without needing its own glyphs.
const LIVE_XAML: &str = r##"<Border xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    Background="#FF3050FF"/>"##;

fn sdk_font() -> Option<(PathBuf, Vec<u8>)> {
    let dir = std::env::var("NOESIS_SDK_DIR").ok()?;
    let path = PathBuf::from(dir).join("Data/Fonts/Roboto-Bold.ttf");
    let bytes = std::fs::read(&path).ok()?;
    Some((path, bytes))
}

#[test]
fn bake_label_completes_without_cross_world_panic() {
    let Some((_, font_bytes)) = sdk_font() else {
        eprintln!("NOESIS_SDK_DIR unset or font missing; skipping bake_label test");
        return;
    };
    let font_bytes = Arc::new(font_bytes);

    let mut app = render_app();
    app.add_plugins(NoesisLabelBakerPlugin);

    app.add_systems(
        Startup,
        move |mut commands: Commands,
              mut reg: ResMut<XamlRegistry>,
              mut fonts: ResMut<FontRegistry>,
              mut images: ResMut<Assets<Image>>,
              baker: Res<NoesisLabelBaker>| {
            reg.insert(
                BAKE_URI.to_string(),
                Arc::new(BAKE_XAML.as_bytes().to_vec()),
            );
            reg.insert(
                "live.xaml".to_string(),
                Arc::new(LIVE_XAML.as_bytes().to_vec()),
            );
            fonts.insert("Fonts", "Roboto-Bold.ttf", Arc::clone(&font_bytes));

            // A live view drives the scene path that installs the font fallback
            // chain, which the bake's font gate waits on.
            commands.spawn((
                Camera2d,
                NoesisCamera,
                NoesisView {
                    xaml_uri: "live.xaml".to_string(),
                    size: UVec2::new(128, 128),
                    ..default()
                },
            ));

            let _handle = baker.bake_label(
                "hello",
                BAKE_URI,
                UVec2::new(256, 64),
                vec![("Label".to_string(), "Hello".to_string())],
                &mut images,
            );
            assert_eq!(
                baker.pending_count(),
                1,
                "label should be queued at startup"
            );
        },
    );

    // Drive until the bake drains. The predicate is the real success condition
    // (no cross-world panic occurred and pending fell to zero), not a frame count.
    let completed = run_until(&mut app, CAP, |app| {
        app.world().resource::<NoesisLabelBaker>().pending_count() == 0
    });
    assert!(
        completed,
        "bake never completed within {CAP} frames (pending labels remain)"
    );

    // Drain any in-flight pipeline compile before dropping the app.
    settle(&mut app, SETTLE_FRAMES);
}

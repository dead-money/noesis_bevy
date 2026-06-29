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
//! with pipelined rendering on (so both worlds are live), and asserts the bake
//! completes ([`NoesisLabelBaker::pending_count`] drops to zero) without panic.
//!
//! Skips (passes) when `$NOESIS_SDK_DIR` is unset: the bake gates on an installed
//! font, and the font is read from the SDK at runtime, never vendored.
//!
//!   `cargo test -p noesis_bevy --test headless_bake_label -- --nocapture`

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    FontRegistry, NoesisCamera, NoesisLabelBaker, NoesisLabelBakerPlugin, NoesisPlugin, NoesisView,
    XamlRegistry,
};

const FRAMES: usize = 180;
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
    noesis_runtime::set_license(
        &std::env::var("NOESIS_LICENSE_NAME").unwrap_or_default(),
        &std::env::var("NOESIS_LICENSE_KEY").unwrap_or_default(),
    );

    let Some((_, font_bytes)) = sdk_font() else {
        eprintln!("NOESIS_SDK_DIR unset or font missing; skipping bake_label test");
        return;
    };
    let font_bytes = Arc::new(font_bytes);

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
    // Pipelined rendering on: the render world and main world run concurrently,
    // exercising the cross-world texture handoff the fix relies on.
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(4)));
    app.add_plugins(NoesisPlugin::default());
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

    // Latch completion when the bake drains, but keep running to a fixed frame
    // budget before exiting. Exiting the instant the bake finishes can tear the
    // app down while Bevy is still async-compiling a pipeline on a background
    // thread, which segfaults the GPU driver. A fixed budget lets that settle,
    // matching the other headless tests.
    let completed = Arc::new(AtomicBool::new(false));
    let completed_sys = Arc::clone(&completed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              baker: Res<NoesisLabelBaker>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            if baker.pending_count() == 0 {
                completed_sys.store(true, Ordering::SeqCst);
            }
            if *frame >= FRAMES {
                exit.write(AppExit::Success);
            }
        },
    );

    let exit = app.run();

    assert!(
        completed.load(Ordering::SeqCst),
        "bake never completed within {FRAMES} frames (pending labels remain)"
    );
    assert!(matches!(exit, AppExit::Success), "app exited with {exit:?}");
}

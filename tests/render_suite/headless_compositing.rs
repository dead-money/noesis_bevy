//! Regression test proving the Noesis UI actually *composites into pixels*.
//!
//! The other `render_suite` tests assert render-world bookkeeping (an intermediate
//! is published, a stale one is torn down) but none read the final image back, so
//! all four stay green even with the blit stubbed out. This one closes that gap:
//! a `Camera2d` renders to an offscreen `Image`, a solid-red XAML fills the view,
//! and a `Screenshot` of that image target is read back so we can assert the red
//! landed in the pixels.
//!
//! It fails if the `Core2d` blit system (`noesis_blit_2d`) is not registered: with
//! no blit the image keeps its black clear colour and the red centre pixel never
//! appears, so `run_until` times out and the assert fires. (Verified by commenting
//! the `.add_systems(Core2d, …)` wiring: the test goes red.)
//!
//! Read-back is via the one-shot `Screenshot` component, not the persistent
//! `Readback`: a `Screenshot` self-despawns after it fires, so no per-frame GPU
//! buffer map survives into app teardown (a mid-work drop is the documented driver
//! SIGSEGV). We stop issuing screenshots the moment red is seen and then `settle`
//! so any last in-flight capture completes before the app drops.
//!
//! Runs on the real render graph (`render_app`) because compositing is a
//! render-world blit. One `#[test]` per file (thread-affine Noesis runtime, one
//! app per process). Font-free XAML so the scene builds without a font folder.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use bevy::asset::RenderAssetUsages;
use bevy::camera::RenderTarget;
use bevy::image::Image;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};
use bevy::render::view::screenshot::{Screenshot, ScreenshotCaptured};
use noesis_bevy::{NoesisCamera, NoesisView, XamlRegistry};

use crate::common::{render_app, run_until, settle};

const URI: &str = "compositing.xaml";
const SIZE: u32 = 128;
const CAP: usize = 240;
// Let Noesis build + paint the scene and the blit composite it before the first
// screenshot; static full-screen UI is steady long before this.
const WARMUP: usize = 30;
// Re-issue a screenshot this often (frames) until red is captured, so a single
// mistimed capture cannot false-fail.
const SCREENSHOT_EVERY: usize = 6;
// Frames pumped after red is seen so any last in-flight capture self-despawns
// before the app drops (dropping mid-work segfaults the GPU driver).
const SETTLE_FRAMES: usize = 60;

// A solid-red Grid filling the whole view (cribbed from assets/test.xaml).
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    Background="Red"/>"##;

/// The centre pixel of `data` (RGBA8, tightly packed at `SIZE`×`SIZE`) reads as
/// red: a high red channel with low green/blue. The camera clears to black, so
/// only the composited UI can turn the centre red.
fn centre_is_red(data: &[u8]) -> bool {
    let px = ((SIZE as usize / 2) * SIZE as usize + SIZE as usize / 2) * 4;
    data.get(px..px + 4)
        .is_some_and(|rgba| rgba[0] > 200 && rgba[1] < 80 && rgba[2] < 80)
}

#[test]
fn the_ui_composites_into_the_camera_target_pixels() {
    // Set once the observer sees a red centre pixel in a captured screenshot.
    let red_seen = Arc::new(AtomicBool::new(false));
    let handle: Arc<Mutex<Option<Handle<Image>>>> = Arc::new(Mutex::new(None));

    let mut app = render_app();

    let handle_startup = Arc::clone(&handle);
    app.add_systems(
        Startup,
        move |mut commands: Commands,
              mut images: ResMut<Assets<Image>>,
              mut reg: ResMut<XamlRegistry>| {
            reg.insert(URI.to_string(), Arc::new(XAML.as_bytes().to_vec()));

            let mut image = Image::new_fill(
                Extent3d {
                    width: SIZE,
                    height: SIZE,
                    depth_or_array_layers: 1,
                },
                TextureDimension::D2,
                &[0, 0, 0, 255],
                TextureFormat::Rgba8UnormSrgb,
                RenderAssetUsages::default(),
            );
            // RENDER_ATTACHMENT: the camera draws into it. COPY_SRC: the screenshot
            // copies it back out. TEXTURE_BINDING/COPY_DST: defaults for an image.
            image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING
                | TextureUsages::COPY_DST
                | TextureUsages::COPY_SRC
                | TextureUsages::RENDER_ATTACHMENT;
            let image_handle = images.add(image);

            commands.spawn((
                Camera2d,
                Camera {
                    // Distinct from red so a missing blit leaves a black centre.
                    clear_color: ClearColorConfig::Custom(Color::BLACK),
                    ..default()
                },
                RenderTarget::from(image_handle.clone()),
                NoesisCamera,
                NoesisView {
                    xaml_uri: URI.to_string(),
                    size: UVec2::new(SIZE, SIZE),
                    ..default()
                },
            ));
            *handle_startup.lock().unwrap() = Some(image_handle);
        },
    );

    // Periodically screenshot the camera's image target until red is seen.
    let handle_shot = Arc::clone(&handle);
    let red_shot = Arc::clone(&red_seen);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>, mut commands: Commands| {
            *frame += 1;
            if red_shot.load(Ordering::Relaxed)
                || *frame < WARMUP
                || !(*frame).is_multiple_of(SCREENSHOT_EVERY)
            {
                return;
            }
            if let Some(h) = handle_shot.lock().unwrap().clone() {
                commands.spawn(Screenshot::image(h));
            }
        },
    );

    let red_obs = Arc::clone(&red_seen);
    app.add_observer(move |on: On<ScreenshotCaptured>| {
        if on.image.data.as_deref().is_some_and(centre_is_red) {
            red_obs.store(true, Ordering::Relaxed);
        }
    });

    let red_pred = Arc::clone(&red_seen);
    let composited = run_until(&mut app, CAP, |_app| red_pred.load(Ordering::Relaxed));

    // Drain any last in-flight capture (screenshots self-despawn once fired) so
    // the app drops with no GPU work outstanding.
    settle(&mut app, SETTLE_FRAMES);

    assert!(
        composited,
        "the solid-red UI never appeared in the camera's target image within \
         {CAP} frames: the Core2d blit system did not composite the Noesis \
         intermediate onto the ViewTarget",
    );
}

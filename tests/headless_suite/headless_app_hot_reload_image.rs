//! Integration test for *image* hot-reload through the real `NoesisPlugin`
//! pipeline.
//!
//! Noesis caches a decoded texture per URI and never re-issues `LoadTexture`
//! for it, so swapping an image's bytes only reaches the screen if the view
//! rebuilds. Re-inserting the same URI with new pixel dimensions must, after
//! the epoch-driven rebuild, re-run `GetTextureInfo` and re-size the `<Image>`.
//!
//! `Stretch="None"` sizes the Image to its source's pixel dimensions, observed
//! via a `NoesisDp` watch on `ActualWidth` — a layout-only effect, no GPU pass
//! or font setup needed (mirrors `headless_app_imaging`).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, ImageRegistry, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisView,
    XamlRegistry,
};

use crate::common::{headless_app, run_until};

const URI: &str = "img.xaml";
const IMG_URI: &str = "dm-bitmap://logo";
const W1: u32 = 13;
const W2: u32 = 21;
const RELOAD_AT_FRAME: usize = 25;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="64">
  <Image x:Name="Pic" Source="dm-bitmap://logo" Stretch="None"
         HorizontalAlignment="Left" VerticalAlignment="Top"/>
</Grid>"##;

// RGBA8 bytes sized to match the declared dimensions; a fresh `Arc` each call is
// what `refresh_images` compares by pointer to bump the image epoch.
fn rgba(w: u32, h: u32) -> Arc<Vec<u8>> {
    Arc::new(vec![0xAB; (w * h * 4) as usize])
}

type Observed = Vec<(Entity, String, DpValue)>;

#[test]
fn image_reload_rebuilds_view_with_new_dimensions() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands,
              mut reg: ResMut<XamlRegistry>,
              mut images: ResMut<ImageRegistry>| {
            reg.insert(URI.to_string(), Arc::new(XAML.as_bytes().to_vec()));
            images.insert(IMG_URI.to_string(), W1, 7, rgba(W1, 7));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: URI.to_string(),
                        size: UVec2::new(64, 64),
                        wait_for_images: vec![IMG_URI.to_string()],
                        ..default()
                    },
                    NoesisDp::new().watch("Pic", "ActualWidth", DpKind::F32),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut images: ResMut<ImageRegistry>,
              mut changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;

            // Same URI, new pixel dimensions + fresh Arc: the image-bytes swap
            // a live edit produces.
            if *frame == RELOAD_AT_FRAME {
                images.insert(IMG_URI.to_string(), W2, 11, rgba(W2, 11));
            }

            for ev in changes.read() {
                // `ev.name` is the x:Name ("Pic"); `ev.property` is the DP name
                // ("ActualWidth") — the discriminator this test keys on.
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.property.clone(), ev.value.clone()));
            }
        },
    );

    let want2 = f64::from(W2);
    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let reloaded = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        let saw_first = got
            .iter()
            .any(|(e, n, v)| *e == view && n == "ActualWidth" && dp_f32(v) == Some(f64::from(W1)));
        let latest_second = got
            .iter()
            .rfind(|(e, n, _)| *e == view && n == "ActualWidth")
            .and_then(|(_, _, v)| dp_f32(v))
            == Some(want2);
        saw_first && latest_second
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    let widths: Vec<Option<f64>> = got
        .iter()
        .filter(|(e, n, _)| *e == view && n == "ActualWidth")
        .map(|(_, _, v)| dp_f32(v))
        .collect();

    assert!(
        reloaded,
        "image reload never converged to width {W2} within 240 frames; got {widths:?}",
    );
    assert!(
        widths.contains(&Some(f64::from(W1))),
        "expected the original image width {W1} before reload; got {widths:?}",
    );
    assert_eq!(
        widths.last().copied().flatten(),
        Some(f64::from(W2)),
        "swapping the image bytes should rebuild the view so GetTextureInfo re-sizes \
         the Image to {W2} (a no-rebuild path would stay at {W1}); got {widths:?}",
    );
}

fn dp_f32(v: &DpValue) -> Option<f64> {
    match v {
        DpValue::F32(f) => Some(f64::from(*f)),
        _ => None,
    }
}

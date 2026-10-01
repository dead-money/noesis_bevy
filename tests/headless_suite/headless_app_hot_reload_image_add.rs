//! Regression test: *adding* a new image to [`ImageRegistry`] must NOT rebuild
//! live views.
//!
//! A newly-added image URI cannot affect any already-built scene — nothing
//! referenced it at build time — so it must not bump the image epoch or trigger
//! a rebuild. The bug this guards against made every `ImageRegistry` insertion
//! rebuild every scene each frame, so an app that trickles procedural images in
//! (e.g. palette-preview thumbnails, one per frame at startup) rebuilt its whole
//! UI continuously.
//!
//! Detection: a rebuild tears down and re-builds the scene, which re-emits the
//! watched `ActualWidth` DP. We stage a fresh unrelated image every frame and
//! assert the view emits `ActualWidth` only for its initial build — not once per
//! frame. Mirrors `headless_app_hot_reload_image`'s harness.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, ImageRegistry, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisView, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const URI: &str = "img_add.xaml";
const IMG_URI: &str = "dm-bitmap://logo";
const W1: u32 = 13;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="64">
  <Image x:Name="Pic" Source="dm-bitmap://logo" Stretch="None"
         HorizontalAlignment="Left" VerticalAlignment="Top"/>
</Grid>"##;

fn rgba(w: u32, h: u32) -> Arc<Vec<u8>> {
    Arc::new(vec![0xAB; (w * h * 4) as usize])
}

type Observed = Vec<(Entity, String)>;

#[test]
fn adding_unrelated_image_does_not_rebuild_view() {
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
        move |mut frame: Local<u32>,
              mut images: ResMut<ImageRegistry>,
              mut changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;
            // Every frame from the fifth on, stage a *new* unrelated image URI —
            // exactly the "procedural image trickling in" pattern. Under the bug
            // each of these rebuilt the view; under the fix none does.
            if *frame >= 5 {
                let uri = format!("dm-bitmap://extra-{}", *frame);
                images.insert(uri, 4, 4, rgba(4, 4));
            }
            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.property.clone()));
            }
        },
    );

    // Run a fixed span; there's no convergence event — we're asserting the
    // *absence* of repeated rebuilds while unrelated images stream in.
    run_until(&mut app, 120, |_app| false);

    let view = view_entity.lock().unwrap().expect("view spawned");
    let width_emits = observed
        .lock()
        .unwrap()
        .iter()
        .filter(|(e, n)| *e == view && n == "ActualWidth")
        .count();

    // The initial build emits ActualWidth (allow a small margin for build/attach
    // settling). A per-add rebuild would emit it ~1×/frame over ~115 frames.
    assert!(
        width_emits <= 3,
        "adding unrelated images rebuilt the view: ActualWidth emitted {width_emits} times \
         (expected ≤3 from the initial build; ~1/frame indicates the add-triggers-rebuild bug)",
    );
}

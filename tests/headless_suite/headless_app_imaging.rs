//! Integration test for the [`NoesisImaging`] bridge, on the headless harness.
//!
//! A `13x7` RGBA8 bitmap staged via [`NoesisImaging`] must drive `Pic.ActualWidth = 13`
//! and `Pic.ActualHeight = 7`. Noesis sizes `<Image Stretch="None"/>` from the source
//! pixel dimensions returned by `GetTextureInfo` during layout, so no GPU pass is needed.
//!
//! Two channels observe the same effect for independent corroboration: [`NoesisImageChanged`]
//! readback (`actual_size`) and a [`NoesisDp`] watch on `ActualWidth`.
//!
//! A second Image with an unregistered URI (negative control) must stay at `0`, proving the
//! size came from the staged bytes, not the container or a Stretch default.
//!
//! Staging happens at spawn time because Noesis resolves a `BitmapImage` source once at
//! scene build and does not retry. The bridge stages before the registry→provider sync so a
//! same-frame spawn lands in time.
//!
//! Font-free XAML: only sizes are asserted, no font gate needed.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, ImageReadback, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisImageChanged,
    NoesisImaging, NoesisView, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const BMP_W: u32 = 13;
const BMP_H: u32 = 7;
const BMP_URI: &str = "dm-bitmap://logo";

// Stretch="None" sizes each Image to its source's pixel dimensions.
// "Pic" is driven by the bridge; "Empty" uses an unregistered URI (negative control).
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="64">
  <Image x:Name="Pic" Source="dm-bitmap://logo" Stretch="None"
         HorizontalAlignment="Left" VerticalAlignment="Top"/>
  <Image x:Name="Empty" Source="dm-bitmap://never-registered" Stretch="None"
         HorizontalAlignment="Left" VerticalAlignment="Top"/>
</Grid>"##;

#[test]
fn imaging_bridge_drives_image_from_rust_bitmap() {
    let dp_observed: Arc<Mutex<Vec<(Entity, String, String, DpValue)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let img_observed: Arc<Mutex<Vec<(Entity, String, ImageReadback)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "imaging.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "imaging.xaml".to_string(),
                        size: UVec2::new(64, 64),
                        ..default()
                    },
                    // At spawn: Noesis resolves BitmapImage source once at scene build, no retry.
                    // Bridge stages before provider sync so this lands in time.
                    NoesisImaging::new().set(
                        "Pic",
                        BMP_URI,
                        BMP_W,
                        BMP_H,
                        Arc::new(vec![255u8; (BMP_W * BMP_H * 4) as usize]),
                    ),
                    // Independent observation of the same effect.
                    NoesisDp::new()
                        .watch("Pic", "ActualWidth", DpKind::F32)
                        .watch("Empty", "ActualWidth", DpKind::F32),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let dp_sys = Arc::clone(&dp_observed);
    let img_sys = Arc::clone(&img_observed);
    app.add_systems(
        Update,
        move |mut dp_changes: MessageReader<NoesisDpChanged>,
              mut img_changes: MessageReader<NoesisImageChanged>| {
            for ev in dp_changes.read() {
                dp_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }
            for ev in img_changes.read() {
                img_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.readback));
            }
        },
    );

    // Latest read-backs, keyed by the spawned view; read by the exit predicate.
    let latest_img = |img: &[(Entity, String, ImageReadback)],
                      view: Entity,
                      name: &str|
     -> Option<ImageReadback> {
        img.iter()
            .rfind(|(e, n, _)| *e == view && n == name)
            .map(|(_, _, rb)| *rb)
    };
    let latest_dp = |dp: &[(Entity, String, String, DpValue)],
                     view: Entity,
                     name: &str,
                     prop: &str|
     -> Option<DpValue> {
        dp.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // Event-driven exit: stop once the staged bitmap has sized Pic on both channels
    // and the negative control has reported its default 0.
    let pred_dp = Arc::clone(&dp_observed);
    let pred_img = Arc::clone(&img_observed);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let img = pred_img.lock().unwrap();
        let dp = pred_dp.lock().unwrap();
        latest_img(&img, view, "Pic").map(|rb| rb.actual_size) == Some([BMP_W as f32, BMP_H as f32])
            && latest_dp(&dp, view, "Pic", "ActualWidth") == Some(DpValue::F32(BMP_W as f32))
            && latest_dp(&dp, view, "Empty", "ActualWidth") == Some(DpValue::F32(0.0))
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let dp = dp_observed.lock().unwrap().clone();
    let img = img_observed.lock().unwrap().clone();

    eprintln!("--- NoesisImageChanged ---");
    for (e, name, rb) in &img {
        eprintln!("  {e:?} {name} -> {rb:?}");
    }
    eprintln!("--- NoesisDpChanged ---");
    for (e, name, prop, value) in &dp {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    assert!(
        converged,
        "imaging never sized Pic from the staged bitmap within 240 frames; \
         img {img:?} dp {dp:?}",
    );

    let pic = latest_img(&img, view, "Pic").expect("expected a NoesisImageChanged for Pic");
    assert!(
        pic.has_source,
        "Pic should have a non-null Source (declared in XAML)",
    );
    assert_eq!(
        pic.actual_size,
        [BMP_W as f32, BMP_H as f32],
        "imaging: a {BMP_W}x{BMP_H} staged bitmap should size Pic to [{BMP_W}, {BMP_H}] \
         (default 0); got {:?}",
        pic.actual_size,
    );

    // Independent corroboration via the generic DP bridge.
    assert_eq!(
        latest_dp(&dp, view, "Pic", "ActualWidth"),
        Some(DpValue::F32(BMP_W as f32)),
        "imaging: Pic.ActualWidth should resolve to the staged bitmap width {BMP_W}",
    );

    // Negative control: unregistered URI stays 0, proving size came from staged bytes
    // and the bridge only touched its target.
    assert_eq!(
        latest_dp(&dp, view, "Empty", "ActualWidth"),
        Some(DpValue::F32(0.0)),
        "imaging: an Image with an unregistered Source must measure to 0",
    );
}

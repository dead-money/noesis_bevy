//! Bevy-app-level integration test for the per-element **imaging** bridge
//! ([`NoesisImaging`]), exercised end-to-end through the real `NoesisPlugin`
//! pipeline (headless, pipelined rendering on).
//!
//! # Observable
//!
//! The bridge stages a Rust-provided RGBA8 bitmap into the [`ImageRegistry`]
//! under a `uri`, and a `<Image x:Name=… Source=uri Stretch="None"/>` resolves
//! it through the live `Noesis::TextureProvider`. We observe the effect through
//! the element's resolved layout size, surfaced two independent ways:
//!
//!   * the bridge's own [`NoesisImageChanged`] read-back
//!     (`readback.actual_size`), and
//!   * an independent [`NoesisDp`] watch on the element's `ActualWidth`.
//!
//! Noesis sizes an `Image` from its source's pixel dimensions, which it obtains
//! from our provider's `GetTextureInfo` during layout — **no GPU render pass
//! required**. So a `13x7` staged bitmap drives `ActualWidth = 13`,
//! `ActualHeight = 7`. The built-in negative control is a second `<Image>` whose
//! `Source` URI is never registered: it stays `0`. A no-op apply, a wrong `uri`,
//! or a wrong size all read back differently from `[13, 7]`, so the assertion is
//! bluff-resistant.
//!
//! The component is populated at spawn time (alongside `NoesisView`): Noesis
//! resolves a `BitmapImage` source once at scene build and does not retry, so the
//! bytes must be staged before the scene exists. The bridge's staging system
//! runs before the registry→provider sync to make a same-frame spawn land in
//! time.
//!
//! Font-free XAML (only sizes are asserted, no glyph rendering), so the scene
//! builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpKind, DpValue, ImageReadback, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisImageChanged,
    NoesisImaging, NoesisPlugin, NoesisView, XamlRegistry,
};

const EXIT_AT_FRAME: usize = 120;

const BMP_W: u32 = 13;
const BMP_H: u32 = 7;
const BMP_URI: &str = "dm-bitmap://logo";

// A 64x64 Grid with two Stretch="None", top-left-aligned Images. "Pic" is
// driven by the bridge; "Empty" references an unregistered URI (negative
// control). With Stretch="None" each Image measures to its source's pixel size,
// so ActualWidth/Height reflect the registered bitmap (or 0 when unresolvable).
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
    noesis_license_from_env();

    let dp_observed: Arc<Mutex<Vec<(Entity, String, String, DpValue)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let img_observed: Arc<Mutex<Vec<(Entity, String, ImageReadback)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

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
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(4)));
    app.add_plugins(NoesisPlugin::default());

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
                    // Populated at spawn: Noesis resolves the BitmapImage source
                    // once at scene build, so the bytes must be staged before
                    // then. The bridge stages ahead of the provider sync.
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
        move |mut frame: Local<usize>,
              mut dp_changes: MessageReader<NoesisDpChanged>,
              mut img_changes: MessageReader<NoesisImageChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

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

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

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

    // Latest bridge read-back for a watched name on our view.
    let latest_img = |name: &str| -> Option<ImageReadback> {
        img.iter()
            .rfind(|(e, n, _)| *e == view && n == name)
            .map(|(_, _, rb)| *rb)
    };
    // Latest DP value for a watched (name, property) on our view.
    let latest_dp = |name: &str, prop: &str| -> Option<DpValue> {
        dp.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // Primary observable: the bridge's own read-back reports the staged bitmap's
    // exact pixel size on the driven element.
    let pic = latest_img("Pic").expect("expected a NoesisImageChanged for Pic");
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
        latest_dp("Pic", "ActualWidth"),
        Some(DpValue::F32(BMP_W as f32)),
        "imaging: Pic.ActualWidth should resolve to the staged bitmap width {BMP_W}",
    );

    // Negative control: an Image whose Source URI is never registered stays 0.
    // Proves the size came from *our staged bytes*, not the container / a
    // stretch, and that the bridge touched only its target.
    assert_eq!(
        latest_dp("Empty", "ActualWidth"),
        Some(DpValue::F32(0.0)),
        "imaging: an Image with an unregistered Source must measure to 0",
    );
}

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

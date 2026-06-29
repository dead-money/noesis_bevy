//! Bevy-app-level integration test for the **write-only** per-view shapes
//! bridge ([`NoesisShapes`]), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! The bridge builds a Noesis `Shape` (`Rectangle`/`Ellipse`/`Line`) in Rust and
//! assigns it to a named container element. Like the geometry bridge it has no
//! read-back message of its own, so we observe its *actual effect* through a
//! [`NoesisDp`] watch on the container's `ActualWidth`/`ActualHeight`: a
//! size-to-content `Border` (Left/Top-aligned, no explicit size) adopts the
//! assigned shape's measured size, so building a 40×24 `Rectangle` and handing
//! it to the `Border` lays the `Border` out to exactly `40 × 24`.
//!
//! The negative control is a second, untouched `Border` ("Empty"): with no
//! assigned child it measures to `0`, so a missing apply / wrong-entity routing
//! / building-into-the-wrong-container regression reads `0` for "Host" and fails.
//!
//! Font-free XAML (only DP sizes are asserted, no glyph rendering), so the scene
//! builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin, NoesisShapes,
    NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

// Two size-to-content Borders. "Host" receives a built shape as its decorator
// Child; "Empty" stays untouched (the negative control). Both are Left/Top so
// they shrink to their content rather than stretching to the 200×120 Grid.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="120">
  <Border x:Name="Host" HorizontalAlignment="Left" VerticalAlignment="Top"/>
  <Border x:Name="Empty" HorizontalAlignment="Left" VerticalAlignment="Top"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Host", "ActualWidth", DpKind::F32)
        .watch("Host", "ActualHeight", DpKind::F32)
        .watch("Empty", "ActualWidth", DpKind::F32) // negative control
}

#[test]
fn shapes_bridge_sizes_its_container() {
    noesis_license_from_env();

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
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
                "shapes.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "shapes.xaml".to_string(),
                        size: UVec2::new(200, 120),
                        ..default()
                    },
                    // Write-only component starts empty (no-op); filled in after
                    // the scene exists so its one-shot apply isn't lost.
                    NoesisShapes::new(),
                    // The DP watcher polls every frame regardless of changes.
                    watcher(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<(&mut NoesisShapes, &mut NoesisDp)>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut shapes, _dp) in &mut q {
                    // Build a 40×24 Rectangle and hand it to "Host"; "Empty"
                    // stays bare.
                    *shapes = NoesisShapes::new().rectangle("Host", 40.0, 24.0);
                }
            }

            for ev in changes.read() {
                observed_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    // Latest value seen for a watched (name, property) on our view.
    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    assert_eq!(
        latest("Host", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "shapes: a 40-wide Rectangle assigned to the Border should size it to ActualWidth 40 \
         (default 0)",
    );
    assert_eq!(
        latest("Host", "ActualHeight"),
        Some(DpValue::F32(24.0)),
        "shapes: a 24-tall Rectangle assigned to the Border should size it to ActualHeight 24 \
         (default 0)",
    );
    // Negative control: the bridge must touch ONLY its target — a wrong-name /
    // "build into every container" regression would size Empty too.
    assert_eq!(
        latest("Empty", "ActualWidth"),
        Some(DpValue::F32(0.0)),
        "shapes: an untouched container must stay at ActualWidth 0",
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

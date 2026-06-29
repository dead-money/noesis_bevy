//! Bevy-app-level integration test for the **write-only** `NoesisTypography`
//! bridge, exercised end-to-end through the real `NoesisPlugin` pipeline
//! (headless, pipelined rendering on).
//!
//! `NoesisTypography` has no read-back message of its own (it only pushes font
//! state into the live view). To make the assertion bluff-*resistant* we observe
//! its actual effect through a `NoesisDp` watch on a scalar dependency property
//! the write provably changes — `TextElement.FontSize` is a readable `f32` DP —
//! and assert the exact value:
//!
//!   * **typography** → `FontSize` (`f32`): bridging `Title.FontSize = 30` reads
//!     back `30`, not the authored default `12`. A second `TextBlock` (`Other`)
//!     left un-bridged is the negative control: it must stay at its authored
//!     `12`, so a "restyle everything" / wrong-entity-routing regression fails.
//!
//! The bridge component starts empty (no-op) and is filled in *after* the scene
//! is built, because it applies only on Bevy change-detection — mutating it before
//! the view exists would drop the one-shot apply.
//!
//! Font-free assertions (only the `FontSize` DP is read, no glyph rendering), so
//! the scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin, NoesisTypography,
    NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

// Two TextBlocks with an explicit, identical authored FontSize of 12 — the
// un-applied default and negative-control baseline.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="64">
  <TextBlock x:Name="Title" FontSize="12" Text="Hello"/>
  <TextBlock x:Name="Other" FontSize="12" Text="World"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Title", "FontSize", DpKind::F32) // bridged target
        .watch("Other", "FontSize", DpKind::F32) // un-bridged negative control
}

#[test]
fn typography_bridge_applies_font_size() {
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
                "typography.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "typography.xaml".to_string(),
                        size: UVec2::new(200, 64),
                        ..default()
                    },
                    // Write-only component starts empty (no-op); filled in after
                    // the scene exists so its one-shot apply isn't lost.
                    NoesisTypography::new(),
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
              mut q: Query<(&mut NoesisTypography, &mut NoesisDp)>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut typo, mut dp) in &mut q {
                    // Restyle only Title; leave Other as the negative control.
                    *typo = NoesisTypography::new().font_size("Title", 30.0);
                    // Keep the watches alive (re-assign re-triggers change det).
                    *dp = watcher();
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
        latest("Title", "FontSize"),
        Some(DpValue::F32(30.0)),
        "typography: bridging Title.FontSize=30 should read back 30 (authored default 12)",
    );
    // Negative control: the bridge must touch ONLY its target — a "restyle
    // everything" or wrong-entity-routing regression would change Other too.
    assert_eq!(
        latest("Other", "FontSize"),
        Some(DpValue::F32(12.0)),
        "typography: an un-bridged TextBlock must keep its authored FontSize 12",
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

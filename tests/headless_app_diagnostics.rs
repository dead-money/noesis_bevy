//! Bevy-app-level test for the **app-level** diagnostics bridge, exercised
//! end-to-end through the real `NoesisPlugin` pipeline (headless, pipelined
//! rendering on).
//!
//! The bridge has no element target: it mirrors Noesis's process-global
//! allocator counters into the `NoesisDiagnostics` resource each frame. To make
//! the assertion bluff-*resistant* the resource's all-zero `Default` is the
//! negative control — a missing/broken refresh system (or a no-op plugin) leaves
//! every counter at `0`. We boot the app, build a font-free scene so Noesis
//! actually allocates a view + visual tree, pump frames, then assert the
//! mirrored counters are plausibly non-zero and internally consistent
//! (`accum >= allocated`, since `accum` is cumulative and `allocated` is live).
//!
//! We snapshot the resource at two points: once early (frame `EARLY_AT_FRAME`,
//! before the scene is guaranteed built) and once late (`SAMPLE_AT_FRAME`, well
//! after). The late sample must be non-zero; capturing both also proves the
//! refresh runs every frame rather than one-shot.
//!
//! Font-free XAML (a colored `Border` in a `Grid`, no glyphs) so the scene
//! builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{NoesisCamera, NoesisDiagnostics, NoesisPlugin, NoesisView, XamlRegistry};

const EARLY_AT_FRAME: usize = 3;
const SAMPLE_AT_FRAME: usize = 40;
const EXIT_AT_FRAME: usize = 50;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Panel" Background="#400000FF"/>
</Grid>"##;

#[test]
fn diagnostics_resource_mirrors_allocator_counters() {
    noesis_license_from_env();

    // (early, late) snapshots of the resource captured from inside the loop.
    let early: Arc<Mutex<Option<NoesisDiagnostics>>> = Arc::new(Mutex::new(None));
    let late: Arc<Mutex<Option<NoesisDiagnostics>>> = Arc::new(Mutex::new(None));

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

    app.add_systems(
        Startup,
        |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("diag.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            commands.spawn((
                Camera2d,
                NoesisCamera,
                NoesisView {
                    xaml_uri: "diag.xaml".to_string(),
                    size: UVec2::new(64, 32),
                    ..default()
                },
            ));
        },
    );

    let early_sys = Arc::clone(&early);
    let late_sys = Arc::clone(&late);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              diag: Res<NoesisDiagnostics>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            if *frame == EARLY_AT_FRAME {
                *early_sys.lock().unwrap() = Some(*diag);
            }
            if *frame == SAMPLE_AT_FRAME {
                *late_sys.lock().unwrap() = Some(*diag);
            }
            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let late = snapshot(&late, "late");
    let early = snapshot(&early, "early");
    eprintln!("--- NoesisDiagnostics early={early:?} late={late:?} ---");

    // The negative control is the all-zero Default: a no-op/broken refresh keeps
    // every counter at 0. A live engine with a built scene allocates plenty.
    assert!(
        late.allocated_memory > 0,
        "allocated_memory should be non-zero after a scene builds (default 0); got {}",
        late.allocated_memory,
    );
    assert!(
        late.allocations_count > 0,
        "allocations_count should be non-zero after a scene builds (default 0); got {}",
        late.allocations_count,
    );
    // `accum` is cumulative-ever, `allocated` is live; the live figure can never
    // exceed the cumulative total. Catches a transposed/garbage read.
    assert!(
        late.allocated_memory_accum >= late.allocated_memory,
        "accum ({}) must be >= live allocated ({})",
        late.allocated_memory_accum,
        late.allocated_memory,
    );
    // The early sample also being non-zero proves the refresh runs every frame,
    // not just once: init() alone already allocates before any scene exists.
    assert!(
        early.allocations_count > 0,
        "early allocations_count should be non-zero (engine init allocates); got {}",
        early.allocations_count,
    );
}

fn snapshot(slot: &Arc<Mutex<Option<NoesisDiagnostics>>>, which: &str) -> NoesisDiagnostics {
    slot.lock()
        .unwrap()
        .unwrap_or_else(|| panic!("{which} diagnostics snapshot captured"))
}

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

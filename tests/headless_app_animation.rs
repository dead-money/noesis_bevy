//! Bevy-app-level integration test for the **write-only** `NoesisAnimation`
//! bridge (`Animation::begin_on`), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! The bridge has no read-back message of its own, so — like the other
//! write-only element bridges — we observe its *actual effect* through a
//! `NoesisDp` watch on the scalar dependency properties the animations provably
//! change, and assert exact values. Three `Border`s let us cover every
//! sub-feature in one scene:
//!
//!   * **"Box"** (`Left`/`Top`, `Width=10`) is driven via the explicit-`From`
//!     builder `animate_from` and re-begun mid-run:
//!       - Phase A begins `Width 20 -> 50` over a short duration. Because the
//!         authored base is `10` but `From` pins the start to `20`, the bridge
//!         must (a) interpolate (we observe an intermediate `20 < w < 50`, which
//!         a bare DP-set could never produce) and (b) honor `From` — *no*
//!         observed value may fall in the open `(10, 20)` gap an ignored `From`
//!         (starting from base `10`) would climb through. `To=50` is distinct
//!         from both the authored `10` and the `From=20`, so a swapped from/to
//!         also fails (it would hold `20`, never reach `50`).
//!       - Phase B re-assigns the component (`Width 50 -> 25`). Re-begin is the
//!         update model: a fresh assignment must restart the clock
//!         (`SnapshotAndReplace`), replacing the held `50`. The final held value
//!         is `25` — if re-begin were a no-op the value would still read the
//!         phase-A `50`.
//!   * **"Tall"** (`Right`/`Bottom`, `Height=10`) is the second entry in phase
//!     A's map and uses the no-`From` builder `animate` to drive a *different
//!     property* (`Height -> 30`). Two distinct `(name, property)` entries in one
//!     component guard map-iteration bugs (only-first / only-last). With the
//!     default `HoldEnd` it holds `30`; never re-begun, it stays `30`.
//!   * **"Other"** (`Right`/`Top`, `Width=10`) is never named ⇒ the negative
//!     control. An "animate everything" / wrong-name-resolution regression would
//!     grow it; it must stay `ActualWidth = 10`.
//!
//! Each driven element re-lays-out so its `Actual{Width,Height}` tracks the
//! animated property. The app pumps real frames (the render-state clock is
//! wall-clock, not Bevy `Time`); each `~0.1 s` animation completes long before
//! the next phase, so the *latest* observed value is the held end. The
//! intermediate/`From`-gap assertions rely on the headless run-loop's ~4 ms
//! frame cadence sampling the in-flight animation many times (the existing
//! held-value assertion already depends on this cadence).
//!
//! The write-only component starts empty (no-op) and is filled in *after* the
//! scene is built, because it applies only on Bevy change-detection — mutating
//! it before the view exists would drop the one-shot begin.
//!
//! Font-free XAML (only DP values are asserted, no glyph rendering), so the
//! scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpKind, DpValue, NoesisAnimation, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin,
    NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const REBEGIN_AT_FRAME: usize = 55;
const EXIT_AT_FRAME: usize = 110;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="64">
  <Border x:Name="Box" Width="10" Height="10"
          HorizontalAlignment="Left" VerticalAlignment="Top"
          Background="#400000FF"/>
  <Border x:Name="Tall" Width="10" Height="10"
          HorizontalAlignment="Right" VerticalAlignment="Bottom"
          Background="#40FF0000"/>
  <Border x:Name="Other" Width="10" Height="10"
          HorizontalAlignment="Right" VerticalAlignment="Top"
          Background="#4000FF00"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Box", "ActualWidth", DpKind::F32) // animate_from + re-begin target
        .watch("Tall", "ActualHeight", DpKind::F32) // second map entry, distinct property
        .watch("Other", "ActualWidth", DpKind::F32) // negative control
}

#[test]
fn animation_bridge_drives_named_property() {
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
            reg.insert("anim.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "anim.xaml".to_string(),
                        size: UVec2::new(64, 64),
                        ..default()
                    },
                    // Write-only component starts empty (no-op); filled in after
                    // the scene exists so its one-shot begin isn't lost.
                    NoesisAnimation::new(),
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
              mut q: Query<(&mut NoesisAnimation, &mut NoesisDp)>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut anim, _dp) in &mut q {
                    // Phase A: two distinct (name, property) entries in one
                    // component (map iteration). Box uses an explicit From=20
                    // (distinct from the authored base 10 and the To 50); Tall
                    // animates a different property (Height) with no From.
                    *anim = NoesisAnimation::new()
                        .animate_from("Box", "Width", 20.0, 50.0, 0.1)
                        .animate("Tall", "Height", 30.0, 0.1);
                }
            }

            if *frame == REBEGIN_AT_FRAME {
                for (mut anim, _dp) in &mut q {
                    // Phase B: re-assign the component to re-begin Box's Width
                    // with a new To (25). A working re-begin restarts the clock
                    // and replaces the held phase-A 50.
                    *anim = NoesisAnimation::new().animate_from("Box", "Width", 50.0, 25.0, 0.1);
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
    // Every f32 observed for a watched (name, property) on our view, in order.
    let series = |name: &str, prop: &str| -> Vec<f32> {
        got.iter()
            .filter(|(e, n, p, _)| *e == view && n == name && p == prop)
            .filter_map(|(_, _, _, v)| match v {
                DpValue::F32(f) => Some(*f),
                _ => None,
            })
            .collect()
    };

    let box_w = series("Box", "ActualWidth");

    // Happy path / phase-A completion: Box's Width 20->50 reached and held the
    // To=50 before the re-begin. A swapped from/to would hold 20 and never hit 50.
    assert!(
        box_w.contains(&50.0),
        "animation: animate_from Width 20->50 should drive Box.ActualWidth to its To=50; \
         observed {box_w:?}",
    );

    // Real interpolation (not a bare DP set): an intermediate value strictly
    // between From=20 and To=50 must appear.
    assert!(
        box_w.iter().any(|w| *w > 20.0 && *w < 50.0),
        "animation: must interpolate (intermediate 20<w<50), not snap; observed {box_w:?}",
    );

    // Explicit From honored: starting from the pinned 20 (not the authored base
    // 10), no observed value may land in the (10, 20) gap an ignored From would
    // climb through. The authored 10 itself is excluded (== 10, not > 10).
    assert!(
        !box_w.iter().any(|w| *w > 10.0 && *w < 20.0),
        "animation: animate_from must start at From=20, never the authored base 10 \
         (no value in the open (10,20) gap); observed {box_w:?}",
    );

    // Re-begin / handoff: phase B restarted Box's Width with a new To=25,
    // replacing the held phase-A 50. A no-op re-begin would still read 50.
    assert_eq!(
        latest("Box", "ActualWidth"),
        Some(DpValue::F32(25.0)),
        "animation: re-assigning the component must re-begin (SnapshotAndReplace) and \
         drive Box.ActualWidth to the new To=25 (not the stale phase-A 50)",
    );

    // Map iteration / multi-property: the second entry animated a distinct
    // property (Height) on a distinct element to its To=30, held.
    assert_eq!(
        latest("Tall", "ActualHeight"),
        Some(DpValue::F32(30.0)),
        "animation: the second map entry (Tall.Height->30) must also apply and hold (default 10)",
    );

    // Negative control: the bridge must touch ONLY named targets.
    assert_eq!(
        latest("Other", "ActualWidth"),
        Some(DpValue::F32(10.0)),
        "animation: an undriven element must stay at its authored ActualWidth 10",
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

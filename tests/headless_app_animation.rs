//! Integration test for the `NoesisAnimation` bridge through the real `NoesisPlugin` pipeline
//! (headless, pipelined rendering on).
//!
//! No read-back message on this bridge; we observe via `NoesisDp` watches on the DPs the
//! animations drive. Three elements cover the sub-features:
//!
//! - **Box**: `animate_from` (From=20, base=10, To=50) then re-begun (To=25). Exercises
//!   interpolation, From honored (gap 10..20 stays empty), held end, and re-begin.
//! - **Tall**: `animate` (no From), Height->30. Second entry in the same component; guards
//!   map-iteration bugs (only-first / only-last).
//! - **Other**: untargeted. Negative control; must stay at ActualWidth=10.
//!
//! Animations run ~0.1 s; the ~4 ms frame cadence samples each in-flight animation many times,
//! enabling the intermediate-value assertion. Each completes before the next phase begins.
//!
//! `NoesisAnimation` starts empty and is filled after the scene exists: the begin is one-shot
//! and would be lost if the component were mutated before the view is live.
//!
//! Font-free XAML; only DP values are asserted.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
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
                    // Phase A: two distinct (name, property) entries (map-iteration test).
                    *anim = NoesisAnimation::new()
                        .animate_from("Box", "Width", 20.0, 50.0, 0.1)
                        .animate("Tall", "Height", 30.0, 0.1);
                }
            }

            if *frame == REBEGIN_AT_FRAME {
                for (mut anim, _dp) in &mut q {
                    // Phase B: re-assigning re-begins; replaces the held 50.
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

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };
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

    assert!(
        box_w.contains(&50.0),
        "animation: animate_from Width 20->50 should drive Box.ActualWidth to its To=50; \
         observed {box_w:?}",
    );

    assert!(
        box_w.iter().any(|w| *w > 20.0 && *w < 50.0),
        "animation: must interpolate (intermediate 20<w<50), not snap; observed {box_w:?}",
    );

    assert!(
        !box_w.iter().any(|w| *w > 10.0 && *w < 20.0),
        "animation: animate_from must start at From=20, never the authored base 10 \
         (no value in the open (10,20) gap); observed {box_w:?}",
    );

    assert_eq!(
        latest("Box", "ActualWidth"),
        Some(DpValue::F32(25.0)),
        "animation: re-assigning the component must re-begin (SnapshotAndReplace) and \
         drive Box.ActualWidth to the new To=25 (not the stale phase-A 50)",
    );

    assert_eq!(
        latest("Tall", "ActualHeight"),
        Some(DpValue::F32(30.0)),
        "animation: the second map entry (Tall.Height->30) must also apply and hold (default 10)",
    );

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

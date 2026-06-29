//! Bevy-app-level integration test for the **write-only** `NoesisAnimation`
//! bridge (`Animation::begin_on`), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! The bridge has no read-back message of its own, so — like the other
//! write-only element bridges — we observe its *actual effect* through a
//! `NoesisDp` watch on a scalar dependency property the animation provably
//! changes, and assert the exact value:
//!
//!   * Two `Border`s ("Box", "Other") are `Left`/`Top`-aligned at `Width=10`.
//!     Beginning a short `DoubleAnimation` that drives **only** "Box"'s `Width`
//!     to `50` advances off the view clock; with the default `HoldEnd` fill the
//!     property holds `50` after the duration, and the element re-lays-out so its
//!     `ActualWidth` tracks the animated `Width`.
//!   * Driving "Box" ⇒ `Box.ActualWidth = 50`, not the authored `10`. The
//!     default `10` is the built-in negative control: a missing apply /
//!     wrong-entity routing / inverted change-detection / never-advancing clock
//!     reads back `10` and fails.
//!   * "Other" is left undriven ⇒ stays `ActualWidth = 10`. An "animate
//!     everything" or wrong-name-resolution regression would grow it to `50`.
//!
//! The animation runs ~`0.1 s`; the app pumps real frames (the render-state
//! clock is wall-clock, not Bevy `Time`) from `SET_AT_FRAME` to `EXIT_AT_FRAME`,
//! well past the duration, so the *latest* observed value is the held `To`.
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
const EXIT_AT_FRAME: usize = 90;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="64">
  <Border x:Name="Box" Width="10" Height="10"
          HorizontalAlignment="Left" VerticalAlignment="Top"
          Background="#400000FF"/>
  <Border x:Name="Other" Width="10" Height="10"
          HorizontalAlignment="Right" VerticalAlignment="Top"
          Background="#4000FF00"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Box", "ActualWidth", DpKind::F32) // animated to 50
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
                    // Animate only Box's Width 10 -> 50 over a short duration;
                    // leave Other alone.
                    *anim = NoesisAnimation::new().animate("Box", "Width", 50.0, 0.1);
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
        latest("Box", "ActualWidth"),
        Some(DpValue::F32(50.0)),
        "animation: begin_on Width 10->50 should drive (and hold) Box.ActualWidth=50 \
         (default 10)",
    );
    // Negative control: the bridge must touch ONLY its target.
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

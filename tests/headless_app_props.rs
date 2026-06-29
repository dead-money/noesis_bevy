//! Bevy-app-level integration test for the **write-only** per-entity element
//! bridges — visibility, layout (margin), focus, and geometry — plus the
//! generic `NoesisDp` get/set bridge, exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! These four bridges have no read-back message of their own (they only push
//! state into the live view). To make the assertions bluff-*resistant* we
//! observe each one's *actual effect* through a `NoesisDp` watch on a scalar
//! dependency property the write provably changes, and assert the exact value —
//! the element's *default* value is the built-in negative control, so a missing
//! apply / wrong-entity routing / inverted change-detection reads back the
//! default and fails:
//!
//!   * **visibility** → `IsVisible` (`bool`): `hide` ⇒ `false`, not the default
//!     `true`. (Noesis's `Visibility` enum isn't reachable through `get_i32` /
//!     `get_string`, but the derived `IsVisible` bool reflects it directly.)
//!   * **focus** → `IsFocused` (`bool`): focusing the `TextBox` ⇒ `true`, not
//!     the default `false`.
//!   * **layout** → `ActualWidth` (`f32`): a stretchy 64-wide element with
//!     `Margin = [8,0,16,0]` lays out to `64 - 8 - 16 = 40`, not `64`.
//!   * **geometry** → `ActualWidth` (`f32`): a `Path` with no `Data` measures
//!     to `0`; assigning a polyline gives it real bounds (> 0).
//!   * **dp** (set side-effect) → `Input.ActualWidth` (`f32`): writing
//!     `Input.Width = 40` drives a re-layout to `ActualWidth = 40`, not the
//!     authored `20`. (A DP read can't observe its *own* write — the bridge
//!     eagerly snapshots self-writes to avoid echoes — so we watch a derived
//!     property the write provably changes.)
//!
//! The write-only components start empty (no-op) and are filled in *after* the
//! scene is built, because each applies only on Bevy change-detection — mutating
//! them before the view exists would drop the one-shot apply.
//!
//! Font-free XAML (only DP values are asserted, no glyph rendering), so the scene
//! builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisFocus, NoesisGeometry,
    NoesisLayout, NoesisPlugin, NoesisView, NoesisVisibility, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Panel" Background="#400000FF"/>
  <TextBox x:Name="Input" Width="20" Height="10"/>
  <TextBox x:Name="Other" Width="20" Height="10"/>
  <Border x:Name="Float" Height="10" Background="#4000FF00"/>
  <Path x:Name="Trace" Stroke="Red" StrokeThickness="1"
        HorizontalAlignment="Left" VerticalAlignment="Top" Stretch="None"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Panel", "IsVisible", DpKind::Bool) // visibility
        .watch("Input", "IsFocused", DpKind::Bool) // focus
        .watch("Other", "IsFocused", DpKind::Bool) // focus negative control
        .watch("Input", "ActualWidth", DpKind::F32) // dp set side-effect
        .watch("Float", "ActualWidth", DpKind::F32) // layout
        .watch("Trace", "ActualWidth", DpKind::F32) // geometry
}

#[test]
fn write_only_bridges_apply_their_effect() {
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
            reg.insert("props.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "props.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    // Write-only components start empty (no-op); filled in after
                    // the scene exists so their one-shot apply isn't lost.
                    NoesisVisibility::new(),
                    NoesisFocus::new(),
                    NoesisLayout::new(),
                    NoesisGeometry::new(),
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
              mut q: Query<(
            &mut NoesisVisibility,
            &mut NoesisFocus,
            &mut NoesisLayout,
            &mut NoesisGeometry,
            &mut NoesisDp,
        )>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut vis, mut focus, mut layout, mut geom, mut dp) in &mut q {
                    *vis = NoesisVisibility::new().hide("Panel");
                    *focus = NoesisFocus::new().focus("Input");
                    *layout = NoesisLayout::new().margin("Float", [8.0, 0.0, 16.0, 0.0]);
                    *geom = NoesisGeometry::new().path("Trace", vec![[0.0, 0.0], [40.0, 20.0]]);
                    // Keep the watches; add a DP write whose layout side-effect
                    // (Input.ActualWidth: 20 -> 40) is observable.
                    *dp = watcher().set_f32("Input", "Width", 40.0);
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
        latest("Panel", "IsVisible"),
        Some(DpValue::Bool(false)),
        "visibility: hiding the Border should set IsVisible=false (default true)",
    );
    assert_eq!(
        latest("Input", "IsFocused"),
        Some(DpValue::Bool(true)),
        "focus: focusing the TextBox should set IsFocused=true (default false)",
    );
    // Negative control: the focus bridge must touch ONLY its target — a
    // "focus everything" or auto-focus regression would flip Other too.
    assert_eq!(
        latest("Other", "IsFocused"),
        Some(DpValue::Bool(false)),
        "focus: an un-targeted TextBox must stay unfocused",
    );
    assert_eq!(
        latest("Float", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "layout: Margin [8,0,16,0] on a 64-wide stretchy element => ActualWidth 40 \
         (default 64)",
    );
    // The Path is Left/Top-aligned, Stretch=None, so its empty default measures
    // to 0 and the [0,0]->[40,20] polyline gives an x-extent of ~40 (+ stroke).
    // A no-op apply reads 0; a stretched cell would read 64 — both fail this.
    match latest("Trace", "ActualWidth") {
        Some(DpValue::F32(w)) => assert!(
            (38.0..=43.0).contains(&w),
            "geometry: a [0,0]->[40,20] polyline should give ActualWidth ~40 \
             (default 0, stretched 64); got {w}",
        ),
        other => panic!("geometry: expected an ActualWidth F32 read-back, got {other:?}"),
    }
    assert_eq!(
        latest("Input", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "dp: setting Input.Width=40 should re-layout to ActualWidth 40 (authored 20)",
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

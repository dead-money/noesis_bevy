//! Tests write-only bridges (visibility, layout, focus, geometry) and `NoesisDp`
//! set through the real `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! These bridges have no read-back message. Each is verified through a `NoesisDp`
//! watch on a derived property the write changes; the element default is the
//! negative control, so a missing apply reads back the default and fails.
//!
//! * visibility: `Panel.IsVisible` (`false` after hide, default `true`);
//!   `Visibility` enum isn't reachable via `get_i32`/`get_string`,
//!   but the derived `IsVisible` bool reflects it.
//! * focus: `Input.IsFocused` (`true` after focus, default `false`);
//!   `Other.IsFocused` stays `false` (proves only the target is focused).
//! * layout: `Float.ActualWidth` (40 after Margin=[8,0,16,0] on 64-wide, default 64).
//! * geometry: `Trace.ActualWidth` (~40 after [0,0]->[40,20] polyline, default 0).
//! * dp set: `Input.ActualWidth` (40 after Width=40, default 20);
//!   a DP read can't observe its own write (bridge snapshots self-writes),
//!   so we watch the derived `ActualWidth` the re-layout changes.
//!
//! Write-only components are spawned empty and mutated at `SET_AT_FRAME` so
//! change-detection fires after the scene is built.

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
                    // keep watches; add a width write whose re-layout (ActualWidth 20->40) is observable
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
    // Negative control: focus bridge must touch only its target; "focus everything" or auto-focus regressions would flip Other.
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
    // Left/Top-aligned, Stretch=None: empty default measures 0; [0,0]->[40,20] gives ~40 (+ stroke).
    // A no-op apply reads 0; a stretched cell reads 64. Both alternatives fail.
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

//! Tests write-only bridges (visibility, layout, focus, geometry) and `NoesisDp`
//! set through the Noesis bridge pipeline on the headless harness.
//!
//! These bridges have no read-back message. Each is verified through a `NoesisDp`
//! watch on a derived property the write changes; the element default is the
//! negative control, so a missing apply reads back the default and fails.
//!
//! * visibility: `Panel.IsVisible` (`false` after hide, default `true`). The
//!   `Visibility` enum can't be watched directly; `IsVisible` reflects it.
//! * focus: `Input.IsFocused` (`true` after focus, default `false`);
//!   `Other.IsFocused` stays `false` (proves only the target is focused).
//! * layout: `Float.ActualWidth` (40 after Margin=[8,0,16,0] on 64-wide, default 64).
//! * geometry: `Trace.ActualWidth` (~40 after [0,0]->[40,20] polyline, default 0).
//! * dp set: `Input.ActualWidth` (40 after Width=40, default 20). A watch doesn't
//!   report the bridge's own writes, so the test watches the re-layout instead.
//!
//! Write-only components are spawned empty and filled at `SET_AT_FRAME`, after the
//! scene is built.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisFocus, NoesisGeometry,
    NoesisLayout, NoesisView, NoesisVisibility, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const SET_AT_FRAME: usize = 10;

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
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

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
                    NoesisVisibility::new(),
                    NoesisFocus::new(),
                    NoesisLayout::new(),
                    NoesisGeometry::new(),
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
              mut changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut vis, mut focus, mut layout, mut geom, mut dp) in &mut q {
                    *vis = NoesisVisibility::new().hide("Panel");
                    *focus = NoesisFocus::new().focus("Input");
                    *layout = NoesisLayout::new().margin("Float", [8.0, 0.0, 16.0, 0.0]);
                    *geom = NoesisGeometry::new().path("Trace", vec![[0.0, 0.0], [40.0, 20.0]]);
                    // Keep the watches and add the Width write.
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
        },
    );

    let latest = |got: &Observed, view: Entity, name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // Stroke makes Trace's width inexact. A no-op reads 0 and a stretched cell 64.
    let trace_ok =
        |v: &Option<DpValue>| matches!(v, Some(DpValue::F32(w)) if (38.0..=43.0).contains(w));

    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        latest(&got, view, "Panel", "IsVisible") == Some(DpValue::Bool(false))
            && latest(&got, view, "Input", "IsFocused") == Some(DpValue::Bool(true))
            && latest(&got, view, "Other", "IsFocused") == Some(DpValue::Bool(false))
            && latest(&got, view, "Float", "ActualWidth") == Some(DpValue::F32(40.0))
            && trace_ok(&latest(&got, view, "Trace", "ActualWidth"))
            && latest(&got, view, "Input", "ActualWidth") == Some(DpValue::F32(40.0))
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    assert!(
        converged,
        "write-only bridges never all applied their effect within 240 frames; \
         observed {got:?}",
    );

    assert_eq!(
        latest(&got, view, "Panel", "IsVisible"),
        Some(DpValue::Bool(false)),
        "visibility: hiding the Border should set IsVisible=false (default true)",
    );
    assert_eq!(
        latest(&got, view, "Input", "IsFocused"),
        Some(DpValue::Bool(true)),
        "focus: focusing the TextBox should set IsFocused=true (default false)",
    );
    assert_eq!(
        latest(&got, view, "Other", "IsFocused"),
        Some(DpValue::Bool(false)),
        "focus: an un-targeted TextBox must stay unfocused",
    );
    assert_eq!(
        latest(&got, view, "Float", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "layout: Margin [8,0,16,0] on a 64-wide stretchy element => ActualWidth 40 \
         (default 64)",
    );
    match latest(&got, view, "Trace", "ActualWidth") {
        Some(DpValue::F32(w)) => assert!(
            (38.0..=43.0).contains(&w),
            "geometry: a [0,0]->[40,20] polyline should give ActualWidth ~40 \
             (default 0, stretched 64); got {w}",
        ),
        other => panic!("geometry: expected an ActualWidth F32 read-back, got {other:?}"),
    }
    assert_eq!(
        latest(&got, view, "Input", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "dp: setting Input.Width=40 should re-layout to ActualWidth 40 (authored 20)",
    );
}

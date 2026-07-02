//! Integration test for the SVG bridge: parse path data, read back measured
//! bounds, and verify element sizing via `ActualWidth`.
//!
//! `NoesisSvg` is populated at frame `SET_AT_FRAME` rather than at spawn so the
//! view exists when change-detection fires; an earlier mutation drops the apply.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisSvg, NoesisSvgChanged,
    NoesisView, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const SET_AT_FRAME: usize = 10;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="64">
  <Border x:Name="Icon" Background="#400000FF"
          HorizontalAlignment="Left" VerticalAlignment="Top"/>
</Grid>"##;

#[test]
fn svg_bridge_parses_and_sizes_element() {
    let svg_msgs: Arc<Mutex<Vec<NoesisSvgChanged>>> = Arc::new(Mutex::new(Vec::new()));
    let dp_msgs: Arc<Mutex<Vec<(Entity, String, String, DpValue)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("svg.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "svg.xaml".to_string(),
                        size: UVec2::new(64, 64),
                        ..default()
                    },
                    // Starts empty (no-op); filled after the scene exists so its
                    // one-shot apply isn't lost.
                    NoesisSvg::new(),
                    NoesisDp::new().watch("Icon", "ActualWidth", DpKind::F32),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let svg_sys = Arc::clone(&svg_msgs);
    let dp_sys = Arc::clone(&dp_msgs);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<&mut NoesisSvg>,
              mut svg_changes: MessageReader<NoesisSvgChanged>,
              mut dp_changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for mut svg in &mut q {
                    *svg = NoesisSvg::new()
                        .path("Icon", "M0 0 L40 0 L40 20 Z")
                        // Negative control: not in the live tree -> no message.
                        .path("Ghost", "M0 0 L99 0 L99 99 Z");
                }
            }

            for ev in svg_changes.read() {
                svg_sys.lock().unwrap().push(ev.clone());
            }
            for ev in dp_changes.read() {
                dp_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }
        },
    );

    // Stop once Icon has reported its measured bounds and re-laid out to width 40,
    // rather than padding a fixed frame count. The SVG apply still fires at SET_AT_FRAME.
    let pred_view = Arc::clone(&view_entity);
    let pred_svg = Arc::clone(&svg_msgs);
    let pred_dp = Arc::clone(&dp_msgs);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let icon_measured = pred_svg
            .lock()
            .unwrap()
            .iter()
            .any(|e| e.view == view && e.name == "Icon");
        let width_ok = pred_dp
            .lock()
            .unwrap()
            .iter()
            .rfind(|(e, n, p, _)| *e == view && n == "Icon" && p == "ActualWidth")
            .map(|(_, _, _, v)| v.clone())
            == Some(DpValue::F32(40.0));
        icon_measured && width_ok
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let svgs = svg_msgs.lock().unwrap().clone();
    let dps = dp_msgs.lock().unwrap().clone();

    eprintln!("--- observed NoesisSvgChanged ---");
    for ev in &svgs {
        eprintln!("  {:?} {} = {:?}", ev.view, ev.name, ev.bounds);
    }

    assert!(
        converged,
        "svg never converged within 240 frames; svgs {svgs:?}, dps {dps:?}",
    );
    assert!(
        !svgs.iter().any(|e| e.name == "Ghost"),
        "svg: a source routed to an absent x:Name must emit nothing",
    );

    let icon = svgs
        .iter()
        .rfind(|e| e.view == view && e.name == "Icon")
        .expect("svg: expected a NoesisSvgChanged for Icon");
    let [x, y, w, h] = icon.bounds;
    assert!(
        (x - 0.0).abs() < 0.01
            && (y - 0.0).abs() < 0.01
            && (w - 40.0).abs() < 0.01
            && (h - 20.0).abs() < 0.01,
        "svg: 'M0 0 L40 0 L40 20 Z' should measure to [0,0,40,20]; got {:?}",
        icon.bounds,
    );

    // Border has no explicit Width and is Left-aligned, so without the SVG apply ActualWidth is 0.
    let latest_aw = dps
        .iter()
        .rfind(|(e, n, p, _)| *e == view && n == "Icon" && p == "ActualWidth")
        .map(|(_, _, _, v)| v.clone());
    assert_eq!(
        latest_aw,
        Some(DpValue::F32(40.0)),
        "svg: sizing Icon to the SVG width should re-layout to ActualWidth 40 (default 0)",
    );
}

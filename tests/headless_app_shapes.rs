//! Integration test for [`NoesisShapes`] through the real `NoesisPlugin` pipeline (headless).
//!
//! Shapes have no read-back message, so the effect is observed via a [`NoesisDp`] watch on
//! `ActualWidth`/`ActualHeight`: a size-to-content `Border` adopts the shape's measured size.
//! A second untouched `Border` ("Empty") is the negative control for wrong-target routing.
//! XAML is font-free.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisShapes, NoesisView,
    XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const SET_AT_FRAME: usize = 10;

// Left/Top alignment causes each Border to shrink to content; explicit size would swallow the shape measurement.
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
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

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
                    // Filled at SET_AT_FRAME so the first apply fires after the scene exists.
                    NoesisShapes::new(),
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
              mut changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut shapes, _dp) in &mut q {
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
        },
    );

    // Stop once the shape has resized its host and the negative control read back,
    // rather than padding a fixed frame count. The stimulus still fires at SET_AT_FRAME.
    let pred_view = Arc::clone(&view_entity);
    let pred_observed = Arc::clone(&observed);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        let latest = |name: &str, prop: &str| -> Option<DpValue> {
            got.iter()
                .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
                .map(|(_, _, _, v)| v.clone())
        };
        latest("Host", "ActualWidth") == Some(DpValue::F32(40.0))
            && latest("Host", "ActualHeight") == Some(DpValue::F32(24.0))
            && latest("Empty", "ActualWidth") == Some(DpValue::F32(0.0))
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    assert!(
        converged,
        "shapes never converged within 240 frames; observed {got:?}",
    );

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
    // Negative control: the bridge must touch only its target. A wrong-name or
    // build-into-every-container regression would size Empty too.
    assert_eq!(
        latest("Empty", "ActualWidth"),
        Some(DpValue::F32(0.0)),
        "shapes: an untouched container must stay at ActualWidth 0",
    );
}

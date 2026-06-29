//! Integration test for [`NoesisShapes`] through the real `NoesisPlugin` pipeline (headless).
//!
//! Shapes have no read-back message, so the effect is observed via a [`NoesisDp`] watch on
//! `ActualWidth`/`ActualHeight`: a size-to-content `Border` adopts the shape's measured size.
//! A second untouched `Border` ("Empty") is the negative control for wrong-target routing.
//! XAML is font-free.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin, NoesisShapes,
    NoesisView, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

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
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

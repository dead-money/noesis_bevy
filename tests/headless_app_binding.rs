//! Integration test for [`NoesisBinding`]: value-converter and multi-binding
//! bridges, run through the real `NoesisPlugin` pipeline (headless).
//!
//! Sources are sibling elements resolved by `x:Name`; no `DataContext` needed.
//! Assertions read converted target values via [`NoesisDp`] string watch:
//!
//!   * `Upper.Text` = `"HELLO"` (Source.Text `"hello"` uppercased by Rust converter)
//!   * `Full.Text` = `"Ada Lovelace"` (First + Last joined by Rust multi-converter)
//!
//! Font-free XAML; no glyph rendering involved.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    ConvertArg, Converted, DpKind, DpValue, NoesisBinding, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisPlugin, NoesisView, SourceSpec, XamlRegistry,
};

const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="120">
  <StackPanel>
    <TextBox   x:Name="Source" Text="hello"/>
    <TextBox   x:Name="First"  Text="Ada"/>
    <TextBox   x:Name="Last"   Text="Lovelace"/>
    <TextBlock x:Name="Upper"/>
    <TextBlock x:Name="Full"/>
  </StackPanel>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

#[test]
fn binding_bridge_drives_targets_through_rust_converters() {
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
                "binding.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "binding.xaml".to_string(),
                        size: UVec2::new(200, 120),
                        ..default()
                    },
                    NoesisBinding::new()
                        .converted(
                            "Upper",
                            "Text",
                            SourceSpec::element("Source", "Text"),
                            |v: &ConvertArg, _p: &ConvertArg| {
                                Some(Converted::String(v.as_str()?.to_uppercase()))
                            },
                        )
                        .multi(
                            "Full",
                            "Text",
                            [
                                SourceSpec::element("First", "Text"),
                                SourceSpec::element("Last", "Text"),
                            ],
                            |vals: &[ConvertArg], _p: &ConvertArg| {
                                let a = vals.first().and_then(ConvertArg::as_str)?;
                                let b = vals.get(1).and_then(ConvertArg::as_str)?;
                                Some(Converted::String(format!("{a} {b}")))
                            },
                        ),
                    NoesisDp::new().watch("Upper", "Text", DpKind::Str).watch(
                        "Full",
                        "Text",
                        DpKind::Str,
                    ),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
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
        latest("Upper", "Text"),
        Some(DpValue::Str("HELLO".to_string())),
        "converted binding: Upper.Text should be Source.Text upper-cased \
         (identity would read \"hello\", no binding reads empty)",
    );
    assert_eq!(
        latest("Full", "Text"),
        Some(DpValue::Str("Ada Lovelace".to_string())),
        "multi binding: Full.Text should combine First+Last through the Rust \
         multi-converter",
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

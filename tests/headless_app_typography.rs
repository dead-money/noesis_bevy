//! Headless integration test for `NoesisTypography`, exercising all five properties
//! (`FontSize`, `FontFamily`, `FontWeight`, `FontStyle`, `FontStretch`) against both read paths.
//!
//! FontWeight/Style/Stretch are Noesis enum DPs that do not round-trip through
//! `NoesisDp::get_i32`; the typed `NoesisTypography` getters are the only read path.
//!
//! `NoesisTypography` spawns empty and is populated after the scene builds because
//! Bevy change-detection drops a write made before the view exists.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    DpKind, DpValue, FontStretch, FontStyle, FontWeight, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisPlugin, NoesisTypography, NoesisTypographyChanged, NoesisView, TypographyField,
    TypographyValue, XamlRegistry,
};

const SET_AT_FRAME: usize = 10;
const EXIT_AT_FRAME: usize = 60;

// Both elements authored with identical defaults so every assertion has a known
// "before" value. Other is left un-bridged as the negative control.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="64">
  <TextBlock x:Name="Title" FontSize="12" FontWeight="Normal" FontStyle="Normal" FontStretch="Normal" Text="Hello"/>
  <TextBlock x:Name="Other" FontSize="12" FontWeight="Normal" FontStyle="Normal" FontStretch="Normal" Text="World"/>
</Grid>"##;

type ObservedDp = Vec<(Entity, String, String, DpValue)>;
type ObservedTypo = Vec<(Entity, String, TypographyValue)>;

fn dp_watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Title", "FontSize", DpKind::F32)
        .watch("Other", "FontSize", DpKind::F32)
}

// Enum DPs have no i32 read path; typed watch required for FontWeight/Style/Stretch.
fn typo_watcher() -> NoesisTypography {
    NoesisTypography::new()
        .watch("Title", TypographyField::FontFamily)
        .watch("Title", TypographyField::FontWeight)
        .watch("Title", TypographyField::FontStyle)
        .watch("Title", TypographyField::FontStretch)
        .watch("Other", TypographyField::FontWeight)
}

#[test]
fn typography_bridge_applies_all_font_properties() {
    noesis_license_from_env();

    let observed_dp: Arc<Mutex<ObservedDp>> = Arc::new(Mutex::new(Vec::new()));
    let observed_typo: Arc<Mutex<ObservedTypo>> = Arc::new(Mutex::new(Vec::new()));
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
                "typography.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "typography.xaml".to_string(),
                        size: UVec2::new(200, 64),
                        ..default()
                    },
                    // Empty on spawn; populated after scene build so the one-shot apply isn't lost.
                    NoesisTypography::new(),
                    NoesisDp::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_dp_sys = Arc::clone(&observed_dp);
    let observed_typo_sys = Arc::clone(&observed_typo);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut q: Query<(&mut NoesisTypography, &mut NoesisDp)>,
              mut dp_changes: MessageReader<NoesisDpChanged>,
              mut typo_changes: MessageReader<NoesisTypographyChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut typo, mut dp) in &mut q {
                    // Restyle only Title; leave Other as the negative control.
                    *typo = NoesisTypography::new()
                        .font_size("Title", 30.0)
                        .font_family("Title", "Arial")
                        .font_weight("Title", FontWeight::Bold)
                        .font_style("Title", FontStyle::Italic)
                        .font_stretch("Title", FontStretch::Condensed);
                    // Re-assign triggers change detection, re-activating the poll list.
                    typo.watch = typo_watcher().watch;
                    *dp = dp_watcher();
                }
            }

            for ev in dp_changes.read() {
                observed_dp_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }
            for ev in typo_changes.read() {
                observed_typo_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
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
    let got_dp = observed_dp.lock().unwrap().clone();
    let got_typo = observed_typo.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got_dp {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }
    eprintln!("--- observed NoesisTypographyChanged ---");
    for (e, name, value) in &got_typo {
        eprintln!("  {e:?} {name} = {value:?}");
    }

    let latest_dp = |name: &str, prop: &str| -> Option<DpValue> {
        got_dp
            .iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };
    let latest_typo = |name: &str, want: fn(&TypographyValue) -> bool| -> Option<TypographyValue> {
        got_typo
            .iter()
            .rfind(|(e, n, v)| *e == view && n == name && want(v))
            .map(|(_, _, v)| v.clone())
    };

    assert_eq!(
        latest_dp("Title", "FontSize"),
        Some(DpValue::F32(30.0)),
        "FontSize: bridging Title.FontSize=30 should read back 30 (authored default 12)",
    );

    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontFamily(_))),
        Some(TypographyValue::FontFamily(Some("Arial".to_string()))),
        "FontFamily: bridging Title to \"Arial\" should read back its source",
    );

    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontWeight(_))),
        Some(TypographyValue::FontWeight(FontWeight::Bold)),
        "FontWeight: bridging Title to Bold(700) should read back Bold (authored Normal/400)",
    );

    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontStyle(_))),
        Some(TypographyValue::FontStyle(FontStyle::Italic)),
        "FontStyle: bridging Title to Italic should read back Italic (authored Normal)",
    );

    assert_eq!(
        latest_typo("Title", |v| matches!(v, TypographyValue::FontStretch(_))),
        Some(TypographyValue::FontStretch(FontStretch::Condensed)),
        "FontStretch: bridging Title to Condensed should read back Condensed (authored Normal)",
    );

    assert_eq!(
        latest_dp("Other", "FontSize"),
        Some(DpValue::F32(12.0)),
        "negative control: un-bridged Other must keep authored FontSize 12",
    );
    assert_eq!(
        latest_typo("Other", |v| matches!(v, TypographyValue::FontWeight(_))),
        Some(TypographyValue::FontWeight(FontWeight::Normal)),
        "negative control: un-bridged Other must keep authored FontWeight Normal(400)",
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

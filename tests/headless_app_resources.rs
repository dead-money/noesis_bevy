//! Bevy-app-level integration test for the **app-level application-resources
//! bridge** ([`NoesisResources`]), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on).
//!
//! The bridge registers Rust-built resources into the process-global
//! application resources so XAML `{StaticResource Key}` references resolve them.
//! To make the assertion bluff-resistant we observe the resolved value's
//! *actual effect* through a [`NoesisDp`] watch on a derived scalar the resource
//! provably changes, and assert the exact value against a negative control:
//!
//!   * A `<Border x:Name="Themed">` sets `Width="{StaticResource PanelWidth}"`
//!     where `PanelWidth` is a registered `Single`/`f32` of `40` (Noesis's
//!     `Border.Width` is a `Single`). Left-aligned in a
//!     64-wide grid, it lays out to `ActualWidth = 40` — *not* the negative
//!     control's authored `20`. A missing install / unresolved `StaticResource`
//!     would leave the Border with no explicit width (Grid-stretched to 64, or
//!     `NaN`/auto), so `40` is positive proof the resource resolved.
//!   * A sibling `<Border x:Name="Plain" Width="20">` carries no `StaticResource`
//!     — its `ActualWidth` stays `20`, proving the resource only feeds the
//!     element that references it.
//!
//! The `Themed` border also paints `Background="{StaticResource AccentBrush}"`
//! (a registered `SolidColorBrush`); a brush has no scalar DP to read back, so we
//! prove the brush *registered* through the bridge's [`NoesisResourcesInstalled`]
//! read-back (its `present` list is confirmed against the live
//! `GUI::GetApplicationResources`), and the value assertion above proves the
//! `{StaticResource}` resolution mechanism the brush rides on.
//!
//! Font-free XAML (only layout-derived DP values are asserted, no glyphs), so the
//! scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisPlugin, NoesisResources,
    NoesisResourcesInstalled, NoesisView, XamlRegistry,
};

const EXIT_AT_FRAME: usize = 60;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Themed"
          Background="{StaticResource AccentBrush}"
          Width="{StaticResource PanelWidth}" Height="10"
          HorizontalAlignment="Left" VerticalAlignment="Top"/>
  <Border x:Name="Plain" Width="20" Height="10"
          HorizontalAlignment="Left" VerticalAlignment="Top"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Themed", "ActualWidth", DpKind::F32) // resource-driven width
        .watch("Plain", "ActualWidth", DpKind::F32) // negative control
}

#[test]
fn app_resources_resolve_static_resource() {
    noesis_license_from_env();

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let installed: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
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

    // Register the application resources up front. The bridge installs them in
    // the `Sync` phase, before the scene builds in `Ensure`, so the scene's
    // `{StaticResource}` references resolve at parse time.
    app.insert_resource(
        NoesisResources::new()
            .solid("AccentBrush", [1.0, 0.0, 0.0, 1.0])
            .value("PanelWidth", DpValue::F32(40.0)),
    );

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("res.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "res.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    watcher(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    let installed_sys = Arc::clone(&installed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut changes: MessageReader<NoesisDpChanged>,
              mut installs: MessageReader<NoesisResourcesInstalled>,
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
            for ev in installs.read() {
                installed_sys.lock().unwrap().push(ev.present.clone());
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    let installs = installed.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }
    eprintln!("--- observed NoesisResourcesInstalled ---");
    for present in &installs {
        eprintln!("  present = {present:?}");
    }

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // The bridge installed once and confirmed both keys present in the live
    // application resources — proves the brush + value registered (the brush has
    // no scalar DP to read back, so this is its proof of registration).
    let present = installs.last().expect("a NoesisResourcesInstalled message");
    assert!(
        present.contains(&"AccentBrush".to_string()),
        "the SolidColorBrush resource should be installed + confirmed; got {present:?}",
    );
    assert!(
        present.contains(&"PanelWidth".to_string()),
        "the value resource should be installed + confirmed; got {present:?}",
    );

    // The StaticResource value drove Themed's Width to 40 (default/unset would
    // Grid-stretch to 64 or read as auto — neither is 40).
    assert_eq!(
        latest("Themed", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "resources: Width={{StaticResource PanelWidth}} (40) should give ActualWidth 40",
    );
    // Negative control: the sibling without the StaticResource keeps its authored
    // width, proving the resource feeds only the element that references it.
    assert_eq!(
        latest("Plain", "ActualWidth"),
        Some(DpValue::F32(20.0)),
        "resources: an element without the StaticResource keeps its authored width",
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

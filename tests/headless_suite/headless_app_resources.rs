//! Integration test for [`NoesisResources`]: registers app-level resources through
//! `NoesisPlugin` (headless) and verifies `{StaticResource}` references resolve.
//!
//! Asserts `Themed.ActualWidth == 40` (set by `{StaticResource PanelWidth}`; unset
//! would Grid-stretch to 64 or auto), `Plain.ActualWidth == 20` (negative control,
//! no resource reference), and that [`NoesisResourcesInstalled`] confirms both keys
//! present in the live application resources.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisResources,
    NoesisResourcesInstalled, NoesisView, XamlRegistry,
};

use crate::common::{headless_app, run_until};

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
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let installed: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    // Registered before the scene builds: bridge installs in Sync, scene builds in Ensure,
    // so {StaticResource} references resolve at parse time.
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
        move |mut changes: MessageReader<NoesisDpChanged>,
              mut installs: MessageReader<NoesisResourcesInstalled>| {
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
        },
    );

    // Stop as soon as both resource keys are confirmed installed and both watched
    // widths have converged, rather than padding a fixed frame count.
    let pred_view = Arc::clone(&view_entity);
    let pred_observed = Arc::clone(&observed);
    let pred_installed = Arc::clone(&installed);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let present_ok = pred_installed.lock().unwrap().last().is_some_and(|p| {
            p.contains(&"AccentBrush".to_string()) && p.contains(&"PanelWidth".to_string())
        });
        let got = pred_observed.lock().unwrap();
        let latest = |name: &str, prop: &str| -> Option<DpValue> {
            got.iter()
                .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
                .map(|(_, _, _, v)| v.clone())
        };
        present_ok
            && latest("Themed", "ActualWidth") == Some(DpValue::F32(40.0))
            && latest("Plain", "ActualWidth") == Some(DpValue::F32(20.0))
    });

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

    assert!(
        converged,
        "resources never converged within 240 frames; observed {got:?}, installs {installs:?}",
    );

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // AccentBrush has no scalar DP to read back; NoesisResourcesInstalled is the
    // only proof it registered.
    let present = installs.last().expect("a NoesisResourcesInstalled message");
    assert!(
        present.contains(&"AccentBrush".to_string()),
        "the SolidColorBrush resource should be installed + confirmed; got {present:?}",
    );
    assert!(
        present.contains(&"PanelWidth".to_string()),
        "the value resource should be installed + confirmed; got {present:?}",
    );

    // Unset would Grid-stretch to 64 or auto; 40 proves the resource resolved.
    assert_eq!(
        latest("Themed", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "resources: Width={{StaticResource PanelWidth}} (40) should give ActualWidth 40",
    );
    // Negative control: no StaticResource, so authored width unchanged.
    assert_eq!(
        latest("Plain", "ActualWidth"),
        Some(DpValue::F32(20.0)),
        "resources: an element without the StaticResource keeps its authored width",
    );
}

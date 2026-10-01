//! The default theme (`NoesisView::application_resources` URI chain) and the
//! [`NoesisResources`] code-built bridge feed one merged application-resources
//! dictionary instead of clobbering each other.
//!
//! The theme is enabled and a code-built `PanelWidth` value is declared.
//! `Themed.ActualWidth == 40` proves the code entry survived the theme chain;
//! `FromTheme.ActualWidth == 17` (`{StaticResource Size.ScrollBar}`, a scalar
//! from the theme's nested `NoesisTheme.Styles.xaml`) proves the theme chain is
//! installed in the same merged dictionary. [`NoesisResourcesInstalled`] still
//! reports the code keys.
//!
//! The view is spawned late (frame 3), so this also checks that the theme patch
//! is keyed on `Added<NoesisView>` and themes a late-spawned view.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDefaultThemePlugin, NoesisDp, NoesisDpChanged,
    NoesisResources, NoesisResourcesInstalled, NoesisView, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Themed"
          Background="{StaticResource AccentBrush}"
          Width="{StaticResource PanelWidth}" Height="10"
          HorizontalAlignment="Left" VerticalAlignment="Top"/>
  <Border x:Name="FromTheme"
          Width="{StaticResource Size.ScrollBar}" Height="10"
          HorizontalAlignment="Left" VerticalAlignment="Bottom"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

#[test]
fn theme_chain_and_code_resources_coexist() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let installed: Arc<Mutex<Vec<Vec<String>>>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();
    // The theme populates `application_resources` with its URI chain.
    app.add_plugins(NoesisDefaultThemePlugin::default());

    // Code-built resources declared alongside the theme.
    app.insert_resource(
        NoesisResources::new()
            .solid("AccentBrush", [1.0, 0.0, 0.0, 1.0])
            .value("PanelWidth", DpValue::F32(40.0)),
    );

    app.add_systems(Startup, |mut reg: ResMut<XamlRegistry>| {
        reg.insert("res.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
    });

    // Spawned at frame 3, after the first update.
    let view_spawn = Arc::clone(&view_entity);
    app.add_systems(
        Update,
        move |mut commands: Commands, mut frame: Local<usize>| {
            *frame += 1;
            if *frame != 3 {
                return;
            }
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "res.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    NoesisDp::new()
                        .watch("Themed", "ActualWidth", DpKind::F32)
                        .watch("FromTheme", "ActualWidth", DpKind::F32),
                ))
                .id();
            *view_spawn.lock().unwrap() = Some(view);
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

    // Latest observed value for a (view, name, property) triple.
    let latest_of = |got: &Observed, view: Entity, name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    let pred_observed = Arc::clone(&observed);
    let pred_installed = Arc::clone(&installed);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        let widths_ok = latest_of(&got, view, "Themed", "ActualWidth") == Some(DpValue::F32(40.0))
            && latest_of(&got, view, "FromTheme", "ActualWidth") == Some(DpValue::F32(17.0));
        let install_ok = pred_installed
            .lock()
            .unwrap()
            .last()
            .is_some_and(|present| {
                present.contains(&"AccentBrush".to_string())
                    && present.contains(&"PanelWidth".to_string())
            });
        widths_ok && install_ok
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    let installs = installed.lock().unwrap().clone();

    let latest = |name: &str, prop: &str| -> Option<DpValue> { latest_of(&got, view, name, prop) };

    assert!(
        converged,
        "theme + code resources never converged within 240 frames; observed {got:?}",
    );

    // The code-built value survived the theme chain: unset would Grid-stretch to
    // 64 (its authored width), 40 proves `{StaticResource PanelWidth}` resolved
    // even though the theme is installed.
    assert_eq!(
        latest("Themed", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "code-built PanelWidth must survive alongside the theme chain (not be clobbered)",
    );

    // The theme chain is installed in the same merged dictionary: a scalar from
    // the theme's own nested Styles.xaml resolves too (proves this isn't just the
    // code-only path with the theme silently absent).
    assert_eq!(
        latest("FromTheme", "ActualWidth"),
        Some(DpValue::F32(17.0)),
        "theme scalar Size.ScrollBar must resolve, proving the theme chain merged in",
    );

    // The install report lists the code-built keys.
    let present = installs.last().expect("a NoesisResourcesInstalled message");
    assert!(
        present.contains(&"AccentBrush".to_string()) && present.contains(&"PanelWidth".to_string()),
        "read-back should confirm both code-built keys present with a theme installed; got {present:?}",
    );
}

//! Regression test for the *mixed* application-resources config: code-built
//! [`NoesisResources`] entries installed alongside a multi-URI
//! [`NoesisView::application_resources`] chain whose later leaf
//! `{StaticResource}`-references an earlier leaf.
//!
//! `headless_app_resources_chain.rs` covers the chain alone. With code-built
//! entries present, the chain must still install leaf by leaf into one parent:
//!
//!   * `styles.xaml`'s `{StaticResource BaseWidth}` (defined in `sizes.xaml`)
//!     resolves, so `Styled.ActualWidth == 40`, and
//!   * the code-built `AccentBrush` lands in the same live dictionary and appears
//!     in the `NoesisResourcesInstalled` read-back.
//!
//! Font-free XAML; no glyph rendering involved.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisResources,
    NoesisResourcesInstalled, NoesisView, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const SIZES_XAML: &str = r##"<ResourceDictionary
    xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
    xmlns:sys="clr-namespace:System;assembly=mscorlib">
  <sys:Double x:Key="BaseWidth">40</sys:Double>
</ResourceDictionary>"##;

const STYLES_XAML: &str = r##"<ResourceDictionary
    xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
    xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
  <Style x:Key="WideBorder" TargetType="Border">
    <Setter Property="Width" Value="{StaticResource BaseWidth}"/>
  </Style>
</ResourceDictionary>"##;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Styled" Style="{StaticResource WideBorder}" Height="10"
          Background="{StaticResource AccentBrush}"
          HorizontalAlignment="Left" VerticalAlignment="Top"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

#[test]
fn mixed_chain_and_code_entries_both_resolve() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let installed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    app.insert_resource(NoesisResources::new().solid("AccentBrush", [1.0, 0.0, 0.0, 1.0]));

    let view_spawn = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "sizes.xaml".to_string(),
                Arc::new(SIZES_XAML.as_bytes().to_vec()),
            );
            reg.insert(
                "styles.xaml".to_string(),
                Arc::new(STYLES_XAML.as_bytes().to_vec()),
            );
            reg.insert("res.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "res.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        // Multi-URI chain: `styles.xaml` depends on `sizes.xaml`.
                        application_resources: vec![
                            "sizes.xaml".to_string(),
                            "styles.xaml".to_string(),
                        ],
                        ..default()
                    },
                    NoesisDp::new().watch("Styled", "ActualWidth", DpKind::F32),
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
              mut res_installed: MessageReader<NoesisResourcesInstalled>| {
            for ev in changes.read() {
                observed_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }
            for ev in res_installed.read() {
                *installed_sys.lock().unwrap() = ev.present.clone();
            }
        },
    );

    let pred_view = Arc::clone(&view_entity);
    let pred_observed = Arc::clone(&observed);
    let pred_installed = Arc::clone(&installed);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let width_ok = pred_observed
            .lock()
            .unwrap()
            .iter()
            .rfind(|(e, n, p, _)| *e == view && n == "Styled" && p == "ActualWidth")
            .map(|(_, _, _, v)| v.clone())
            == Some(DpValue::F32(40.0));
        let brush_ok = pred_installed
            .lock()
            .unwrap()
            .contains(&"AccentBrush".to_string());
        width_ok && brush_ok
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    let present = installed.lock().unwrap().clone();

    assert!(
        converged,
        "mixed config never converged within 240 frames; observed {got:?}, present {present:?}",
    );

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    assert_eq!(
        latest("Styled", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "mixed config must still resolve the chain's cross-leaf StaticResource",
    );

    assert!(
        present.contains(&"AccentBrush".to_string()),
        "code-built AccentBrush should be resolvable in the mixed install; present = {present:?}",
    );
}

//! Regression test for P1.2 + P1.3 remainder: a *multi-URI*
//! `NoesisView::application_resources` chain must resolve cross-leaf
//! `{StaticResource}` references in dependency order.
//!
//! Two `ResourceDictionary` leaves are installed as the chain: `sizes.xaml`
//! defines `BaseWidth`, and `styles.xaml` defines a `Style` whose `Setter`
//! pulls `Width` from `{StaticResource BaseWidth}` — a reference that crosses
//! from the second leaf back to the first. With no code-built `NoesisResources`
//! present the reconcile installs the chain leaf-by-leaf (parent scope wired in
//! first), so the cross-leaf reference resolves and `Styled.ActualWidth == 40`.
//! If each leaf were re-parsed standalone (the merge path) `BaseWidth` would
//! null-resolve at parse time and the Border would stretch to the grid's 64.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisView, XamlRegistry,
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
          HorizontalAlignment="Left" VerticalAlignment="Top"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

#[test]
fn multi_uri_chain_resolves_cross_leaf_static_resource() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

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
    app.add_systems(
        Update,
        move |mut changes: MessageReader<NoesisDpChanged>| {
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

    // Stop as soon as the cross-leaf reference resolves to the chained width.
    let pred_view = Arc::clone(&view_entity);
    let pred_observed = Arc::clone(&observed);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        pred_observed
            .lock()
            .unwrap()
            .iter()
            .rfind(|(e, n, p, _)| *e == view && n == "Styled" && p == "ActualWidth")
            .map(|(_, _, _, v)| v.clone())
            == Some(DpValue::F32(40.0))
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // The cross-leaf `{StaticResource BaseWidth}` (styles.xaml -> sizes.xaml)
    // resolved: 40 is the chained value; an unresolved Setter would leave the
    // Border to Grid-stretch to its authored 64.
    assert!(
        converged,
        "multi-URI chain never converged within 240 frames; observed {got:?}",
    );
    assert_eq!(
        latest("Styled", "ActualWidth"),
        Some(DpValue::F32(40.0)),
        "multi-URI chain must resolve cross-leaf StaticResource in dependency order",
    );
}

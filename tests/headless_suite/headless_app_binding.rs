//! Integration test for [`NoesisBinding`]: value-converter and multi-binding
//! bridges, run through the Noesis bridge pipeline on the headless harness.
//!
//! Sources are sibling elements resolved by `x:Name`; no `DataContext` needed.
//! Assertions read converted target values via [`NoesisDp`] string watch:
//!
//!   * `Upper.Text` = `"HELLO"` (Source.Text `"hello"` uppercased by Rust converter)
//!   * `Full.Text` = `"Ada Lovelace"` (First + Last joined by Rust multi-converter)
//!
//! Font-free XAML; no glyph rendering involved.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    ConvertArg, Converted, DpKind, DpValue, NoesisBinding, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisView, SourceSpec, XamlRegistry,
};

use crate::common::{headless_app, run_until};

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
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

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

    // Latest observed value for a (view, name, property) triple.
    let latest = |got: &Observed, view: Entity, name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // Event-driven exit: stop as soon as both converted targets have landed, not
    // after a padded frame count.
    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        latest(&got, view, "Upper", "Text") == Some(DpValue::Str("HELLO".to_string()))
            && latest(&got, view, "Full", "Text") == Some(DpValue::Str("Ada Lovelace".to_string()))
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    assert!(
        converged,
        "converted + multi bindings never converged within 240 frames; observed {got:?}",
    );
    assert_eq!(
        latest(&got, view, "Upper", "Text"),
        Some(DpValue::Str("HELLO".to_string())),
        "converted binding: Upper.Text should be Source.Text upper-cased \
         (identity would read \"hello\", no binding reads empty)",
    );
    assert_eq!(
        latest(&got, view, "Full", "Text"),
        Some(DpValue::Str("Ada Lovelace".to_string())),
        "multi binding: Full.Text should combine First+Last through the Rust \
         multi-converter",
    );
}

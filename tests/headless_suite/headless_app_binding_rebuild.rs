//! Regression test for [`NoesisBinding`] rebuild-on-re-insert (audit P1.5).
//!
//! A view is spawned with a binding whose Rust converter upper-cases
//! `Source.Text` into `Upper.Text` (`"hello"` → `"HELLO"`). Mid-run the
//! component is re-inserted with a *different* converter (upper-case + `"!"`).
//! Before the fix `has_binding` short-circuited and the new converter was
//! swallowed, freezing `Upper.Text` at `"HELLO"`; now the changed target
//! rebuilds and `Upper.Text` becomes `"HELLO!"`.
//!
//! Font-free XAML; no glyph rendering involved.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    ConvertArg, Converted, DpKind, DpValue, NoesisBinding, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisView, SourceSpec, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const REINSERT_AT_FRAME: usize = 25;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="200" Height="80">
  <StackPanel>
    <TextBox   x:Name="Source" Text="hello"/>
    <TextBlock x:Name="Upper"/>
  </StackPanel>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

/// The first binding: `Upper.Text` = upper-cased `Source.Text`.
fn upper_binding() -> NoesisBinding {
    NoesisBinding::new().converted(
        "Upper",
        "Text",
        SourceSpec::element("Source", "Text"),
        |v: &ConvertArg, _p: &ConvertArg| Some(Converted::String(v.as_str()?.to_uppercase())),
    )
}

/// The re-inserted binding: same target, a *different* converter (adds `"!"`).
fn upper_bang_binding() -> NoesisBinding {
    NoesisBinding::new().converted(
        "Upper",
        "Text",
        SourceSpec::element("Source", "Text"),
        |v: &ConvertArg, _p: &ConvertArg| {
            Some(Converted::String(format!(
                "{}!",
                v.as_str()?.to_uppercase()
            )))
        },
    )
}

#[test]
fn reinserted_binding_rebuilds_with_new_converter() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "binding_rebuild.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "binding_rebuild.xaml".to_string(),
                        size: UVec2::new(200, 80),
                        ..default()
                    },
                    upper_binding(),
                    NoesisDp::new().watch("Upper", "Text", DpKind::Str),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    let view_update = Arc::clone(&view_entity);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut commands: Commands,
              mut changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;
            for ev in changes.read() {
                observed_sys.lock().unwrap().push((
                    ev.view,
                    ev.name.clone(),
                    ev.property.clone(),
                    ev.value.clone(),
                ));
            }
            if *frame == REINSERT_AT_FRAME
                && let Some(view) = *view_update.lock().unwrap()
            {
                commands.entity(view).insert(upper_bang_binding());
            }
        },
    );

    // The latest observed value for a (view, name, property) triple.
    let latest_for = |got: &Observed, view: Entity, name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    // Event-driven exit: the first converter drove "HELLO", then the re-inserted
    // converter rebuilt the target to "HELLO!". Both observed => done.
    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let rebuilt = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        got.iter().any(|(e, n, p, v)| {
            *e == view && n == "Upper" && p == "Text" && *v == DpValue::Str("HELLO".to_string())
        }) && latest_for(&got, view, "Upper", "Text") == Some(DpValue::Str("HELLO!".to_string()))
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    let latest = |name: &str, prop: &str| -> Option<DpValue> { latest_for(&got, view, name, prop) };

    assert!(
        rebuilt,
        "re-inserted binding never rebuilt to \"HELLO!\" within 240 frames; observed {got:?}",
    );

    // The first converter must have driven the target before re-insert.
    assert!(
        got.iter().any(|(e, n, p, v)| *e == view
            && n == "Upper"
            && p == "Text"
            && *v == DpValue::Str("HELLO".to_string())),
        "first binding should have upper-cased Source.Text to \"HELLO\" before re-insert",
    );
    // After re-insert the rebuilt converter wins: the change is applied, not
    // swallowed (which would leave the value frozen at \"HELLO\").
    assert_eq!(
        latest("Upper", "Text"),
        Some(DpValue::Str("HELLO!".to_string())),
        "re-inserted NoesisBinding must rebuild the target with its new converter",
    );
}

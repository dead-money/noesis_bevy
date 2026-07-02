//! Integration test for the `NoesisStyles` bridge (headless, pipelined rendering on).
//!
//! Asserts via [`NoesisDp`] watches:
//!   - `Styled.Opacity`: Style Setter Opacity=0.5 drives it from the default 1.0 to 0.5.
//!   - `Styled.Width`: Style Setter Width=40 drives it from unset to 40.
//!   - `Plain.Opacity`: unstyled sibling stays at 1.0 (negative control for wrong-entity routing).
//!
//! `NoesisStyles` starts empty and is filled at frame 10, after the scene exists.
//! `set_style` applies only on change-detection; a style is sealed on first apply.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DpKind, DpValue, NoesisCamera, NoesisDp, NoesisDpChanged, NoesisStyles, NoesisView, StyleSpec,
    XamlRegistry,
};

use crate::common::{headless_app, run_until};

const SET_AT_FRAME: usize = 10;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Styled" Background="#400000FF"/>
  <Border x:Name="Plain" Background="#4000FF00"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Styled", "Opacity", DpKind::F32) // setter effect
        .watch("Styled", "Width", DpKind::F32) // setter effect (no local value)
        .watch("Plain", "Opacity", DpKind::F32) // negative control
}

#[test]
fn code_built_style_applies_to_named_element() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "styles.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "styles.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    // Starts empty (no-op); filled in after the scene exists so
                    // the one-shot style apply isn't lost.
                    NoesisStyles::new(),
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
              mut q: Query<(&mut NoesisStyles, &mut NoesisDp)>,
              mut changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                for (mut styles, _dp) in &mut q {
                    *styles = NoesisStyles::new().apply(
                        "Styled",
                        StyleSpec::new("Border")
                            .setter("Opacity", DpValue::F32(0.5))
                            .setter("Width", DpValue::F32(40.0)),
                    );
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
        },
    );

    // Stop once the setters have driven Styled and the negative control read back,
    // rather than padding a fixed frame count. The style apply still fires at SET_AT_FRAME.
    let pred_view = Arc::clone(&view_entity);
    let pred_observed = Arc::clone(&observed);
    let converged = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        let latest = |name: &str, prop: &str| -> Option<DpValue> {
            got.iter()
                .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
                .map(|(_, _, _, v)| v.clone())
        };
        latest("Styled", "Opacity") == Some(DpValue::F32(0.5))
            && latest("Styled", "Width") == Some(DpValue::F32(40.0))
            && latest("Plain", "Opacity") == Some(DpValue::F32(1.0))
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisDpChanged ---");
    for (e, name, prop, value) in &got {
        eprintln!("  {e:?} {name}.{prop} = {value:?}");
    }

    assert!(
        converged,
        "styles never converged within 240 frames; observed {got:?}",
    );

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    assert_eq!(
        latest("Styled", "Opacity"),
        Some(DpValue::F32(0.5)),
        "setter: a Style with Setter Opacity=0.5 should drive Styled.Opacity to 0.5 (default 1.0)",
    );
    assert_eq!(
        latest("Styled", "Width"),
        Some(DpValue::F32(40.0)),
        "setter: a Style with Setter Width=40 should drive Styled.Width to 40 (no local value)",
    );
    // Negative control: the style targets one element only; a wrong-entity-routing
    // regression would flip Plain too.
    assert_eq!(
        latest("Plain", "Opacity"),
        Some(DpValue::F32(1.0)),
        "negative control: an unstyled sibling must stay at the default Opacity 1.0",
    );
}

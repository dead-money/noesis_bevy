//! Integration test for [`NoesisStyles`] deep features: `BasedOn` inheritance
//! chains, [`DataTriggerSpec`] (binding-value driven), and [`MultiTriggerSpec`]
//! (all-conditions-hold). Runs end-to-end through the real `NoesisPlugin`
//! pipeline (headless). Assertions observe actual style effects via [`NoesisDp`]
//! watches; element defaults serve as negative controls.
//!
//! Styles are applied at frame 10 rather than startup because `set_style` fires
//! only on Bevy change-detection and a style is sealed on first apply.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    DataTriggerSpec, DpKind, DpValue, MultiTriggerSpec, NoesisCamera, NoesisDp, NoesisDpChanged,
    NoesisStyles, NoesisView, StyleSpec, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const SET_AT_FRAME: usize = 10;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Border x:Name="Chain" Background="#400000FF"/>
  <Border x:Name="Trig" Tag="active" Background="#4000FF00"/>
  <Border x:Name="TrigOff" Tag="idle" Background="#40FF0000"/>
  <Border x:Name="Multi" Background="#4000FFFF"/>
  <Border x:Name="MultiOff" IsEnabled="False" Background="#40FF00FF"/>
  <Border x:Name="Plain" Background="#40FFFF00"/>
</Grid>"##;

type Observed = Vec<(Entity, String, String, DpValue)>;

fn watcher() -> NoesisDp {
    NoesisDp::new()
        .watch("Chain", "Opacity", DpKind::F32) // BasedOn: own setter
        .watch("Chain", "Height", DpKind::F32) // BasedOn: 1 hop
        .watch("Chain", "Width", DpKind::F32) // BasedOn: 2 hops
        .watch("Trig", "Opacity", DpKind::F32) // DataTrigger fires
        .watch("TrigOff", "Opacity", DpKind::F32) // DataTrigger negative control
        .watch("Multi", "Opacity", DpKind::F32) // MultiTrigger fires
        .watch("MultiOff", "Opacity", DpKind::F32) // MultiTrigger negative control
        .watch("Plain", "Opacity", DpKind::F32) // overall negative control
}

#[test]
fn deep_styles_apply_basedon_chain_and_triggers() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "styles_deep.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "styles_deep.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
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
              mut q: Query<&mut NoesisStyles>,
              mut changes: MessageReader<NoesisDpChanged>| {
            *frame += 1;

            if *frame == SET_AT_FRAME {
                // 3-level BasedOn chain: own Opacity, Height one hop, Width two hops.
                let chain = StyleSpec::new("Border")
                    .setter("Opacity", DpValue::F32(0.5))
                    .based_on(
                        StyleSpec::new("Border")
                            .setter("Height", DpValue::F32(12.0))
                            .based_on(StyleSpec::new("Border").setter("Width", DpValue::F32(40.0))),
                    );
                // DataTrigger: Tag binding, RelativeSource Self.
                let trig = StyleSpec::new("Border").data_trigger(
                    DataTriggerSpec::new("Tag", DpValue::Str("active".into()))
                        .relative_source_self()
                        .setter("Opacity", DpValue::F32(0.5)),
                );
                // MultiTrigger: two conditions both true by default.
                let multi = StyleSpec::new("Border").multi_trigger(
                    MultiTriggerSpec::new()
                        .condition("IsEnabled", DpValue::Bool(true))
                        .condition("IsHitTestVisible", DpValue::Bool(true))
                        .setter("Opacity", DpValue::F32(0.5)),
                );

                for mut styles in &mut q {
                    *styles = NoesisStyles::new()
                        .apply("Chain", chain.clone())
                        .apply("Trig", trig.clone())
                        .apply("TrigOff", trig.clone())
                        .apply("Multi", multi.clone())
                        .apply("MultiOff", multi.clone());
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

    // Stop once every watched property (styled effects + default-valued negative
    // controls) has converged, rather than padding a fixed frame count. The style
    // apply still fires at SET_AT_FRAME.
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
        latest("Chain", "Opacity") == Some(DpValue::F32(0.5))
            && latest("Chain", "Height") == Some(DpValue::F32(12.0))
            && latest("Chain", "Width") == Some(DpValue::F32(40.0))
            && latest("Trig", "Opacity") == Some(DpValue::F32(0.5))
            && latest("TrigOff", "Opacity") == Some(DpValue::F32(1.0))
            && latest("Multi", "Opacity") == Some(DpValue::F32(0.5))
            && latest("MultiOff", "Opacity") == Some(DpValue::F32(1.0))
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
        "deep styles never converged within 240 frames; observed {got:?}",
    );

    let latest = |name: &str, prop: &str| -> Option<DpValue> {
        got.iter()
            .rfind(|(e, n, p, _)| *e == view && n == name && p == prop)
            .map(|(_, _, _, v)| v.clone())
    };

    assert_eq!(
        latest("Chain", "Opacity"),
        Some(DpValue::F32(0.5)),
        "BasedOn: the derived style's own Setter Opacity=0.5 (default 1.0)",
    );
    assert_eq!(
        latest("Chain", "Height"),
        Some(DpValue::F32(12.0)),
        "BasedOn: Height=12 inherited one hop up the chain (no local value)",
    );
    assert_eq!(
        latest("Chain", "Width"),
        Some(DpValue::F32(40.0)),
        "BasedOn: Width=40 inherited two hops up the chain — proves the chain links",
    );

    assert_eq!(
        latest("Trig", "Opacity"),
        Some(DpValue::F32(0.5)),
        "DataTrigger: Tag=active matches Value, so Setter Opacity=0.5 applies (default 1.0)",
    );
    assert_eq!(
        latest("TrigOff", "Opacity"),
        Some(DpValue::F32(1.0)),
        "DataTrigger negative control: Tag=idle does not match, so Opacity stays 1.0",
    );

    assert_eq!(
        latest("Multi", "Opacity"),
        Some(DpValue::F32(0.5)),
        "MultiTrigger: both default-true conditions hold, so Setter Opacity=0.5 applies",
    );
    assert_eq!(
        latest("MultiOff", "Opacity"),
        Some(DpValue::F32(1.0)),
        "MultiTrigger negative control: IsEnabled=False breaks a condition, so Opacity stays 1.0",
    );

    assert_eq!(
        latest("Plain", "Opacity"),
        Some(DpValue::F32(1.0)),
        "negative control: an unstyled sibling must stay at the default Opacity 1.0",
    );
}

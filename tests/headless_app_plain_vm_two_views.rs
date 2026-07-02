//! Regression test for two views carrying the *same* plain-struct view model
//! type. The bridge keys entries `(entity, TypeId)` but must register each
//! entity's reflection type under a per-entity unique name; otherwise the second
//! view re-registers the shared global name, fails, retries, and warns every
//! frame (audit P1.6). Here both views must seed their bound `TextBox`
//! independently (Rust→UI), proving each got its own live instance.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisText, NoesisTextChanged, NoesisView, NoesisViewModel,
    NoesisViewModelAppExt, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const SEED_A: &str = "Alpha";
const SEED_B: &str = "Beta";

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <TextBox x:Name="Box" Text="{Binding title, Mode=TwoWay}"/>
</Grid>"##;

#[derive(Component, NoesisViewModel)]
struct DemoVm {
    title: String,
}

#[test]
fn two_views_same_plain_vm_type_both_bind() {
    let text_changes: Arc<Mutex<Vec<(Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let views: Arc<Mutex<HashMap<&'static str, Entity>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut app = headless_app();
    app.add_noesis_view_model::<DemoVm>();

    let views_startup = Arc::clone(&views);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("vm.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let mut spawn = |seed: &str| {
                commands
                    .spawn((
                        Camera2d,
                        NoesisCamera,
                        NoesisView {
                            xaml_uri: "vm.xaml".to_string(),
                            size: UVec2::new(64, 32),
                            ..default()
                        },
                        DemoVm { title: seed.into() },
                        NoesisText::new().watching(["Box"]),
                    ))
                    .id()
            };
            let mut map = views_startup.lock().unwrap();
            map.insert("a", spawn(SEED_A));
            map.insert("b", spawn(SEED_B));
        },
    );

    let text_sys = Arc::clone(&text_changes);
    app.add_systems(
        Update,
        move |mut changes: MessageReader<NoesisTextChanged>| {
            for ev in changes.read() {
                text_sys.lock().unwrap().push((ev.view, ev.text.clone()));
            }
        },
    );

    // Exit once both views have independently seeded their TextBox.
    let pred_texts = Arc::clone(&text_changes);
    let pred_views = Arc::clone(&views);
    let converged = run_until(&mut app, 240, |_app| {
        let map = pred_views.lock().unwrap();
        let (Some(&view_a), Some(&view_b)) = (map.get("a"), map.get("b")) else {
            return false;
        };
        let texts = pred_texts.lock().unwrap();
        texts.iter().any(|(e, t)| *e == view_a && t == SEED_A)
            && texts.iter().any(|(e, t)| *e == view_b && t == SEED_B)
    });

    let views = views.lock().unwrap().clone();
    let view_a = views["a"];
    let view_b = views["b"];
    let texts = text_changes.lock().unwrap().clone();

    assert!(
        converged,
        "both views' plain VMs never seeded within 240 frames; got {texts:?}",
    );

    assert!(
        texts.iter().any(|(e, t)| *e == view_a && t == SEED_A),
        "view A's plain VM never seeded its TextBox; got {texts:?}",
    );
    assert!(
        texts.iter().any(|(e, t)| *e == view_b && t == SEED_B),
        "view B's plain VM never seeded its TextBox (second-view registration collision?); \
         got {texts:?}",
    );
}

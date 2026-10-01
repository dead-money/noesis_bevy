//! End-to-end test of the per-entity plain-struct view model bridge
//! (`#[derive(Component, NoesisViewModel)]` + `add_noesis_view_model::<T>()`).
//!
//! Two directions:
//!   * Rust to UI: `DemoVm.title = "Hello"` binds to a `<TextBox>` via
//!     `{Binding title}`; a [`NoesisText`] watch confirms the control sees it.
//!   * UI to Rust: a [`NoesisDp`] write sets the `TextBox`'s `Text` to `"World"`,
//!     and the `TwoWay`/`PropertyChanged` binding must write it back into the
//!     `DemoVm` component.
//!
//! `headless_app_plain_vm_two_views.rs` covers two views with the same view model
//! type.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisDp, NoesisText, NoesisTextChanged, NoesisView, NoesisViewModel,
    NoesisViewModelAppExt, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const SEED: &str = "Hello";
const EDIT: &str = "World";
const EDIT_AT_FRAME: usize = 14;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <TextBox x:Name="Box"
           Text="{Binding title, Mode=TwoWay, UpdateSourceTrigger=PropertyChanged}"/>
</Grid>"##;

/// Attached by the bridge as the view root's `DataContext`.
#[derive(Component, NoesisViewModel)]
struct DemoVm {
    title: String,
}

#[test]
fn plain_vm_component_round_trips_two_way() {
    let titles: Arc<Mutex<HashMap<Entity, String>>> = Arc::new(Mutex::new(HashMap::new()));
    let text_changes: Arc<Mutex<Vec<(Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();
    app.add_noesis_view_model::<DemoVm>();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert("vm.xaml".to_string(), Arc::new(XAML.as_bytes().to_vec()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "vm.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    DemoVm { title: SEED.into() },
                    NoesisText::new().watching(["Box"]),
                    NoesisDp::new(),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let titles_sys = Arc::clone(&titles);
    let text_sys = Arc::clone(&text_changes);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              vms: Query<(Entity, &DemoVm)>,
              mut dps: Query<&mut NoesisDp>,
              mut changes: MessageReader<NoesisTextChanged>| {
            *frame += 1;

            {
                let mut snap = titles_sys.lock().unwrap();
                for (e, vm) in &vms {
                    snap.insert(e, vm.title.clone());
                }
            }
            for ev in changes.read() {
                text_sys.lock().unwrap().push((ev.view, ev.text.clone()));
            }

            // Stands in for a user edit.
            if *frame == EDIT_AT_FRAME {
                for mut dp in &mut dps {
                    *dp = NoesisDp::new().set_string("Box", "Text", EDIT);
                }
            }
        },
    );

    let pred_titles = Arc::clone(&titles);
    let pred_texts = Arc::clone(&text_changes);
    let pred_view = Arc::clone(&view_entity);
    let converged = run_until(&mut app, 240, |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let seeded = pred_texts
            .lock()
            .unwrap()
            .iter()
            .any(|(e, t)| *e == view && t == SEED);
        let wrote_back = pred_titles.lock().unwrap().get(&view).map(String::as_str) == Some(EDIT);
        seeded && wrote_back
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let final_titles = titles.lock().unwrap().clone();
    let texts = text_changes.lock().unwrap().clone();

    assert!(
        converged,
        "plain VM two-way round-trip never converged within 240 frames; \
         titles {final_titles:?} text changes {texts:?}",
    );

    assert!(
        texts.iter().any(|(e, t)| *e == view && t == SEED),
        "Rust→UI snapshot never reached the bound TextBox; got text changes {texts:?}",
    );

    assert_eq!(
        final_titles.get(&view).map(String::as_str),
        Some(EDIT),
        "UI→Rust writeback never reached the DemoVm component; titles {final_titles:?}",
    );
}

//! End-to-end test of the per-entity plain-struct view model bridge
//! (`#[derive(Component, NoesisViewModel)]` + `add_noesis_view_model::<T>()`).
//!
//! Two assertion directions:
//!   * **Rust→UI.** `DemoVm.title = "Hello"` binds to a `<TextBox>` via
//!     `{Binding title}`; a [`NoesisText`] watch confirms the control sees it.
//!   * **UI→Rust.** A [`NoesisDp`] write sets the `TextBox`'s `Text` to `"World"`;
//!     the `TwoWay/PropertyChanged` binding must push that back into the `DemoVm`
//!     component via the reconcile system.
//!
//! Two views carrying the *same* `#[derive(NoesisViewModel)]` type are covered by
//! `headless_app_plain_vm_two_views.rs` (the bridge registers each entity's
//! reflection type under a per-entity unique name, so they don't collide).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    NoesisCamera, NoesisDp, NoesisPlugin, NoesisText, NoesisTextChanged, NoesisView,
    NoesisViewModel, NoesisViewModelAppExt, XamlRegistry,
};

const SEED: &str = "Hello";
const EDIT: &str = "World";
const EDIT_AT_FRAME: usize = 14;
const EXIT_AT_FRAME: usize = 48;

const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <TextBox x:Name="Box"
           Text="{Binding title, Mode=TwoWay, UpdateSourceTrigger=PropertyChanged}"/>
</Grid>"##;

/// Bridge attaches this as the view-root `DataContext`.
#[derive(Component, NoesisViewModel)]
struct DemoVm {
    title: String,
}

#[test]
fn plain_vm_component_round_trips_two_way() {
    noesis_license_from_env();

    let titles: Arc<Mutex<HashMap<Entity, String>>> = Arc::new(Mutex::new(HashMap::new()));
    // Rust→UI proof: Box text from the NoesisText watch
    let text_changes: Arc<Mutex<Vec<(Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = App::new();
    app.add_plugins(
        DefaultPlugins
            .build()
            .disable::<bevy::winit::WinitPlugin>()
            .set(WindowPlugin {
                primary_window: None,
                exit_condition: ExitCondition::DontExit,
                close_when_requested: false,
                ..default()
            }),
    );
    app.add_plugins(ScheduleRunnerPlugin::run_loop(Duration::from_millis(4)));
    app.add_plugins(NoesisPlugin::default());
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
              mut changes: MessageReader<NoesisTextChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            // UI→Rust readback
            {
                let mut snap = titles_sys.lock().unwrap();
                for (e, vm) in &vms {
                    snap.insert(e, vm.title.clone());
                }
            }
            // Rust→UI readback
            for ev in changes.read() {
                text_sys.lock().unwrap().push((ev.view, ev.text.clone()));
            }

            // simulate user edit via DP; TwoWay/PropertyChanged must push back to DemoVm
            if *frame == EDIT_AT_FRAME {
                for mut dp in &mut dps {
                    *dp = NoesisDp::new().set_string("Box", "Text", EDIT);
                }
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let final_titles = titles.lock().unwrap().clone();
    let texts = text_changes.lock().unwrap().clone();

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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

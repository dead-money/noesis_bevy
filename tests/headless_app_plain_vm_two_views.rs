//! Regression test for two views carrying the *same* plain-struct view model
//! type. The bridge keys entries `(entity, TypeId)` but must register each
//! entity's reflection type under a per-entity unique name; otherwise the second
//! view re-registers the shared global name, fails, retries, and warns every
//! frame (audit P1.6). Here both views must seed their bound `TextBox`
//! independently (Rust→UI), proving each got its own live instance.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    NoesisCamera, NoesisPlugin, NoesisText, NoesisTextChanged, NoesisView, NoesisViewModel,
    NoesisViewModelAppExt, XamlRegistry,
};

const SEED_A: &str = "Alpha";
const SEED_B: &str = "Beta";
const EXIT_AT_FRAME: usize = 48;

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
    noesis_license_from_env();

    let text_changes: Arc<Mutex<Vec<(Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let views: Arc<Mutex<HashMap<&'static str, Entity>>> = Arc::new(Mutex::new(HashMap::new()));

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
        move |mut frame: Local<usize>,
              mut changes: MessageReader<NoesisTextChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;
            for ev in changes.read() {
                text_sys.lock().unwrap().push((ev.view, ev.text.clone()));
            }
            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let views = views.lock().unwrap().clone();
    let view_a = views["a"];
    let view_b = views["b"];
    let texts = text_changes.lock().unwrap().clone();

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

fn noesis_license_from_env() {
    if let (Ok(name), Ok(key)) = (
        std::env::var("NOESIS_LICENSE_NAME"),
        std::env::var("NOESIS_LICENSE_KEY"),
    ) {
        noesis_runtime::set_license(&name, &key);
    }
}

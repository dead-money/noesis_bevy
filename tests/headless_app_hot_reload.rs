//! Bevy-app-level integration test for **XAML hot-reload** (TODO §3), exercised
//! end-to-end through the real `NoesisPlugin` pipeline (headless, pipelined
//! rendering on).
//!
//! A live [`NoesisView`] is built from an in-memory XAML whose `Label` element
//! reads `Text="VERSION ONE"`. We pump frames and observe that value through a
//! [`NoesisText`] watch. Then we replace the *same URI*'s bytes in
//! [`XamlRegistry`] with markup reading `Text="VERSION TWO"` (the path an asset
//! `Modified` event drives) and pump more frames.
//!
//! The assertion is bluff-resistant: the new value differs from the old, and a
//! no-op hot-reload would keep the view built from the first bytes — its watch
//! would keep reporting `VERSION ONE` and never `VERSION TWO`, failing the test.
//! We also assert the *old* value was genuinely observed first, so the test
//! can't pass by the view never having built against the original bytes.
//!
//! Font-free assertion path: we only read the `Text` dependency property, no
//! glyph rendering, so the scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    NoesisCamera, NoesisPlugin, NoesisText, NoesisTextChanged, NoesisView, XamlRegistry,
};

const URI: &str = "hot.xaml";
const RELOAD_AT_FRAME: usize = 25;
const EXIT_AT_FRAME: usize = 90;

fn xaml(label: &str) -> String {
    format!(
        r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <TextBlock x:Name="Label" Text="{label}"/>
</Grid>"##
    )
}

type Observed = Vec<(Entity, String, String)>;

#[test]
fn xaml_hot_reload_rebuilds_view_with_new_markup() {
    noesis_license_from_env();

    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
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

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(URI.to_string(), Arc::new(xaml("VERSION ONE").into_bytes()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: URI.to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                    NoesisText::new().watching(["Label"]),
                ))
                .id();
            *view_startup.lock().unwrap() = Some(view);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut reg: ResMut<XamlRegistry>,
              mut changes: MessageReader<NoesisTextChanged>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            // Replace the SAME URI's bytes with new markup — the path an asset
            // `Modified` event drives through `update_xaml_registry`. The render
            // state must notice the bytes changed and rebuild the view.
            if *frame == RELOAD_AT_FRAME {
                reg.insert(URI.to_string(), Arc::new(xaml("VERSION TWO").into_bytes()));
            }

            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.text.clone()));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed NoesisTextChanged ---");
    for (e, name, text) in &got {
        eprintln!("  {e:?} {name} = {text:?}");
    }

    let texts: Vec<&str> = got
        .iter()
        .filter(|(e, n, _)| *e == view && n == "Label")
        .map(|(_, _, t)| t.as_str())
        .collect();

    assert!(
        texts.contains(&"VERSION ONE"),
        "expected to observe the original markup's text before reload; got {texts:?}",
    );
    assert_eq!(
        texts.last().copied(),
        Some("VERSION TWO"),
        "hot-reload should rebuild the view against the new bytes so the latest \
         observed Text is the reloaded value (a no-op reload would stay on \
         VERSION ONE); got {texts:?}",
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

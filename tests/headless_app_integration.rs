//! Integration test for the three Noesis host callbacks: cursor
//! ([`NoesisCursorRequested`]), open-URL ([`NoesisOpenUrl`]), and play-audio
//! ([`NoesisPlayAudio`]). Single headless app (one Noesis init/shutdown per process).
//!
//! Mouse input goes directly onto [`NoesisInputQueue`] rather than through
//! `NoesisInputPlugin` because the windowed forwarders need a `PrimaryWindow`.
//! Font-free XAML; no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use noesis_bevy::{
    CursorType, NoesisCamera, NoesisCursorRequested, NoesisInputEvent, NoesisInputQueue,
    NoesisOpenUrl, NoesisPlayAudio, NoesisPlugin, NoesisView, XamlRegistry, open_url, play_audio,
};

// View is built on first PostUpdate; these constants ensure it exists before use.
const MOVE_AT_FRAME: usize = 12;
const TRIGGER_AT_FRAME: usize = 16;
const EXIT_AT_FRAME: usize = 40;

const URL: &str = "https://www.noesisengine.com/docs/";
const AUDIO_URI: &str = "click.wav";
const AUDIO_VOLUME: f32 = 0.5;

// Root sets a non-default `Cursor`; `Background` makes the `Grid` hit-testable so
// a mouse-move over it raises the cursor callback.
const XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Background="#FF202020" Width="200" Height="200" Cursor="Hand"/>"##;

#[derive(Default)]
struct Captured {
    cursors: Vec<CursorType>,
    urls: Vec<String>,
    audio: Vec<(String, f32)>,
}

#[test]
fn integration_callbacks_surface_as_messages() {
    noesis_license_from_env();

    let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(Captured::default()));

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

    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "integration.xaml".to_string(),
                Arc::new(XAML.as_bytes().to_vec()),
            );
            commands.spawn((
                Camera2d,
                NoesisCamera,
                NoesisView {
                    xaml_uri: "integration.xaml".to_string(),
                    size: UVec2::new(200, 200),
                    ..default()
                },
            ));
        },
    );

    let cap = Arc::clone(&captured);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut queue: ResMut<NoesisInputQueue>,
              mut cursor: MessageReader<NoesisCursorRequested>,
              mut urls: MessageReader<NoesisOpenUrl>,
              mut audio: MessageReader<NoesisPlayAudio>,
              mut exit: MessageWriter<AppExit>| {
            *frame += 1;

            // Repeated across frames to guarantee an enter/move firing.
            if (MOVE_AT_FRAME..MOVE_AT_FRAME + 3).contains(&*frame) {
                queue.push(NoesisInputEvent::MouseMove { x: 100, y: 100 });
            }

            if *frame == TRIGGER_AT_FRAME {
                open_url(URL);
                play_audio(AUDIO_URI, AUDIO_VOLUME);
            }

            let mut c = cap.lock().unwrap();
            for ev in cursor.read() {
                c.cursors.push(ev.cursor);
            }
            for ev in urls.read() {
                c.urls.push(ev.url.clone());
            }
            for ev in audio.read() {
                c.audio.push((ev.uri.clone(), ev.volume));
            }

            if *frame >= EXIT_AT_FRAME {
                exit.write(AppExit::Success);
            }
        },
    );

    app.run();

    let got = captured.lock().unwrap();
    eprintln!("--- integration messages ---");
    eprintln!("  cursors: {:?}", got.cursors);
    eprintln!("  urls:    {:?}", got.urls);
    eprintln!("  audio:   {:?}", got.audio);

    assert!(
        got.cursors.contains(&CursorType::Hand),
        "cursor: a mouse-move over a Cursor=\"Hand\" element should raise \
         NoesisCursorRequested{{Hand}}; observed {:?}",
        got.cursors,
    );

    assert!(
        got.urls.iter().any(|u| u == URL),
        "open_url should surface NoesisOpenUrl with the exact URL; observed {:?}",
        got.urls,
    );

    assert!(
        got.audio
            .iter()
            .any(|(uri, vol)| uri == AUDIO_URI && (*vol - AUDIO_VOLUME).abs() < f32::EPSILON),
        "play_audio should surface NoesisPlayAudio with the exact uri+volume; observed {:?}",
        got.audio,
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

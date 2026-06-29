//! Bevy-app-level integration test for the **system-integration bridge**
//! ([`dm_noesis_bevy::integration`]), exercised end-to-end through the real
//! `NoesisPlugin` pipeline (headless, pipelined rendering on, no window).
//!
//! The bridge registers Noesis's three process-global host callbacks and
//! surfaces each as a Bevy message. All three are driven here from a single
//! headless app (one Noesis init/shutdown per process):
//!
//!   * **cursor** ([`NoesisCursorRequested`]) — the genuinely engine-driven
//!     path. The root `Grid` declares `Cursor="Hand"` and is hit-testable
//!     (`Background` set), so feeding a `MouseMove` over it through the live
//!     view's event pump makes Noesis raise a cursor change. We assert the
//!     message reports exactly `CursorType::Hand` — the default cursor for a
//!     bare view is `Arrow`/`None`, so a missing-apply / wrong-element regression
//!     reads back a non-`Hand` value (or nothing) and fails. This is the
//!     built-in negative control.
//!   * **open-URL** ([`NoesisOpenUrl`]) — driven synchronously via
//!     [`open_url`], a genuine engine round-trip (the engine invokes the
//!     registered callback inline). We assert the exact URL flows through, not a
//!     default/empty string.
//!   * **play-audio** ([`NoesisPlayAudio`]) — driven synchronously via
//!     [`play_audio`]. We assert both the exact URI *and* the exact volume
//!     (`0.5`) round-trip — a placeholder/zero would fail.
//!
//! The `MouseMove` is pushed directly onto [`NoesisInputQueue`] (the windowed
//! forwarders in `NoesisInputPlugin` need a `PrimaryWindow`, which a headless
//! app has none of) in *view* coordinates; that is exactly what the render-side
//! `apply_noesis_input` consumes, so the cursor path is still a real engine
//! firing, not a synthesized message.
//!
//! Font-free XAML, so the scene builds with no font gate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy::app::{AppExit, ScheduleRunnerPlugin};
use bevy::prelude::*;
use bevy::window::{ExitCondition, WindowPlugin};
use dm_noesis_bevy::{
    CursorType, NoesisCamera, NoesisCursorRequested, NoesisInputEvent, NoesisInputQueue,
    NoesisOpenUrl, NoesisPlayAudio, NoesisPlugin, NoesisView, XamlRegistry, open_url, play_audio,
};

// Frame schedule. The view is built on the first PostUpdate; by these frames it
// exists and is active.
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

            // Drive the cursor callback: a move into the Cursor="Hand" Grid.
            // Repeated across a few frames to guarantee an enter/move firing.
            if (MOVE_AT_FRAME..MOVE_AT_FRAME + 3).contains(&*frame) {
                queue.push(NoesisInputEvent::MouseMove { x: 100, y: 100 });
            }

            // Drive the synchronous host round-trips once.
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

    // Cursor: the engine raised a change for the Hand-cursor element. The
    // default (no Cursor / not over the element) would be Arrow or None.
    assert!(
        got.cursors.contains(&CursorType::Hand),
        "cursor: a mouse-move over a Cursor=\"Hand\" element should raise \
         NoesisCursorRequested{{Hand}}; observed {:?}",
        got.cursors,
    );

    // Open-URL: exact string round-trips (not empty/default).
    assert!(
        got.urls.iter().any(|u| u == URL),
        "open_url should surface NoesisOpenUrl with the exact URL; observed {:?}",
        got.urls,
    );

    // Play-audio: exact URI and exact volume round-trip.
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

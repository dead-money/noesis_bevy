//! Integration test for the three Noesis host callbacks: cursor
//! ([`NoesisCursorRequested`]), open-URL ([`NoesisOpenUrl`]), and play-audio
//! ([`NoesisPlayAudio`]). Single headless app (one Noesis init/shutdown per process).
//!
//! Mouse input goes directly onto [`NoesisInputQueue`] rather than through
//! `NoesisInputPlugin` because the windowed forwarders need a `PrimaryWindow`.
//! Font-free XAML; no font gate.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    CursorType, NoesisCamera, NoesisCursorRequested, NoesisInputEvent, NoesisInputQueue,
    NoesisOpenUrl, NoesisPlayAudio, NoesisView, XamlRegistry, open_url, play_audio,
};

use crate::common::{headless_app, run_until};

// Frame-gated stimulus: the view is built on first PostUpdate, so move the mouse
// once it exists, then trigger the open-URL / play-audio calls. Frames are instant
// under run_until; the exit predicate is the captured callbacks, not a count.
const MOVE_AT_FRAME: usize = 12;
const TRIGGER_AT_FRAME: usize = 16;

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
    let captured: Arc<Mutex<Captured>> = Arc::new(Mutex::new(Captured::default()));

    let mut app = headless_app();

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
              mut audio: MessageReader<NoesisPlayAudio>| {
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
        },
    );

    // Exit once all three callbacks have surfaced as messages.
    let pred_cap = Arc::clone(&captured);
    let converged = run_until(&mut app, 240, |_app| {
        let c = pred_cap.lock().unwrap();
        c.cursors.contains(&CursorType::Hand)
            && c.urls.iter().any(|u| u == URL)
            && c.audio
                .iter()
                .any(|(uri, vol)| uri == AUDIO_URI && (*vol - AUDIO_VOLUME).abs() < f32::EPSILON)
    });

    let got = captured.lock().unwrap();
    eprintln!("--- integration messages ---");
    eprintln!("  cursors: {:?}", got.cursors);
    eprintln!("  urls:    {:?}", got.urls);
    eprintln!("  audio:   {:?}", got.audio);

    assert!(
        converged,
        "not all host callbacks surfaced within 240 frames; cursors {:?} urls {:?} audio {:?}",
        got.cursors, got.urls, got.audio,
    );

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

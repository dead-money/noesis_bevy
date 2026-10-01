//! Integration test for *panel fragment* hot-reload through the real
//! `NoesisPlugin` pipeline.
//!
//! A `UiPanel` fragment is built by `sync_panel` (Apply phase) and cached in
//! `NoesisRenderState::panels`, outside the scene fetch-log window — so without
//! its own re-parse guard, editing a fragment's XAML would re-insert bytes that
//! never reach the screen. This asserts the guard: editing the fragment file
//! rebuilds the fragment tree against the new markup.
//!
//! Observes the fragment element's `Text` via `NoesisPanelText` (fragment-scope
//! namescope) — no glyph rendering, so no font setup needed.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisPanelText, NoesisPanelTextChanged, NoesisView, UiPanel, XamlRegistry,
};

use crate::common::{headless_app, run_until};

const HOST_URI: &str = "host.xaml";
const FRAG_URI: &str = "frag.xaml";
const SLOT: &str = "Slot";
const FRAG_NAME: &str = "Frag";
const RELOAD_AT_FRAME: usize = 25;

// Host scene: a named Panel the fragment mounts into. Unchanged across the test.
const HOST: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <StackPanel x:Name="Slot"/>
</Grid>"##;

fn fragment(label: &str) -> String {
    format!(
        r##"<TextBlock xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      x:Name="Frag" Text="{label}"/>"##
    )
}

type Observed = Vec<(Entity, String, String)>;

#[test]
fn panel_fragment_reload_rebuilds_fragment_tree() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let panel_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let panel_startup = Arc::clone(&panel_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(HOST_URI.to_string(), Arc::new(HOST.as_bytes().to_vec()));
            reg.insert(
                FRAG_URI.to_string(),
                Arc::new(fragment("FRAG ONE").into_bytes()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: HOST_URI.to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                ))
                .id();
            let panel = commands
                .spawn((
                    UiPanel::new(FRAG_URI)
                        .mount_into(view, SLOT)
                        .static_context(),
                    NoesisPanelText::new().watching([FRAG_NAME]),
                ))
                .id();
            *panel_startup.lock().unwrap() = Some(panel);
        },
    );

    let observed_sys = Arc::clone(&observed);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut reg: ResMut<XamlRegistry>,
              mut changes: MessageReader<NoesisPanelTextChanged>| {
            *frame += 1;

            // Edit only the fragment file; the host scene is untouched. A fresh
            // Arc is what the fragment's re-parse guard compares against.
            if *frame == RELOAD_AT_FRAME {
                reg.insert(
                    FRAG_URI.to_string(),
                    Arc::new(fragment("FRAG TWO").into_bytes()),
                );
            }

            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.panel, ev.name.clone(), ev.text.clone()));
            }
        },
    );

    let pred_observed = Arc::clone(&observed);
    let pred_panel = Arc::clone(&panel_entity);
    let reloaded = run_until(&mut app, 240, move |_app| {
        let Some(panel) = *pred_panel.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        let saw_one = got
            .iter()
            .any(|(p, n, t)| *p == panel && n == FRAG_NAME && t == "FRAG ONE");
        let latest_two = got
            .iter()
            .rfind(|(p, n, _)| *p == panel && n == FRAG_NAME)
            .map(|(_, _, t)| t.as_str())
            == Some("FRAG TWO");
        saw_one && latest_two
    });

    let panel = panel_entity.lock().unwrap().expect("panel spawned");
    let got = observed.lock().unwrap().clone();
    let texts: Vec<&str> = got
        .iter()
        .filter(|(p, n, _)| *p == panel && n == FRAG_NAME)
        .map(|(_, _, t)| t.as_str())
        .collect();

    assert!(
        reloaded,
        "fragment reload never converged to FRAG TWO within 240 frames; got {texts:?}",
    );
    assert!(
        texts.contains(&"FRAG ONE"),
        "expected the original fragment text before reload; got {texts:?}",
    );
    assert_eq!(
        texts.last().copied(),
        Some("FRAG TWO"),
        "editing the fragment file should re-parse the cached fragment so the latest \
         observed Text is the reloaded value (the Vacant-only build path would stay \
         on FRAG ONE); got {texts:?}",
    );
}

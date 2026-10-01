//! Integration test for *application-resources / theme* hot-reload through the
//! real `NoesisPlugin` pipeline.
//!
//! A theme dictionary pulled via `NoesisView.application_resources` installs
//! into the process-global resource dictionary through `reconcile_app_resources`
//! (Sync phase), whose unchanged-check was keyed on the URI *list*, not bytes —
//! so editing the theme in place used to be a no-op. This asserts the
//! byte-keyed reinstall + `app_resources_epoch` rebuild: editing the theme file
//! restyles the live view.
//!
//! The theme defines a `Style` whose setter drives a named `TextBlock`'s `Text`,
//! read via `NoesisText` — no glyph rendering, so no font setup needed.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisText, NoesisTextChanged, NoesisView, XamlRegistry};

use crate::common::{headless_app, run_until};

const VIEW_URI: &str = "themed.xaml";
const THEME_URI: &str = "theme.xaml";
const RELOAD_AT_FRAME: usize = 25;

// The view applies a themed style by key; its own bytes never change.
const VIEW: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <TextBlock x:Name="Label" Style="{StaticResource LabelStyle}"/>
</Grid>"##;

fn theme(label: &str) -> String {
    format!(
        r##"<ResourceDictionary xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
  <Style x:Key="LabelStyle" TargetType="TextBlock">
    <Setter Property="Text" Value="{label}"/>
  </Style>
</ResourceDictionary>"##
    )
}

type Observed = Vec<(Entity, String, String)>;

#[test]
fn theme_reload_restyles_live_view() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(VIEW_URI.to_string(), Arc::new(VIEW.as_bytes().to_vec()));
            reg.insert(
                THEME_URI.to_string(),
                Arc::new(theme("THEME ONE").into_bytes()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: VIEW_URI.to_string(),
                        size: UVec2::new(64, 32),
                        application_resources: vec![THEME_URI.to_string()],
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
              mut changes: MessageReader<NoesisTextChanged>| {
            *frame += 1;

            // Edit only the theme dictionary; the view's own bytes and the URI
            // list are unchanged — only the byte-keyed check catches this.
            if *frame == RELOAD_AT_FRAME {
                reg.insert(
                    THEME_URI.to_string(),
                    Arc::new(theme("THEME TWO").into_bytes()),
                );
            }

            for ev in changes.read() {
                observed_sys
                    .lock()
                    .unwrap()
                    .push((ev.view, ev.name.clone(), ev.text.clone()));
            }
        },
    );

    let pred_observed = Arc::clone(&observed);
    let pred_view = Arc::clone(&view_entity);
    let reloaded = run_until(&mut app, 240, move |_app| {
        let Some(view) = *pred_view.lock().unwrap() else {
            return false;
        };
        let got = pred_observed.lock().unwrap();
        let saw_one = got
            .iter()
            .any(|(e, n, t)| *e == view && n == "Label" && t == "THEME ONE");
        let latest_two = got
            .iter()
            .rfind(|(e, n, _)| *e == view && n == "Label")
            .map(|(_, _, t)| t.as_str())
            == Some("THEME TWO");
        saw_one && latest_two
    });

    let view = view_entity.lock().unwrap().expect("view spawned");
    let got = observed.lock().unwrap().clone();
    let texts: Vec<&str> = got
        .iter()
        .filter(|(e, n, _)| *e == view && n == "Label")
        .map(|(_, _, t)| t.as_str())
        .collect();

    assert!(
        reloaded,
        "theme reload never converged to THEME TWO within 240 frames; got {texts:?}",
    );
    assert!(
        texts.contains(&"THEME ONE"),
        "expected the original theme's setter value before reload; got {texts:?}",
    );
    assert_eq!(
        texts.last().copied(),
        Some("THEME TWO"),
        "editing an application-resources theme dictionary should reinstall it and rebuild \
         the view so the latest observed Text is the reloaded value (the URI-list-only check \
         would stay on THEME ONE); got {texts:?}",
    );
}

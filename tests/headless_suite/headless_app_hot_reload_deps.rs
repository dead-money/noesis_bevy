//! Integration test for XAML *dependency* hot-reload through the real
//! `NoesisPlugin` pipeline.
//!
//! The view's root markup (`main.xaml`) never changes; only a merged
//! `ResourceDictionary` it pulls via `Source="styles.xaml"` is edited. A no-op
//! (root-only) reload would keep reporting the original value; the dependency
//! fetch-log is what lets `ensure_scene` notice the merged dictionary's bytes
//! changed and rebuild the view against them.
//!
//! Observes the `Text` a `Style` setter (defined in the dictionary) applies to
//! a named `TextBlock` — no glyph rendering, so no font setup is needed.

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{NoesisCamera, NoesisText, NoesisTextChanged, NoesisView, XamlRegistry};

use crate::common::{headless_app, run_until};

const ROOT_URI: &str = "main.xaml";
const DEP_URI: &str = "styles.xaml";
const RELOAD_AT_FRAME: usize = 25;

// Root merges the dictionary and applies its style to `Label`. The root bytes
// are identical across the whole test — only `styles.xaml` is edited.
const ROOT: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="64" Height="32">
  <Grid.Resources>
    <ResourceDictionary>
      <ResourceDictionary.MergedDictionaries>
        <ResourceDictionary Source="styles.xaml"/>
      </ResourceDictionary.MergedDictionaries>
    </ResourceDictionary>
  </Grid.Resources>
  <TextBlock x:Name="Label" Style="{StaticResource LabelStyle}"/>
</Grid>"##;

fn dep(label: &str) -> String {
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
fn xaml_dependency_reload_rebuilds_dependent_view() {
    let observed: Arc<Mutex<Observed>> = Arc::new(Mutex::new(Vec::new()));
    let view_entity: Arc<Mutex<Option<Entity>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();

    let view_startup = Arc::clone(&view_entity);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(ROOT_URI.to_string(), Arc::new(ROOT.as_bytes().to_vec()));
            reg.insert(DEP_URI.to_string(), Arc::new(dep("DEP ONE").into_bytes()));
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: ROOT_URI.to_string(),
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
              mut changes: MessageReader<NoesisTextChanged>| {
            *frame += 1;

            // Edit ONLY the merged dictionary, leaving the root untouched. A
            // fresh `Arc` is what the fetch-log compares against.
            if *frame == RELOAD_AT_FRAME {
                reg.insert(DEP_URI.to_string(), Arc::new(dep("DEP TWO").into_bytes()));
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
            .any(|(e, n, t)| *e == view && n == "Label" && t == "DEP ONE");
        let latest_two = got
            .iter()
            .rfind(|(e, n, _)| *e == view && n == "Label")
            .map(|(_, _, t)| t.as_str())
            == Some("DEP TWO");
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
        "dependency reload never converged to DEP TWO within 240 frames; got {texts:?}",
    );
    assert!(
        texts.contains(&"DEP ONE"),
        "expected the original dictionary's setter value before reload; got {texts:?}",
    );
    assert_eq!(
        texts.last().copied(),
        Some("DEP TWO"),
        "editing the merged dictionary should rebuild the view that pulled it so the \
         latest observed Text is the reloaded value (a root-only reload check would \
         stay on DEP ONE); got {texts:?}",
    );
}

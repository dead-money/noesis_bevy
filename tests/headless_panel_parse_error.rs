//! F5 regression: a [`UiPanel`] whose fragment fails to load degrades gracefully.
//! It never mounts, the app does not panic, and a sibling panel with a valid
//! fragment is unaffected.
//!
//! The reliable hard failure is a missing / unregistered fragment URI (a typo'd
//! registration key): `FrameworkElement::load` returns `None`, which F5 surfaces
//! as a deduped Bevy `error!`. Noesis's XAML parser is lenient about *malformed*
//! markup (an unbalanced or mismatched tag still returns a partial element, with
//! only a Noesis-side parser warning), so this test exercises the missing-URI path.
//!
//! What this asserts: no panic, `live_panels == 1` (only the good fragment built a
//! `PanelEntry`; the missing one's build returned `None`), and the good panel's
//! binding still reaches the UI. It does NOT assert the `error!` fired (tracing
//! capture is fiddly headless); the loud log is verified by inspection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::{
    NoesisCamera, NoesisDiagnostics, NoesisPanelAppExt, NoesisPanelText, NoesisPanelTextChanged,
    NoesisView, NoesisViewModel, UiPanel, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      Width="256" Height="256">
  <StackPanel x:Name="Hud"/>
</Grid>"##;

const GOOD_XAML: &str = r##"<StackPanel xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml">
  <TextBlock x:Name="GoodText" Text="{Binding Health}"/>
</StackPanel>"##;

// The "broken" panel points at a URI that is never registered, so the provider
// can't serve it and `FrameworkElement::load` returns `None` (a hard load failure).
const MISSING_URI: &str = "missing.xaml";

#[derive(Component, NoesisViewModel)]
struct Health(f32);

#[test]
fn broken_fragment_degrades_gracefully_without_blocking_siblings() {
    let captured: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));

    let mut app = headless_app();
    app.add_noesis_panel_field::<Health>();

    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            for (name, xaml) in [("host.xaml", HOST_XAML), ("good.xaml", GOOD_XAML)] {
                reg.insert(name.to_string(), Arc::new(xaml.as_bytes().to_vec()));
            }

            let host = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(256, 256),
                        ..default()
                    },
                ))
                .id();

            // Missing fragment URI: must not crash the app or block the good panel.
            commands.spawn((
                UiPanel::new(MISSING_URI).mount_into(host, "Hud"),
                Health(1.0),
            ));
            // Valid fragment: must still bind despite its broken sibling.
            commands.spawn((
                UiPanel::new("good.xaml").mount_into(host, "Hud"),
                NoesisPanelText::new().watching(["GoodText"]),
                Health(42.0),
            ));
        },
    );

    let captured_sys = Arc::clone(&captured);
    app.add_systems(
        Update,
        move |mut reads: MessageReader<NoesisPanelTextChanged>| {
            for ev in reads.read() {
                captured_sys
                    .lock()
                    .unwrap()
                    .insert(ev.name.clone(), ev.text.clone());
            }
        },
    );

    // Event-driven exit: the good panel bound its text through the UI. Completing
    // without panic is itself part of the assertion.
    let pred_captured = Arc::clone(&captured);
    let bound = run_until(&mut app, 240, move |_app| {
        pred_captured
            .lock()
            .unwrap()
            .get("GoodText")
            .map(String::as_str)
            == Some("42")
    });

    let good = captured.lock().unwrap().clone();
    let live = app.world().resource::<NoesisDiagnostics>().live_panels;

    // The good panel bound despite its broken sibling.
    assert!(
        bound,
        "the valid panel's binding did not reach the UI within 240 frames; reads {good:?}",
    );
    // Only the good fragment built a PanelEntry; the broken one's build returned None.
    assert_eq!(
        live, 1,
        "expected exactly 1 live panel (the broken fragment must not mount), got {live}",
    );
    assert_eq!(
        good.get("GoodText").map(String::as_str),
        Some("42"),
        "the valid panel's binding did not reach the UI; reads {good:?}",
    );
}

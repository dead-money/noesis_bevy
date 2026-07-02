//! ECS-UI integration proof (keydown twin of `headless_panel_click`): a
//! [`NoesisKeyDownWatch`] on a mounted [`UiPanel`] entity resolves an `x:Name`
//! inside the panel's *own* fragment namescope (F4), and a [`NoesisFocus`] on the
//! same entity focuses that fragment-internal element (F6). A key injected into
//! the focused element fires a [`UiKeyDown`] targeting the panel entity, carrying
//! the host as its `view`. Two instances of the same fragment stay isolated: only
//! the focused panel's watch fires.
//!
//! One `#[test]` per file (thread-affine Noesis runtime, one app per process).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::{
    Key, KeyDownWatchEntry, NoesisCamera, NoesisFocus, NoesisKeyDownWatch, NoesisPanelAppExt,
    NoesisView, UiKeyDown, UiPanel, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

#[allow(dead_code)]
#[path = "../examples/ecs_ui.rs"]
mod ecs_ui;
use ecs_ui::Health;

const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Width="64" Height="32">
  <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="*"/></Grid.ColumnDefinitions>
  <Grid x:Name="SlotL" Grid.Column="0"/>
  <Grid x:Name="SlotR" Grid.Column="1"/>
</Grid>"##;

// Fragment: a focusable TextBox named in the fragment's OWN namescope. Both panels
// load this same XAML, so "PanelInput" exists twice, once per private namescope.
const FRAG_XAML: &str = r##"<TextBox xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      x:Name="PanelInput" HorizontalAlignment="Stretch" VerticalAlignment="Stretch"/>"##;

// Inject the key well after the fragments mount and focus lands.
const KEY_AT: usize = 40;

#[test]
fn keydown_watch_on_panel_entity_resolves_fragment_internal_name() {
    // (event_target, view, name, key) for every observed UiKeyDown.
    let observed: Arc<Mutex<Vec<(Entity, Entity, String, Key)>>> = Arc::new(Mutex::new(Vec::new()));
    // (view, p1, p2)
    let ids: Arc<Mutex<Option<(Entity, Entity, Entity)>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();
    // A registered bound field so each UiPanel actually builds + mounts.
    app.add_noesis_panel_field::<Health>();

    let obs = Arc::clone(&observed);
    app.add_observer(move |on: On<UiKeyDown>| {
        obs.lock()
            .unwrap()
            .push((on.event_target(), on.view, on.name.clone(), on.key));
    });

    let ids_startup = Arc::clone(&ids);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "host.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );
            reg.insert(
                "frag.xaml".to_string(),
                Arc::new(FRAG_XAML.as_bytes().to_vec()),
            );
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "host.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                ))
                .id();
            // p1: focused (F6) AND watched (F4) on its own fragment's "PanelInput".
            let p1 = commands
                .spawn((
                    UiPanel::new("frag.xaml").mount_into(view, "SlotL"),
                    Health(100.0),
                    NoesisFocus::new().focus("PanelInput"),
                    NoesisKeyDownWatch::new([KeyDownWatchEntry::new("PanelInput")]),
                ))
                .id();
            // p2: watched but NOT focused; its identically-named input must stay silent.
            let p2 = commands
                .spawn((
                    UiPanel::new("frag.xaml").mount_into(view, "SlotR"),
                    Health(100.0),
                    NoesisKeyDownWatch::new([KeyDownWatchEntry::new("PanelInput")]),
                ))
                .id();
            *ids_startup.lock().unwrap() = Some((view, p1, p2));
        },
    );

    app.add_systems(
        Update,
        move |mut frame: Local<usize>, mut input: ResMut<NoesisInputQueue>| {
            *frame += 1;
            if *frame == KEY_AT {
                // Routes to the focused element (p1's PanelInput).
                input.push(NoesisInputEvent::KeyDown(Key::Return));
            }
        },
    );

    // Exit as soon as p1's focused fragment input fires its UiKeyDown(Return).
    // Same-drive dispatch means if p2 has not fired by then, it never will.
    let pred_obs = Arc::clone(&observed);
    let pred_ids = Arc::clone(&ids);
    let fired =
        run_until(&mut app, 200, move |_app| {
            let Some((view, p1, _p2)) = *pred_ids.lock().unwrap() else {
                return false;
            };
            pred_obs.lock().unwrap().iter().any(|(t, v, n, k)| {
                *t == p1 && *v == view && n == "PanelInput" && *k == Key::Return
            })
        });

    let (view, p1, p2) = ids.lock().unwrap().expect("ids captured");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed UiKeyDown: {got:?}; view={view:?} p1={p1:?} p2={p2:?} ---");

    // The fragment-internal input fired: a UiKeyDown targeting its panel entity,
    // carrying the host view and the pressed key. Before F4/F6 a keydown watch +
    // focus on a panel entity were silently ignored (the panel isn't a `scene`).
    assert!(
        fired,
        "expected a UiKeyDown(Return) from p1's focused fragment input targeting p1 \
         with view {view:?} within 200 frames; observed {got:?}",
    );
    // Namescope isolation: only p1's input had focus, so p2's identically-named
    // input must not have fired.
    assert!(
        !got.iter().any(|(t, _, _, _)| *t == p2),
        "right panel p2 fired without focus (namescope cross-talk); observed {got:?}",
    );
}

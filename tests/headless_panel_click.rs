//! ECS-UI integration proof: a [`NoesisClickWatch`] placed on a mounted
//! [`UiPanel`] entity resolves `x:Name`s inside the panel's *own* fragment
//! namescope (a host-view `FindName` can't see them) and fires a [`UiClicked`]
//! that targets the panel entity, carrying the host as its `view`. Two instances
//! of the same fragment XAML stay isolated: clicking one panel's button never
//! fires the other's watch.
//!
//! This is the buttons-inside-fragments case the `ecs_ui` example sidesteps (it
//! only watches host-scene buttons and re-targets them at panel entities).
//!
//! One `#[test]` per file (thread-affine Noesis runtime, one app per process).

use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use noesis_bevy::input::{NoesisInputEvent, NoesisInputQueue};
use noesis_bevy::routed_events::MouseButton;
use noesis_bevy::{
    ClickWatchEntry, NoesisCamera, NoesisClickWatch, NoesisPanelAppExt, NoesisView, UiClicked,
    UiPanel, XamlRegistry,
};

mod common;
use common::{headless_app, run_until};

#[allow(dead_code)]
#[path = "../examples/ecs_ui.rs"]
mod ecs_ui;
use ecs_ui::Health;

// Host scene: two side-by-side full-bleed mount slots. SlotL covers x 0..32,
// SlotR covers x 32..64 (both full height), so a click in either half lands on
// that slot's mounted panel button.
const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Width="64" Height="32">
  <Grid.ColumnDefinitions><ColumnDefinition Width="*"/><ColumnDefinition Width="*"/></Grid.ColumnDefinitions>
  <Grid x:Name="SlotL" Grid.Column="0"/>
  <Grid x:Name="SlotR" Grid.Column="1"/>
</Grid>"##;

// Panel fragment: one full-bleed Button named in the fragment's OWN namescope.
// Both panels load this same XAML, so "PanelBtn" exists twice, once per private
// namescope: the case a root-level FindName can't disambiguate.
const FRAG_XAML: &str = r##"<Button xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml"
      x:Name="PanelBtn" HorizontalAlignment="Stretch" VerticalAlignment="Stretch" Content="X"/>"##;

// Panels mount + seal over the first frames; press once they are live, release
// two frames later. These are stimulus timings, not the exit condition.
const PRESS_AT: usize = 25;
const RELEASE_AT: usize = 27;

#[test]
fn click_watch_on_panel_entity_resolves_fragment_internal_name() {
    // (event_target, view, name) for every observed UiClicked.
    let observed: Arc<Mutex<Vec<(Entity, Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
    // (view, p1, p2)
    let ids: Arc<Mutex<Option<(Entity, Entity, Entity)>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();
    // A registered bound field so each UiPanel actually builds + mounts; the
    // fragment button doesn't bind it, it just needs to be hit-testable.
    app.add_noesis_panel_field::<Health>();

    let obs = Arc::clone(&observed);
    app.add_observer(move |on: On<UiClicked>| {
        obs.lock()
            .unwrap()
            .push((on.event_target(), on.view, on.name.clone()));
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
            // Two instances of the SAME fragment. Each watches "PanelBtn" on its
            // OWN panel entity (no explicit target, so default = the panel entity).
            let p1 = commands
                .spawn((
                    UiPanel::new("frag.xaml").mount_into(view, "SlotL"),
                    Health(100.0),
                    NoesisClickWatch::from_entries([ClickWatchEntry::new("PanelBtn")]),
                ))
                .id();
            let p2 = commands
                .spawn((
                    UiPanel::new("frag.xaml").mount_into(view, "SlotR"),
                    Health(100.0),
                    NoesisClickWatch::from_entries([ClickWatchEntry::new("PanelBtn")]),
                ))
                .id();
            *ids_startup.lock().unwrap() = Some((view, p1, p2));
        },
    );

    app.add_systems(
        Update,
        move |mut frame: Local<usize>, mut input: ResMut<NoesisInputQueue>| {
            *frame += 1;
            // Click the LEFT slot's button (center of the left half: x=16, y=16).
            if *frame == PRESS_AT {
                input.push(NoesisInputEvent::MouseMove { x: 16, y: 16 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 16,
                    y: 16,
                    button: MouseButton::Left,
                });
            }
            if *frame == RELEASE_AT {
                input.push(NoesisInputEvent::MouseButton {
                    down: false,
                    x: 16,
                    y: 16,
                    button: MouseButton::Left,
                });
            }
        },
    );

    // Exit as soon as the left panel's fragment-internal button has fired.
    let pred_obs = Arc::clone(&observed);
    let pred_ids = Arc::clone(&ids);
    let clicked = run_until(&mut app, 120, move |_app| {
        let Some((view, p1, _p2)) = *pred_ids.lock().unwrap() else {
            return false;
        };
        pred_obs
            .lock()
            .unwrap()
            .iter()
            .any(|(t, v, n)| *t == p1 && *v == view && n == "PanelBtn")
    });

    let (view, p1, p2) = ids.lock().unwrap().expect("ids captured");
    let got = observed.lock().unwrap().clone();
    eprintln!("--- observed UiClicked: {got:?}; view={view:?} p1={p1:?} p2={p2:?} ---");

    // The fragment-internal button fired: a UiClicked targeting its panel entity,
    // carrying the host view. Before the fix, the watch on a panel entity was
    // silently ignored (the panel isn't a `scene`), so nothing fired.
    assert!(
        clicked,
        "expected a UiClicked from the left panel's fragment button targeting p1 \
         with view {view:?}; observed {got:?}",
    );
    // Namescope isolation: the right panel's identically-named button was never
    // clicked (the same click event is dispatched against both watches in one
    // drive, so if p2 hasn't fired by the time p1 has, it never will).
    assert!(
        !got.iter().any(|(t, _, _)| *t == p2),
        "right panel p2 fired without being clicked (namescope cross-talk); observed {got:?}",
    );
}

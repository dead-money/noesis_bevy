//! ECS-UI integration proof, **Primitive 3 (events = observers), named half**: a
//! watched host `Button` fires a [`UiClicked`] re-targeted at a *panel entity* (via
//! [`ClickWatchEntry::target`]); an observer recovers it through `event_target()`
//! and heals only that panel. Proves the trigger target carries the entity an
//! observer needs, and that re-targeting routes to the right one of two panels.
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

use crate::common::{headless_app, run_until};

use crate::ecs_ui::{Health, Score};

// Host scene: a HUD mount slot plus two full-bleed buttons, each hit-testable at a
// known point: HealP1 fills the top half (y=8), HealP2 the bottom (y=24).
const HOST_XAML: &str = r##"<Grid xmlns="http://schemas.microsoft.com/winfx/2006/xaml/presentation"
      xmlns:x="http://schemas.microsoft.com/winfx/2006/xaml" Width="64" Height="32">
  <Grid.RowDefinitions><RowDefinition Height="*"/><RowDefinition Height="*"/></Grid.RowDefinitions>
  <StackPanel x:Name="Pad"/>
  <Button x:Name="HealP1" Grid.Row="0" Content="P1" HorizontalAlignment="Stretch" VerticalAlignment="Stretch"/>
  <Button x:Name="HealP2" Grid.Row="1" Content="P2" HorizontalAlignment="Stretch" VerticalAlignment="Stretch"/>
</Grid>"##;

/// The example's heal behaviour: a `UiClicked` from a heal button targets the
/// panel entity, recovered via `event_target()`.
fn heal_on_click(on: On<UiClicked>, mut huds: Query<&mut Health, With<UiPanel>>) {
    if let Ok(mut hp) = huds.get_mut(on.event_target()) {
        hp.0 = (hp.0 + 25.0).min(100.0);
    }
}

// Stimulus timings: press once the panels are live, release two frames later.
// The run's exit is the heal-observed predicate below.
const PRESS_AT: usize = 18;
const RELEASE_AT: usize = 20;

#[test]
fn named_button_event_retargets_to_panel() {
    let observed: Arc<Mutex<Vec<(Entity, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let panels: Arc<Mutex<Option<(Entity, Entity)>>> = Arc::new(Mutex::new(None));
    let final_hp: Arc<Mutex<Option<(f32, f32)>>> = Arc::new(Mutex::new(None));

    let mut app = headless_app();
    app.add_noesis_panel_field::<Health>()
        .add_noesis_panel_field::<Score>();

    // Capture every UiClicked, plus the example-style heal.
    let observed_obs = Arc::clone(&observed);
    app.add_observer(move |on: On<UiClicked>| {
        observed_obs
            .lock()
            .unwrap()
            .push((on.event_target(), on.name.clone()));
    });
    app.add_observer(heal_on_click);

    const HUD_SLOT: &str = "Pad";
    let panels_startup = Arc::clone(&panels);
    app.add_systems(
        Startup,
        move |mut commands: Commands, mut reg: ResMut<XamlRegistry>| {
            reg.insert(
                "pad.xaml".to_string(),
                Arc::new(HOST_XAML.as_bytes().to_vec()),
            );
            crate::ecs_ui::register_xaml(&mut reg);
            let view = commands
                .spawn((
                    Camera2d,
                    NoesisCamera,
                    NoesisView {
                        xaml_uri: "pad.xaml".to_string(),
                        size: UVec2::new(64, 32),
                        ..default()
                    },
                ))
                .id();
            // Two panels of the same component set; we assert their Health, not
            // their pixels, so both mount into the one slot.
            let p1 = commands
                .spawn((
                    UiPanel::new(crate::ecs_ui::HUD_URI).mount_into(view, HUD_SLOT),
                    Health(40.0),
                    Score(0),
                ))
                .id();
            let p2 = commands
                .spawn((
                    UiPanel::new(crate::ecs_ui::HUD_URI).mount_into(view, HUD_SLOT),
                    Health(40.0),
                    Score(0),
                ))
                .id();
            // Re-target each button's UiClicked at its panel entity (the crux: the
            // observer reads the panel straight off event_target()).
            commands
                .entity(view)
                .insert(NoesisClickWatch::from_entries([
                    ClickWatchEntry::new("HealP1").target(p1),
                    ClickWatchEntry::new("HealP2").target(p2),
                ]));
            *panels_startup.lock().unwrap() = Some((p1, p2));
        },
    );

    let panels_sys = Arc::clone(&panels);
    let final_sys = Arc::clone(&final_hp);
    app.add_systems(
        Update,
        move |mut frame: Local<usize>,
              mut input: ResMut<NoesisInputQueue>,
              healths: Query<&Health>| {
            *frame += 1;
            // Click HealP1 (top half: y=8).
            if *frame == PRESS_AT {
                input.push(NoesisInputEvent::MouseMove { x: 32, y: 8 });
                input.push(NoesisInputEvent::MouseButton {
                    down: true,
                    x: 32,
                    y: 8,
                    button: MouseButton::Left,
                });
            }
            if *frame == RELEASE_AT {
                input.push(NoesisInputEvent::MouseButton {
                    down: false,
                    x: 32,
                    y: 8,
                    button: MouseButton::Left,
                });
            }
            // Latest health per panel, refreshed every frame for the exit predicate.
            if let Some((p1, p2)) = *panels_sys.lock().unwrap() {
                if let (Ok(h1), Ok(h2)) = (healths.get(p1), healths.get(p2)) {
                    *final_sys.lock().unwrap() = Some((h1.0, h2.0));
                }
            }
        },
    );

    // Exit once the targeted panel has been healed by its button click (p1 40 ->
    // 65) with the other panel untouched.
    let pred_panels = Arc::clone(&panels);
    let pred_final = Arc::clone(&final_hp);
    let healed = run_until(&mut app, 240, move |_app| {
        let Some((_p1, _p2)) = *pred_panels.lock().unwrap() else {
            return false;
        };
        matches!(*pred_final.lock().unwrap(), Some((hp1, hp2)) if (hp1 - 65.0).abs() < 0.5 && (hp2 - 40.0).abs() < 0.5)
    });

    let (p1, _p2) = panels.lock().unwrap().expect("panels");
    let got = observed.lock().unwrap().clone();
    let (hp1, hp2) = final_hp.lock().unwrap().expect("final hp captured");
    eprintln!("--- observed UiClicked: {got:?}; final hp=({hp1},{hp2}) ---");

    assert!(
        healed,
        "panel 1 was never healed to 65 (with panel 2 untouched) within 240 \
         frames; observed {got:?}, final hp=({hp1},{hp2})",
    );

    // The UiClicked from "HealP1" must target panel 1's entity.
    assert!(
        got.iter().any(|(t, n)| *t == p1 && n == "HealP1"),
        "expected a UiClicked targeting panel 1 for button HealP1; observed {got:?}",
    );
    // The observer healed exactly the targeted panel: p1 40 -> 65, p2 untouched.
    assert!(
        (hp1 - 65.0).abs() < 0.5,
        "panel 1 was not healed by its targeted button click (hp1={hp1}); observed {got:?}",
    );
    assert!(
        (hp2 - 40.0).abs() < 0.5,
        "panel 2 was wrongly affected by panel 1's button (hp2={hp2})",
    );
}
